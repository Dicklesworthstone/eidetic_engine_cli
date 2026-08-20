//! CI/local verification orchestration and artifact policy (EE-TST-007).
//!
//! This module defines the verification pipeline that runs both locally and in CI,
//! ensuring consistent quality gates across development environments.
//!
//! # Verification Steps
//!
//! The verification pipeline runs these steps in order:
//! 1. `cargo fmt --check` - formatting consistency
//! 2. `cargo clippy --all-targets -- -D warnings` - lint checks
//! 3. `cargo test` - unit and integration tests
//! 4. Forbidden dependency audit - no tokio, rusqlite, petgraph, etc.
//!
//! # Artifact Policy
//!
//! Defines what gets generated, cached, and excluded from version control:
//! - `target/` - build artifacts (gitignored, cached in CI)
//! - `.ee/` - workspace state (user-specific, gitignored)
//! - `tests/fixtures/` - test fixtures (versioned)
//! - Golden test outputs regenerated on demand, versioned

use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use super::{build_info, duration_millis_saturating};
use crate::core::memory::{EvidenceFreshnessStatus, assess_memory_evidence_freshness};
use crate::curate::{CandidateSource, CandidateStatus, CandidateType};
use crate::db::{
    CreateAuditInput, CreateCurationCandidateInput, CreateWorkspaceInput, DbConnection,
    PROVENANCE_STATUS_MISMATCH, PROVENANCE_STATUS_MISSING, PROVENANCE_STATUS_SKIPPED,
    PROVENANCE_STATUS_VERIFIED, audit_actions, generate_audit_id,
};
use crate::models::{
    CandidateId, DomainError, LineSpan, ProducerMetadata, ProvenanceUri, RCH_VERIFY_SCHEMA_V1,
    RESPONSE_SCHEMA_V2, TrustClass, VERIFICATION_EVIDENCE_SCHEMA_V1, VerificationClosureGuidance,
    VerificationEvidenceRecord, VerificationGateRequirement, VerificationStatus,
    rch_cargo_closure_requirements, verification_closure_guidance,
    verification_evidence_beads_summary,
};

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for verification reports.
pub const VERIFY_REPORT_SCHEMA_V1: &str = "ee.verify.report.v1";
pub const VERIFY_RECORD_REPORT_SCHEMA_V1: &str = "ee.verify.record_report.v1";
pub const VERIFY_CLOSURE_GUIDANCE_REPORT_SCHEMA_V1: &str = "ee.verify.closure_guidance_report.v1";
pub const VERIFY_PROVENANCE_REPORT_SCHEMA_V1: &str = "ee.verify.provenance.v1";
pub const VERIFY_PROVENANCE_REFERENT_SCHEMA_V1: &str = "ee.verify.provenance_referent.v1";
pub const VERIFY_PROVENANCE_MUTATION_SCHEMA_V1: &str = "ee.verify.provenance_mutation.v1";
pub const VERIFICATION_LEDGER_ENTRY_SCHEMA_V1: &str = "ee.verification.ledger_entry.v1";
pub const VERIFICATION_POSTURE_SCHEMA_V1: &str = "ee.verification.posture.v1";
const LEGACY_VERIFICATION_RECORD_ACTION: &str = "verification.record";
const VERIFICATION_POSTURE_WINDOW_HOURS: u32 = 24;
const VERIFY_STEP_TIMEOUT: Duration = Duration::from_secs(60);
const VERIFY_STEP_OUTPUT_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const VERIFY_PROVENANCE_FILE_SCAN_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const VERIFY_PROVENANCE_GIT_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_VERIFY_PROVENANCE_LIMIT: u32 = 100;
pub const DEFAULT_VERIFY_PROVENANCE_STALE_AFTER_DAYS: u32 = 7;
const VERIFY_PROVENANCE_ACTOR: &str = "ee verify provenance";
const VERIFY_PROVENANCE_CANDIDATE_CONFIDENCE: f32 = 0.82;

/// Schema for artifact policy.
pub const ARTIFACT_POLICY_SCHEMA_V1: &str = "ee.artifact_policy.v1";

/// Redaction-safe verification evidence posture for status/doctor/support.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationPostureReport {
    pub schema: String,
    pub status: String,
    pub window_hours: u32,
    pub record_count: u32,
    pub recent_run_count: u32,
    pub stale_run_count: u32,
    pub unknown_age_count: u32,
    pub recent_reusable_run_count: u32,
    pub in_flight_equivalent_command_count: u32,
    pub advisory_counts: VerificationPostureAdvisoryCounts,
    pub evidence_health: VerificationPostureEvidenceHealth,
    pub recovery_actions: Vec<VerificationPostureRecoveryAction>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationPostureAdvisoryCounts {
    pub remote_success: u32,
    pub remote_failed: u32,
    pub remote_in_flight: u32,
    pub local_disallowed: u32,
    pub topology_blocked: u32,
    pub missing_artifact_manifest: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationPostureEvidenceHealth {
    pub ledger_available: bool,
    pub status: String,
    pub malformed_timestamp_count: u32,
    pub missing_artifact_manifest_count: u32,
    pub local_disallowed_count: u32,
    pub topology_blocked_count: u32,
    pub issue_count: u32,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationPostureRecoveryAction {
    pub priority: u8,
    pub kind: String,
    pub command: Option<String>,
    pub message: String,
    pub related_bead_id: Option<String>,
}

impl VerificationPostureReport {
    #[must_use]
    pub fn not_inspected() -> Self {
        Self::unavailable(
            "not_inspected",
            "workspace_not_selected",
            "Run `ee status --workspace . --json` from an initialized workspace.",
        )
    }

    #[must_use]
    pub fn unavailable(status: &str, reason: &str, repair_command: &str) -> Self {
        Self {
            schema: VERIFICATION_POSTURE_SCHEMA_V1.to_owned(),
            status: status.to_owned(),
            window_hours: VERIFICATION_POSTURE_WINDOW_HOURS,
            record_count: 0,
            recent_run_count: 0,
            stale_run_count: 0,
            unknown_age_count: 0,
            recent_reusable_run_count: 0,
            in_flight_equivalent_command_count: 0,
            advisory_counts: VerificationPostureAdvisoryCounts::default(),
            evidence_health: VerificationPostureEvidenceHealth {
                ledger_available: false,
                status: "unavailable".to_owned(),
                malformed_timestamp_count: 0,
                missing_artifact_manifest_count: 0,
                local_disallowed_count: 0,
                topology_blocked_count: 0,
                issue_count: 1,
                reason: Some(reason.to_owned()),
            },
            recovery_actions: vec![VerificationPostureRecoveryAction {
                priority: 1,
                kind: "initialize_or_inspect_ledger".to_owned(),
                command: Some(repair_command.to_owned()),
                message: "Verification evidence posture could not inspect the workspace ledger."
                    .to_owned(),
                related_bead_id: None,
            }],
        }
    }

    #[must_use]
    pub fn from_records(now: DateTime<Utc>, records: &[VerificationEvidenceRecord]) -> Self {
        let mut report = Self {
            schema: VERIFICATION_POSTURE_SCHEMA_V1.to_owned(),
            status: String::new(),
            window_hours: VERIFICATION_POSTURE_WINDOW_HOURS,
            record_count: saturating_len_u32(records.len()),
            recent_run_count: 0,
            stale_run_count: 0,
            unknown_age_count: 0,
            recent_reusable_run_count: 0,
            in_flight_equivalent_command_count: 0,
            advisory_counts: VerificationPostureAdvisoryCounts::default(),
            evidence_health: VerificationPostureEvidenceHealth {
                ledger_available: true,
                status: String::new(),
                malformed_timestamp_count: 0,
                missing_artifact_manifest_count: 0,
                local_disallowed_count: 0,
                topology_blocked_count: 0,
                issue_count: 0,
                reason: None,
            },
            recovery_actions: Vec::new(),
        };

        for record in records {
            match verification_record_age_bucket(now, record) {
                VerificationAgeBucket::Recent => report.recent_run_count += 1,
                VerificationAgeBucket::Stale => report.stale_run_count += 1,
                VerificationAgeBucket::Unknown => report.unknown_age_count += 1,
                VerificationAgeBucket::Malformed => {
                    report.unknown_age_count += 1;
                    report.evidence_health.malformed_timestamp_count += 1;
                }
            }

            let remote_required = verification_remote_required(record);
            let local_disallowed = remote_required
                && (record.offload.fallback_detected
                    || record.status == VerificationStatus::FallbackDetected);
            let missing_manifest =
                remote_required && !verification_record_has_artifact_manifest(record);
            let topology_blocked = remote_required
                && record.status == VerificationStatus::Blocked
                && verification_record_mentions_rch_topology(record);
            let in_flight = remote_required
                && (record.status == VerificationStatus::Interrupted
                    || record.finished_at.as_deref().is_none());

            if local_disallowed {
                report.advisory_counts.local_disallowed += 1;
                report.evidence_health.local_disallowed_count += 1;
            } else if topology_blocked {
                report.advisory_counts.topology_blocked += 1;
                report.evidence_health.topology_blocked_count += 1;
            } else if in_flight {
                report.advisory_counts.remote_in_flight += 1;
                report.in_flight_equivalent_command_count += 1;
            } else if remote_required && record.status == VerificationStatus::Passed {
                report.advisory_counts.remote_success += 1;
                if matches!(
                    verification_record_age_bucket(now, record),
                    VerificationAgeBucket::Recent
                ) && !missing_manifest
                {
                    report.recent_reusable_run_count += 1;
                }
            } else if remote_required && record.status == VerificationStatus::Failed {
                report.advisory_counts.remote_failed += 1;
            }

            if missing_manifest {
                report.advisory_counts.missing_artifact_manifest += 1;
                report.evidence_health.missing_artifact_manifest_count += 1;
            }
        }

        report.evidence_health.issue_count = report.evidence_health.malformed_timestamp_count
            + report.evidence_health.missing_artifact_manifest_count
            + report.evidence_health.local_disallowed_count
            + report.evidence_health.topology_blocked_count;
        report.evidence_health.status = verification_evidence_health_status(&report).to_owned();
        report.evidence_health.reason =
            verification_evidence_health_reason(&report).map(str::to_owned);
        report.status = verification_posture_status(&report).to_owned();
        report.recovery_actions = verification_posture_recovery_actions(&report);
        report
    }
}

// ============================================================================
// Verification Steps
// ============================================================================

/// A verification step in the pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyStep {
    Format,
    Clippy,
    Test,
    ForbiddenDeps,
}

impl VerifyStep {
    /// All verification steps in execution order.
    pub const ALL: &'static [Self] = &[Self::Format, Self::Clippy, Self::Test, Self::ForbiddenDeps];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Clippy => "clippy",
            Self::Test => "test",
            Self::ForbiddenDeps => "forbidden_deps",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Format => "Check code formatting with cargo fmt",
            Self::Clippy => "Run clippy lints with warnings as errors",
            Self::Test => "Run unit and integration tests",
            Self::ForbiddenDeps => "Audit for forbidden dependencies",
        }
    }

    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Self::Format => "cargo fmt --check",
            Self::Clippy => "cargo clippy --all-targets -- -D warnings",
            Self::Test => "cargo test",
            Self::ForbiddenDeps => "cargo test forbidden_deps",
        }
    }

    #[must_use]
    pub const fn is_required(self) -> bool {
        true
    }
}

impl fmt::Display for VerifyStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Step Result
// ============================================================================

/// Result of running a verification step.
#[derive(Clone, Debug)]
pub struct StepResult {
    pub step: VerifyStep,
    pub passed: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub skipped: bool,
    pub skip_reason: Option<String>,
}

impl StepResult {
    fn passed(step: VerifyStep, duration: Duration, stdout: String, stderr: String) -> Self {
        Self {
            step,
            passed: true,
            duration_ms: duration_millis_saturating(duration),
            stdout,
            stderr,
            exit_code: Some(0),
            skipped: false,
            skip_reason: None,
        }
    }

    fn failed(
        step: VerifyStep,
        duration: Duration,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            step,
            passed: false,
            duration_ms: duration_millis_saturating(duration),
            stdout,
            stderr,
            exit_code,
            skipped: false,
            skip_reason: None,
        }
    }

    fn skipped(step: VerifyStep, reason: &str) -> Self {
        Self {
            step,
            passed: true,
            duration_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            skipped: true,
            skip_reason: Some(reason.to_string()),
        }
    }
}

// ============================================================================
// Verification Report
// ============================================================================

/// Complete verification report.
#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub version: &'static str,
    pub workspace_path: String,
    pub all_passed: bool,
    pub total_duration_ms: u64,
    pub steps: Vec<StepResult>,
    pub failed_count: usize,
    pub passed_count: usize,
    pub skipped_count: usize,
}

impl VerifyReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("Verification Report\n");
        out.push_str("===================\n\n");
        out.push_str(&format!("Workspace: {}\n", self.workspace_path));
        out.push_str(&format!("Duration: {}ms\n\n", self.total_duration_ms));

        for result in &self.steps {
            let status = if result.skipped {
                "SKIP"
            } else if result.passed {
                "PASS"
            } else {
                "FAIL"
            };
            out.push_str(&format!(
                "[{}] {} ({}ms)\n",
                status,
                result.step.as_str(),
                result.duration_ms
            ));
            if !result.passed && !result.stderr.is_empty() {
                let preview: String = result.stderr.lines().take(5).collect::<Vec<_>>().join("\n");
                out.push_str(&format!("    {}\n", preview.replace('\n', "\n    ")));
            }
        }

        out.push_str(&format!(
            "\nSummary: {} passed, {} failed, {} skipped\n",
            self.passed_count, self.failed_count, self.skipped_count
        ));

        if self.all_passed {
            out.push_str("Result: PASSED\n");
        } else {
            out.push_str("Result: FAILED\n");
        }

        out
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        let status = if self.all_passed { "PASS" } else { "FAIL" };
        format!(
            "VERIFY|{}|{}|{}|{}ms",
            status, self.passed_count, self.failed_count, self.total_duration_ms
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "step": s.step.as_str(),
                    "passed": s.passed,
                    "durationMs": s.duration_ms,
                    "exitCode": s.exit_code,
                    "skipped": s.skipped,
                    "skipReason": s.skip_reason,
                })
            })
            .collect();

        serde_json::json!({
            "command": "verify run",
            "version": self.version,
            "schema": VERIFY_REPORT_SCHEMA_V1,
            "workspacePath": self.workspace_path,
            "allPassed": self.all_passed,
            "totalDurationMs": self.total_duration_ms,
            "passedCount": self.passed_count,
            "failedCount": self.failed_count,
            "skippedCount": self.skipped_count,
            "steps": steps,
        })
    }
}

// ============================================================================
// Artifact Policy
// ============================================================================

/// Artifact category for policy rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactCategory {
    BuildOutput,
    TestFixture,
    WorkspaceState,
    GoldenOutput,
    CacheDirectory,
    GeneratedCode,
}

impl ArtifactCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuildOutput => "build_output",
            Self::TestFixture => "test_fixture",
            Self::WorkspaceState => "workspace_state",
            Self::GoldenOutput => "golden_output",
            Self::CacheDirectory => "cache_directory",
            Self::GeneratedCode => "generated_code",
        }
    }
}

/// Policy rule for an artifact pattern.
#[derive(Clone, Debug)]
pub struct ArtifactRule {
    pub pattern: &'static str,
    pub category: ArtifactCategory,
    pub versioned: bool,
    pub ci_cached: bool,
    pub description: &'static str,
}

/// Standard artifact policy for ee workspaces.
pub const ARTIFACT_RULES: &[ArtifactRule] = &[
    ArtifactRule {
        pattern: "target/",
        category: ArtifactCategory::BuildOutput,
        versioned: false,
        ci_cached: true,
        description: "Cargo build artifacts",
    },
    ArtifactRule {
        pattern: ".ee/",
        category: ArtifactCategory::WorkspaceState,
        versioned: false,
        ci_cached: false,
        description: "User workspace state (database, indexes)",
    },
    ArtifactRule {
        pattern: "tests/fixtures/",
        category: ArtifactCategory::TestFixture,
        versioned: true,
        ci_cached: false,
        description: "Deterministic test fixtures",
    },
    ArtifactRule {
        pattern: "tests/fixtures/golden/",
        category: ArtifactCategory::GoldenOutput,
        versioned: true,
        ci_cached: false,
        description: "Golden test expected outputs",
    },
    ArtifactRule {
        pattern: "Cargo.lock",
        category: ArtifactCategory::GeneratedCode,
        versioned: true,
        ci_cached: false,
        description: "Locked dependency versions",
    },
    ArtifactRule {
        pattern: ".rch-target/",
        category: ArtifactCategory::CacheDirectory,
        versioned: false,
        ci_cached: false,
        description: "Remote compilation helper cache",
    },
];

/// Get artifact policy report.
#[must_use]
pub fn artifact_policy_report() -> ArtifactPolicyReport {
    ArtifactPolicyReport {
        version: build_info().version,
        rules: ARTIFACT_RULES.to_vec(),
    }
}

/// Artifact policy report.
#[derive(Clone, Debug)]
pub struct ArtifactPolicyReport {
    pub version: &'static str,
    pub rules: Vec<ArtifactRule>,
}

impl ArtifactPolicyReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("Artifact Policy\n");
        out.push_str("===============\n\n");

        for rule in &self.rules {
            let versioned = if rule.versioned {
                "versioned"
            } else {
                "gitignored"
            };
            let cached = if rule.ci_cached { ", CI cached" } else { "" };
            out.push_str(&format!(
                "{} ({}{}) - {}\n",
                rule.pattern, versioned, cached, rule.description
            ));
        }

        out
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let rules: Vec<serde_json::Value> = self
            .rules
            .iter()
            .map(|r| {
                serde_json::json!({
                    "pattern": r.pattern,
                    "category": r.category.as_str(),
                    "versioned": r.versioned,
                    "ciCached": r.ci_cached,
                    "description": r.description,
                })
            })
            .collect();

        serde_json::json!({
            "command": "artifact-policy",
            "version": self.version,
            "rules": rules,
        })
    }
}

// ============================================================================
// Verification Options
// ============================================================================

/// Options for running verification.
#[derive(Clone, Debug, Default)]
pub struct VerifyOptions {
    pub workspace_path: Option<String>,
    pub steps: Option<Vec<VerifyStep>>,
    pub fail_fast: bool,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
pub struct VerificationRecordOptions<'a> {
    pub database_path: &'a Path,
    pub workspace_path: &'a Path,
    pub target_type: &'a str,
    pub target_id: &'a str,
    pub actor: Option<&'a str>,
    pub evidence: VerificationEvidenceRecord,
}

/// Trust boundary established by the parser that produced normalized evidence.
///
/// The generic `ee.verification_evidence.v1` envelope is useful as advisory
/// evidence, but all of its authority-bearing fields are caller controlled. A
/// specialized parser may establish stronger provenance after validating the
/// source artifact's own contract. This value is stored beside, rather than
/// inside, the caller-provided evidence so an embedded producer block cannot
/// grant itself closure authority.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VerificationEvidenceAuthority {
    #[default]
    CallerAuthored,
    ValidatedRunRecord,
    ValidatedRchVerify,
    ValidatedGithubActions,
}

impl VerificationEvidenceAuthority {
    pub(crate) const fn can_authorize_pass(self) -> bool {
        !matches!(self, Self::CallerAuthored)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::CallerAuthored => "caller_authored",
            Self::ValidatedRunRecord => "validated_run_record",
            Self::ValidatedRchVerify => "validated_rch_verify",
            Self::ValidatedGithubActions => "validated_github_actions",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationRecordReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub audit_id: String,
    pub content_hash: String,
    pub workspace_id: String,
    pub target_type: String,
    pub target_id: String,
    pub persisted: bool,
    pub replayed: bool,
    pub authority: &'static str,
    pub pass_authority_validated: bool,
    pub degradations: Vec<String>,
    pub evidence: VerificationEvidenceRecord,
}

impl VerificationRecordReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let verb = if self.replayed {
            "verification evidence already recorded"
        } else {
            "verification evidence recorded"
        };
        format!(
            "{verb}\n  ID: {}\n  Audit: {}\n  Content hash: {}\n  Target: {}:{}\n  Status: {}\n  Authority: {}\n  Pass authority validated: {}\n  Beads summary: {}\n",
            self.evidence.verification_id,
            self.audit_id,
            self.content_hash,
            self.target_type,
            self.target_id,
            self.evidence.status.as_str(),
            self.authority,
            self.pass_authority_validated,
            verification_evidence_beads_summary(&self.evidence)
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": self.command,
            "version": self.version,
            "schema": self.schema,
            "auditId": self.audit_id,
            "contentHash": self.content_hash,
            "workspaceId": self.workspace_id,
            "targetType": self.target_type,
            "targetId": self.target_id,
            "persisted": self.persisted,
            "replayed": self.replayed,
            "authority": self.authority,
            "passAuthorityValidated": self.pass_authority_validated,
            "degradations": self.degradations,
            "beadsSummary": verification_evidence_beads_summary(&self.evidence),
            "verificationEvidence": self.evidence,
        })
    }
}

#[derive(Clone)]
pub struct VerifyProvenanceReferentOptions<'a> {
    pub workspace_path: &'a Path,
    pub database: Option<&'a DbConnection>,
    pub allow_network: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyProvenanceReferentStatus {
    Verified,
    EvidenceMissing,
    EvidenceDrift,
    Unverifiable,
}

impl VerifyProvenanceReferentStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::EvidenceMissing => "evidence_missing",
            Self::EvidenceDrift => "evidence_drift",
            Self::Unverifiable => "unverifiable",
        }
    }

    /// The maintenance action `ee verify provenance` proposes for a referent
    /// (bd-1n0np.9.2). See [`ProvenanceReverifyAction`].
    #[must_use]
    pub const fn reverify_action(self) -> ProvenanceReverifyAction {
        ProvenanceReverifyAction::for_status(self)
    }
}

/// What `ee verify provenance` proposes when a cited evidence referent is
/// re-resolved (bd-1n0np.9.2).
///
/// Enforces two invariants: ee NEVER removes a memory (RULE 1 / no silent
/// mutation) — it demotes (audited) and raises a `revalidate` curation
/// candidate whose reason requests evidence revalidation; and an `Unverifiable`
/// referent (e.g. cass down, network-gated) is advisory ONLY — never demoted,
/// mirroring "cass missing -> unverifiable, not missing".
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProvenanceReverifyAction {
    /// Referent re-verified — no action.
    None,
    /// Referent could not be checked — advisory only: no demotion, no candidate.
    Advisory,
    /// Referent is gone or drifted — write an audited demotion (trust-class /
    /// freshness transition) and raise a review candidate requesting evidence
    /// revalidation. The memory is never removed.
    DemoteAndRevalidate,
}

impl ProvenanceReverifyAction {
    /// Map a referent status to the proposed action.
    #[must_use]
    pub const fn for_status(status: VerifyProvenanceReferentStatus) -> Self {
        match status {
            VerifyProvenanceReferentStatus::Verified => Self::None,
            VerifyProvenanceReferentStatus::Unverifiable => Self::Advisory,
            VerifyProvenanceReferentStatus::EvidenceMissing
            | VerifyProvenanceReferentStatus::EvidenceDrift => Self::DemoteAndRevalidate,
        }
    }

    /// Whether this action writes an audited demotion + revalidation candidate.
    #[must_use]
    pub const fn demotes(self) -> bool {
        matches!(self, Self::DemoteAndRevalidate)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Advisory => "advisory",
            Self::DemoteAndRevalidate => "demote_and_revalidate",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyProvenanceReferentReport {
    pub schema: &'static str,
    pub uri: String,
    pub scheme: String,
    pub status: VerifyProvenanceReferentStatus,
    pub reason: String,
    pub referent_hash: Option<String>,
    pub repair: Option<String>,
}

impl VerifyProvenanceReferentReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "uri": self.uri,
            "scheme": self.scheme,
            "status": self.status.as_str(),
            "reason": self.reason,
            "referentHash": self.referent_hash,
            "action": self.status.reverify_action().as_str(),
            "repair": self.repair,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyProvenanceMutationReport {
    pub schema: &'static str,
    pub memory_id: String,
    pub status: VerifyProvenanceReferentStatus,
    pub action: ProvenanceReverifyAction,
    pub persisted: bool,
    pub previous_verification_status: String,
    pub new_verification_status: String,
    pub previous_verified_at: Option<String>,
    pub new_verified_at: Option<String>,
    pub verification_status_updated: bool,
    pub previous_trust_class: String,
    pub new_trust_class: String,
    pub trust_class_updated: bool,
    pub trust_audit_id: Option<String>,
    pub candidate_id: Option<String>,
    pub candidate_type: Option<String>,
    pub candidate_status: Option<String>,
    pub candidate_audit_id: Option<String>,
    pub reason: String,
}

impl VerifyProvenanceMutationReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "memoryId": self.memory_id,
            "status": self.status.as_str(),
            "action": self.action.as_str(),
            "persisted": self.persisted,
            "previousVerificationStatus": self.previous_verification_status,
            "newVerificationStatus": self.new_verification_status,
            "previousVerifiedAt": self.previous_verified_at,
            "newVerifiedAt": self.new_verified_at,
            "verificationStatusUpdated": self.verification_status_updated,
            "previousTrustClass": self.previous_trust_class,
            "newTrustClass": self.new_trust_class,
            "trustClassUpdated": self.trust_class_updated,
            "trustAuditId": self.trust_audit_id,
            "candidateId": self.candidate_id,
            "candidateType": self.candidate_type,
            "candidateStatus": self.candidate_status,
            "candidateAuditId": self.candidate_audit_id,
            "reason": self.reason,
        })
    }
}

#[derive(Clone)]
pub struct VerifyProvenanceOptions<'a> {
    pub workspace_path: &'a Path,
    pub database: &'a DbConnection,
    pub workspace_id: &'a str,
    pub memory_id: Option<&'a str>,
    pub stale_after_days: u32,
    pub limit: u32,
    pub allow_network: bool,
    pub dry_run: bool,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyProvenanceMemoryReport {
    pub memory_id: String,
    pub provenance_uri: String,
    pub previous_status: String,
    pub previous_verified_at: Option<String>,
    pub checked_at: String,
    pub referent: VerifyProvenanceReferentReport,
    pub mutation: Option<VerifyProvenanceMutationReport>,
}

impl VerifyProvenanceMemoryReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "memoryId": self.memory_id,
            "provenanceUri": self.provenance_uri,
            "previousStatus": self.previous_status,
            "previousVerifiedAt": self.previous_verified_at,
            "checkedAt": self.checked_at,
            "referent": self.referent.data_json(),
            "mutation": self.mutation.as_ref().map(VerifyProvenanceMutationReport::data_json),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyProvenanceReport {
    pub schema: &'static str,
    pub workspace_id: String,
    pub requested_memory_id: Option<String>,
    pub checked_at: String,
    pub stale_after_days: u32,
    pub limit: u32,
    pub inspected_count: u32,
    pub no_provenance_count: u32,
    pub due_count: u32,
    pub checked_count: u32,
    pub skipped_recent_count: u32,
    pub bounded_skipped_count: u32,
    pub verified_count: u32,
    pub evidence_missing_count: u32,
    pub evidence_drift_count: u32,
    pub unverifiable_count: u32,
    pub dry_run: bool,
    pub mutation_count: u32,
    pub trust_demotion_count: u32,
    pub curation_candidate_count: u32,
    pub audit_count: u32,
    pub records: Vec<VerifyProvenanceMemoryReport>,
}

impl VerifyProvenanceReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let records = self
            .records
            .iter()
            .map(VerifyProvenanceMemoryReport::data_json)
            .collect::<Vec<_>>();
        let referents = self
            .records
            .iter()
            .map(|record| {
                let mut referent = record.referent.data_json();
                if let Some(object) = referent.as_object_mut() {
                    object.insert(
                        "memoryId".to_owned(),
                        serde_json::Value::String(record.memory_id.clone()),
                    );
                    object.insert(
                        "provenanceUri".to_owned(),
                        serde_json::Value::String(record.provenance_uri.clone()),
                    );
                    object.insert(
                        "previousStatus".to_owned(),
                        serde_json::Value::String(record.previous_status.clone()),
                    );
                    object.insert(
                        "previousVerifiedAt".to_owned(),
                        record
                            .previous_verified_at
                            .clone()
                            .map_or(serde_json::Value::Null, serde_json::Value::String),
                    );
                    object.insert(
                        "checkedAt".to_owned(),
                        serde_json::Value::String(record.checked_at.clone()),
                    );
                    object.insert(
                        "mutation".to_owned(),
                        record
                            .mutation
                            .as_ref()
                            .map(VerifyProvenanceMutationReport::data_json)
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
                referent
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "schema": self.schema,
            "workspaceId": self.workspace_id,
            "requestedMemoryId": self.requested_memory_id,
            "checkedAt": self.checked_at,
            "staleAfterDays": self.stale_after_days,
            "limit": self.limit,
            "inspectedCount": self.inspected_count,
            "noProvenanceCount": self.no_provenance_count,
            "dueCount": self.due_count,
            "checkedCount": self.checked_count,
            "skippedRecentCount": self.skipped_recent_count,
            "boundedSkippedCount": self.bounded_skipped_count,
            "verifiedCount": self.verified_count,
            "evidenceMissingCount": self.evidence_missing_count,
            "evidenceDriftCount": self.evidence_drift_count,
            "unverifiableCount": self.unverifiable_count,
            "dryRun": self.dry_run,
            "mutationCount": self.mutation_count,
            "trustDemotionCount": self.trust_demotion_count,
            "curationCandidateCount": self.curation_candidate_count,
            "auditCount": self.audit_count,
            "referents": referents,
            "records": records,
        })
    }

    fn push(&mut self, record: VerifyProvenanceMemoryReport) {
        self.checked_count = self.checked_count.saturating_add(1);
        match record.referent.status {
            VerifyProvenanceReferentStatus::Verified => {
                self.verified_count = self.verified_count.saturating_add(1);
            }
            VerifyProvenanceReferentStatus::EvidenceMissing => {
                self.evidence_missing_count = self.evidence_missing_count.saturating_add(1);
            }
            VerifyProvenanceReferentStatus::EvidenceDrift => {
                self.evidence_drift_count = self.evidence_drift_count.saturating_add(1);
            }
            VerifyProvenanceReferentStatus::Unverifiable => {
                self.unverifiable_count = self.unverifiable_count.saturating_add(1);
            }
        }
        if let Some(mutation) = &record.mutation {
            self.mutation_count = self.mutation_count.saturating_add(1);
            if mutation.trust_class_updated {
                self.trust_demotion_count = self.trust_demotion_count.saturating_add(1);
            }
            if mutation.candidate_audit_id.is_some() {
                self.curation_candidate_count = self.curation_candidate_count.saturating_add(1);
            }
            if mutation.trust_audit_id.is_some() {
                self.audit_count = self.audit_count.saturating_add(1);
            }
            if mutation.candidate_audit_id.is_some() {
                self.audit_count = self.audit_count.saturating_add(1);
            }
        }
        self.records.push(record);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationClosureGuidanceReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub bead_id: Option<String>,
    pub evidence_count: usize,
    pub guidance: VerificationClosureGuidance,
}

impl VerificationClosureGuidanceReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!(
            "verification closure guidance\n  Bead: {}\n  Evidence records: {}\n  Can close: {}\n",
            self.bead_id.as_deref().unwrap_or("none"),
            self.evidence_count,
            if self.guidance.can_close { "yes" } else { "no" }
        );
        for reason in &self.guidance.rejected_reasons {
            output.push_str(&format!("  Reject: {reason}\n"));
        }
        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": self.command,
            "version": self.version,
            "schema": self.schema,
            "beadId": self.bead_id,
            "evidenceCount": self.evidence_count,
            "guidance": self.guidance,
        })
    }
}

#[derive(Clone, Debug)]
pub struct VerificationClosureGuidanceOptions<'a> {
    pub database_path: &'a Path,
    pub bead_id: Option<&'a str>,
    pub requirements: Vec<VerificationGateRequirement>,
}

pub fn verify_bounded_provenance(
    options: VerifyProvenanceOptions<'_>,
) -> Result<VerifyProvenanceReport, DomainError> {
    let memories = match options.memory_id {
        Some(memory_id) => match options.database.get_memory(memory_id) {
            Ok(Some(memory)) => vec![memory],
            Ok(None) => {
                return Err(DomainError::Usage {
                    message: format!("Memory {memory_id} was not found."),
                    repair: Some("Pass an existing memory ID or omit --memory-id.".to_owned()),
                });
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!("Failed to load memory {memory_id}: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                });
            }
        },
        None => options
            .database
            .list_memories(options.workspace_id, None, false)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to list memories for provenance verification: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?,
    };
    let checked_at = options.now.to_rfc3339();
    let mut report = VerifyProvenanceReport {
        schema: VERIFY_PROVENANCE_REPORT_SCHEMA_V1,
        workspace_id: options.workspace_id.to_owned(),
        requested_memory_id: options.memory_id.map(str::to_owned),
        checked_at: checked_at.clone(),
        stale_after_days: options.stale_after_days,
        limit: options.limit,
        inspected_count: 0,
        no_provenance_count: 0,
        due_count: 0,
        checked_count: 0,
        skipped_recent_count: 0,
        bounded_skipped_count: 0,
        verified_count: 0,
        evidence_missing_count: 0,
        evidence_drift_count: 0,
        unverifiable_count: 0,
        dry_run: options.dry_run,
        mutation_count: 0,
        trust_demotion_count: 0,
        curation_candidate_count: 0,
        audit_count: 0,
        records: Vec::new(),
    };
    let referent_options = VerifyProvenanceReferentOptions {
        workspace_path: options.workspace_path,
        database: Some(options.database),
        allow_network: options.allow_network,
    };

    for memory in memories {
        report.inspected_count = report.inspected_count.saturating_add(1);
        let Some(provenance_uri) = memory.provenance_uri.clone() else {
            report.no_provenance_count = report.no_provenance_count.saturating_add(1);
            continue;
        };
        if options.memory_id.is_none()
            && !provenance_memory_is_due(&memory, options.stale_after_days, options.now)
        {
            report.skipped_recent_count = report.skipped_recent_count.saturating_add(1);
            continue;
        }
        report.due_count = report.due_count.saturating_add(1);
        if report.checked_count >= options.limit {
            report.bounded_skipped_count = report.bounded_skipped_count.saturating_add(1);
            continue;
        }
        let referent = verify_memory_provenance_referent(
            &memory,
            options.workspace_path,
            verify_provenance_referent(&provenance_uri, &referent_options),
        );
        let mutation = apply_provenance_reverify_action(
            options.database,
            options.workspace_id,
            &memory,
            &referent,
            &checked_at,
            options.dry_run,
        )?;
        report.push(VerifyProvenanceMemoryReport {
            memory_id: memory.id,
            provenance_uri,
            previous_status: memory.provenance_verification_status,
            previous_verified_at: memory.provenance_verified_at,
            checked_at: checked_at.clone(),
            referent,
            mutation,
        });
    }

    Ok(report)
}

fn apply_provenance_reverify_action(
    database: &DbConnection,
    workspace_id: &str,
    memory: &crate::db::StoredMemory,
    referent: &VerifyProvenanceReferentReport,
    checked_at: &str,
    dry_run: bool,
) -> Result<Option<VerifyProvenanceMutationReport>, DomainError> {
    let action = referent.status.reverify_action();
    if dry_run {
        if !action.demotes() {
            return Ok(None);
        }
        let Some(current_memory) =
            database
                .get_memory(&memory.id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to inspect memory before provenance dry run: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                })?
        else {
            return Ok(None);
        };
        if current_memory.tombstoned_at.is_some() {
            return Ok(None);
        }

        let candidate_id = provenance_reverify_candidate_id(workspace_id, &memory.id, referent);
        let previous_trust_class = current_memory.trust_class.clone();
        let new_trust_class = provenance_reverify_demoted_trust_class(&previous_trust_class)
            .unwrap_or(&previous_trust_class)
            .to_owned();
        let reason = provenance_reverify_reason(&current_memory, referent);
        let new_verification_status = provenance_reverify_memory_status(referent.status).to_owned();
        return Ok(Some(VerifyProvenanceMutationReport {
            schema: VERIFY_PROVENANCE_MUTATION_SCHEMA_V1,
            memory_id: current_memory.id.clone(),
            status: referent.status,
            action,
            persisted: false,
            previous_verification_status: current_memory.provenance_verification_status.clone(),
            new_verification_status,
            previous_verified_at: current_memory.provenance_verified_at.clone(),
            new_verified_at: Some(checked_at.to_owned()),
            verification_status_updated: false,
            previous_trust_class,
            new_trust_class,
            trust_class_updated: false,
            trust_audit_id: None,
            candidate_id: Some(candidate_id),
            candidate_type: Some(CandidateType::Deprecate.as_str().to_owned()),
            candidate_status: Some("planned".to_owned()),
            candidate_audit_id: None,
            reason,
        }));
    }

    database
        .with_transaction(|| {
            let Some(current_memory) = database.get_memory(&memory.id)? else {
                return Ok(None);
            };
            if current_memory.tombstoned_at.is_some() {
                return Ok(None);
            }

            let previous_verification_status =
                current_memory.provenance_verification_status.clone();
            let new_verification_status =
                provenance_reverify_memory_status(referent.status).to_owned();
            let previous_verified_at = current_memory.provenance_verified_at.clone();
            let new_verified_at = Some(checked_at.to_owned());
            let previous_trust_class = current_memory.trust_class.clone();
            let mut new_trust_class = previous_trust_class.clone();
            let reason = provenance_reverify_reason(&current_memory, referent);
            let verification_note = provenance_reverify_verification_note(referent);
            let mut trust_class_updated = false;
            let candidate_id = provenance_reverify_candidate_id(workspace_id, &memory.id, referent);
            let trust_audit_id = if action.demotes() {
                new_trust_class = provenance_reverify_demoted_trust_class(&previous_trust_class)
                    .unwrap_or(&previous_trust_class)
                    .to_owned();
                if previous_trust_class != new_trust_class {
                    trust_class_updated =
                        database.update_memory_trust_class(&current_memory.id, &new_trust_class)?;
                    if trust_class_updated {
                        let trust_audit_id = generate_audit_id();
                        database.insert_audit(
                            &trust_audit_id,
                            &CreateAuditInput {
                                workspace_id: Some(workspace_id.to_owned()),
                                actor: Some(VERIFY_PROVENANCE_ACTOR.to_owned()),
                                action: audit_actions::TRUST_CLASS_TRANSITION.to_owned(),
                                target_type: Some("memory".to_owned()),
                                target_id: Some(current_memory.id.clone()),
                                details: Some(provenance_reverify_trust_audit_details(
                                    &current_memory,
                                    referent,
                                    checked_at,
                                    &previous_trust_class,
                                    &new_trust_class,
                                    &candidate_id,
                                )),
                            },
                        )?;
                        Some(trust_audit_id)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let verification_status_updated = database.update_memory_provenance_verification(
                &current_memory.id,
                &new_verification_status,
                checked_at,
                &verification_note,
            )?;

            if !action.demotes() {
                return Ok(Some(VerifyProvenanceMutationReport {
                    schema: VERIFY_PROVENANCE_MUTATION_SCHEMA_V1,
                    memory_id: current_memory.id.clone(),
                    status: referent.status,
                    action,
                    persisted: verification_status_updated,
                    previous_verification_status,
                    new_verification_status,
                    previous_verified_at,
                    new_verified_at,
                    verification_status_updated,
                    previous_trust_class: previous_trust_class.clone(),
                    new_trust_class: previous_trust_class,
                    trust_class_updated: false,
                    trust_audit_id: None,
                    candidate_id: None,
                    candidate_type: None,
                    candidate_status: None,
                    candidate_audit_id: None,
                    reason,
                }));
            }

            let existing_candidate =
                database.get_curation_candidate(workspace_id, &candidate_id)?;
            let (candidate_status, candidate_audit_id) = if existing_candidate.is_some() {
                ("already_exists".to_owned(), None)
            } else {
                database.insert_curation_candidate(
                    &candidate_id,
                    &CreateCurationCandidateInput {
                        workspace_id: workspace_id.to_owned(),
                        candidate_type: CandidateType::Deprecate.as_str().to_owned(),
                        target_memory_id: Some(current_memory.id.clone()),
                        proposed_content: None,
                        proposed_confidence: Some(
                            (current_memory.confidence * 0.5).clamp(0.0, 1.0),
                        ),
                        proposed_trust_class: trust_class_updated.then(|| new_trust_class.clone()),
                        source_type: CandidateSource::RuleEngine.as_str().to_owned(),
                        source_id: Some(provenance_reverify_source_id(&current_memory, referent)),
                        reason: reason.clone(),
                        confidence: VERIFY_PROVENANCE_CANDIDATE_CONFIDENCE,
                        status: Some(CandidateStatus::Pending.as_str().to_owned()),
                        created_at: Some(checked_at.to_owned()),
                        ttl_expires_at: None,
                        derivation_source_refs_json: None,
                        derivation_metadata_json: None,
                    },
                )?;
                let candidate_audit_id = generate_audit_id();
                database.insert_audit(
                    &candidate_audit_id,
                    &CreateAuditInput {
                        workspace_id: Some(workspace_id.to_owned()),
                        actor: Some(VERIFY_PROVENANCE_ACTOR.to_owned()),
                        action: audit_actions::CURATION_CANDIDATE_CREATE.to_owned(),
                        target_type: Some("curation_candidate".to_owned()),
                        target_id: Some(candidate_id.clone()),
                        details: Some(provenance_reverify_candidate_audit_details(
                            &current_memory,
                            referent,
                            checked_at,
                            &candidate_id,
                        )),
                    },
                )?;
                ("created".to_owned(), Some(candidate_audit_id))
            };

            Ok(Some(VerifyProvenanceMutationReport {
                schema: VERIFY_PROVENANCE_MUTATION_SCHEMA_V1,
                memory_id: current_memory.id.clone(),
                status: referent.status,
                action,
                persisted: verification_status_updated
                    || trust_audit_id.is_some()
                    || candidate_audit_id.is_some(),
                previous_verification_status,
                new_verification_status,
                previous_verified_at,
                new_verified_at,
                verification_status_updated,
                previous_trust_class,
                new_trust_class,
                trust_class_updated,
                trust_audit_id,
                candidate_id: Some(candidate_id),
                candidate_type: Some(CandidateType::Deprecate.as_str().to_owned()),
                candidate_status: Some(candidate_status),
                candidate_audit_id,
                reason,
            }))
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to persist provenance revalidation action: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })
}

fn provenance_reverify_memory_status(status: VerifyProvenanceReferentStatus) -> &'static str {
    match status {
        VerifyProvenanceReferentStatus::Verified => PROVENANCE_STATUS_VERIFIED,
        VerifyProvenanceReferentStatus::EvidenceMissing => PROVENANCE_STATUS_MISSING,
        VerifyProvenanceReferentStatus::EvidenceDrift => PROVENANCE_STATUS_MISMATCH,
        VerifyProvenanceReferentStatus::Unverifiable => PROVENANCE_STATUS_SKIPPED,
    }
}

fn provenance_reverify_verification_note(referent: &VerifyProvenanceReferentReport) -> String {
    format!(
        "external provenance reverify {}: {}",
        referent.status.as_str(),
        referent.reason.as_str()
    )
}

fn provenance_reverify_demoted_trust_class(previous: &str) -> Option<&'static str> {
    match TrustClass::from_str(previous).ok()? {
        TrustClass::HumanExplicit | TrustClass::PeerHumanAttested | TrustClass::AgentValidated => {
            Some(TrustClass::AgentAssertion.as_str())
        }
        TrustClass::AgentAssertion | TrustClass::CassEvidence | TrustClass::LegacyImport => None,
    }
}

fn provenance_reverify_candidate_id(
    workspace_id: &str,
    memory_id: &str,
    referent: &VerifyProvenanceReferentReport,
) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [
        "ee.verify.provenance.revalidate.v1",
        workspace_id,
        memory_id,
        referent.uri.as_str(),
        referent.status.as_str(),
        referent.reason.as_str(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let hash = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    let candidate = CandidateId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string();
    format!("curate_{}", candidate.trim_start_matches("cand_"))
}

fn provenance_reverify_source_id(
    memory: &crate::db::StoredMemory,
    referent: &VerifyProvenanceReferentReport,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(memory.id.as_bytes());
    hasher.update(b"\0");
    hasher.update(referent.uri.as_bytes());
    hasher.update(b"\0");
    hasher.update(referent.status.as_str().as_bytes());
    format!("verify_provenance:{}", hasher.finalize().to_hex())
}

fn provenance_reverify_reason(
    memory: &crate::db::StoredMemory,
    referent: &VerifyProvenanceReferentReport,
) -> String {
    format!(
        "Provenance re-verification reported {} for memory {} at {} ({}); revalidate the cited evidence before authoritative use.",
        referent.status.as_str(),
        memory.id.as_str(),
        referent.uri.as_str(),
        referent.reason.as_str()
    )
}

fn provenance_reverify_trust_audit_details(
    memory: &crate::db::StoredMemory,
    referent: &VerifyProvenanceReferentReport,
    checked_at: &str,
    previous_trust_class: &str,
    new_trust_class: &str,
    candidate_id: &str,
) -> String {
    serde_json::json!({
        "schema": VERIFY_PROVENANCE_MUTATION_SCHEMA_V1,
        "memoryId": memory.id.as_str(),
        "provenanceUri": referent.uri.as_str(),
        "status": referent.status.as_str(),
        "reason": referent.reason.as_str(),
        "referentHash": referent.referent_hash.as_deref(),
        "action": referent.status.reverify_action().as_str(),
        "previousTrustClass": previous_trust_class,
        "newTrustClass": new_trust_class,
        "trustClassUpdated": true,
        "candidateId": candidate_id,
        "checkedAt": checked_at,
        "noRemoval": true,
    })
    .to_string()
}

fn provenance_reverify_candidate_audit_details(
    memory: &crate::db::StoredMemory,
    referent: &VerifyProvenanceReferentReport,
    checked_at: &str,
    candidate_id: &str,
) -> String {
    serde_json::json!({
        "schema": VERIFY_PROVENANCE_MUTATION_SCHEMA_V1,
        "candidateId": candidate_id,
        "candidateType": CandidateType::Deprecate.as_str(),
        "targetMemoryId": memory.id.as_str(),
        "provenanceUri": referent.uri.as_str(),
        "status": referent.status.as_str(),
        "reason": referent.reason.as_str(),
        "sourceType": CandidateSource::RuleEngine.as_str(),
        "sourceId": provenance_reverify_source_id(memory, referent),
        "checkedAt": checked_at,
        "purpose": "provenance_revalidate",
        "noRemoval": true,
    })
    .to_string()
}

fn verify_memory_provenance_referent(
    memory: &crate::db::StoredMemory,
    workspace_path: &Path,
    referent: VerifyProvenanceReferentReport,
) -> VerifyProvenanceReferentReport {
    if !matches!(referent.status, VerifyProvenanceReferentStatus::Verified) {
        return referent;
    }
    let Some(raw_uri) = memory.provenance_uri.as_deref() else {
        return referent;
    };
    if !matches!(
        ProvenanceUri::from_str(raw_uri),
        Ok(ProvenanceUri::File { .. })
    ) {
        return referent;
    }

    let freshness = assess_memory_evidence_freshness(memory, Some(workspace_path));
    match freshness.status {
        EvidenceFreshnessStatus::ChangedSource => provenance_referent_report(
            raw_uri,
            "file",
            VerifyProvenanceReferentStatus::EvidenceDrift,
            "file_referent_content_drift".to_owned(),
            referent.referent_hash,
            freshness.repair,
        ),
        EvidenceFreshnessStatus::MissingSource => provenance_referent_report(
            raw_uri,
            "file",
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "file_referent_missing".to_owned(),
            None,
            freshness.repair,
        ),
        EvidenceFreshnessStatus::UnreachableSource => provenance_referent_report(
            raw_uri,
            "file",
            VerifyProvenanceReferentStatus::Unverifiable,
            "file_referent_unreachable".to_owned(),
            None,
            freshness.repair,
        ),
        EvidenceFreshnessStatus::Fresh
        | EvidenceFreshnessStatus::UnsupportedSource
        | EvidenceFreshnessStatus::Unknown => referent,
    }
}

/// Re-resolve one provenance URI without mutating memories or evidence.
#[must_use]
pub fn verify_provenance_referent(
    raw_uri: &str,
    options: &VerifyProvenanceReferentOptions<'_>,
) -> VerifyProvenanceReferentReport {
    match ProvenanceUri::from_str(raw_uri) {
        Ok(uri) => verify_parsed_provenance_referent(&uri, options),
        Err(error) => provenance_referent_report(
            raw_uri.trim(),
            "unknown",
            VerifyProvenanceReferentStatus::Unverifiable,
            format!("invalid_provenance_uri: {error}"),
            None,
            Some("Fix or replace the provenance URI before re-verifying the referent.".to_owned()),
        ),
    }
}

#[must_use]
pub fn verify_parsed_provenance_referent(
    uri: &ProvenanceUri,
    options: &VerifyProvenanceReferentOptions<'_>,
) -> VerifyProvenanceReferentReport {
    match uri {
        ProvenanceUri::File { path, span } => {
            verify_file_provenance_referent(uri, options.workspace_path, path, *span)
        }
        ProvenanceUri::EeMemory(memory_id) => {
            let memory_id = memory_id.to_string();
            verify_ee_memory_provenance_referent(uri, options.database, &memory_id)
        }
        ProvenanceUri::External { scheme, body } if scheme == "git-sha" => {
            verify_git_sha_provenance_referent(uri, options.workspace_path, body)
        }
        ProvenanceUri::External { scheme, body }
            if scheme == "bench-run" || scheme == "flamegraph" =>
        {
            verify_artifact_provenance_referent(uri, options.workspace_path, scheme, body)
        }
        ProvenanceUri::Web { .. } => verify_web_provenance_referent(uri, options.allow_network),
        ProvenanceUri::CassSession { .. } => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Unverifiable,
            "cass_recheck_requires_cass_contract".to_owned(),
            None,
            Some("Retry after the CASS session/span resolver is available.".to_owned()),
        ),
        ProvenanceUri::AgentMail { .. } => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Unverifiable,
            "agent_mail_recheck_requires_mail_resolver".to_owned(),
            None,
            Some("Retry from a command path with Agent Mail lookup capability.".to_owned()),
        ),
        ProvenanceUri::External { scheme, .. } if scheme == "ee-reflect" => {
            provenance_referent_report(
                &uri.to_string(),
                uri.scheme(),
                VerifyProvenanceReferentStatus::Unverifiable,
                "reflection_recheck_requires_request_ledger".to_owned(),
                None,
                Some(
                    "Retry from a command path with reflection request ledger lookup capability."
                        .to_owned(),
                ),
            )
        }
        ProvenanceUri::External { scheme, .. } if scheme == "manual" => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Unverifiable,
            "manual_provenance_has_no_re_resolvable_referent".to_owned(),
            None,
            None,
        ),
        ProvenanceUri::External { .. } => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Unverifiable,
            "external_scheme_not_re_resolvable".to_owned(),
            None,
            Some("Register a scheme-specific provenance resolver before treating this evidence as verified.".to_owned()),
        ),
    }
}

fn verify_file_provenance_referent(
    uri: &ProvenanceUri,
    workspace_path: &Path,
    raw_path: &str,
    span: Option<LineSpan>,
) -> VerifyProvenanceReferentReport {
    let path = provenance_workspace_path(workspace_path, raw_path);
    // bd-3n8tt: refuse a symlinked referent (leaf or any parent) instead of
    // following it with metadata/read/File::open, matching the memory-freshness
    // contract (assess_memory_evidence_freshness rejects the same file:// shape
    // with a "traverses symlinked path component" error). Otherwise the verifier
    // hashes/opens the symlink target as if it were the workspace evidence file.
    match super::path_safety::first_existing_symlink_component(&path) {
        Ok(Some(symlink_path)) => {
            return provenance_referent_report(
                &uri.to_string(),
                uri.scheme(),
                VerifyProvenanceReferentStatus::Unverifiable,
                "file_referent_symlinked".to_owned(),
                None,
                Some(format!(
                    "Provenance file {} traverses symlinked path component {}; point the URI at a real workspace file.",
                    path.display(),
                    symlink_path.display()
                )),
            );
        }
        Ok(None) => {}
        Err(_) => {
            return provenance_referent_report(
                &uri.to_string(),
                uri.scheme(),
                VerifyProvenanceReferentStatus::Unverifiable,
                "file_referent_symlink_check_failed".to_owned(),
                None,
                Some(format!(
                    "Could not confirm {} is free of symlinked path components.",
                    path.display()
                )),
            );
        }
    }
    let Ok(metadata) = std::fs::metadata(&path) else {
        return provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "file_referent_missing".to_owned(),
            None,
            Some(format!("Restore or update {}", path.display())),
        );
    };
    if !metadata.is_file() {
        return provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "file_referent_not_regular_file".to_owned(),
            None,
            Some(format!(
                "Point the provenance URI at a regular file, not {}",
                path.display()
            )),
        );
    }

    match span {
        Some(span) => verify_file_span_provenance_referent(uri, &path, span),
        None => {
            let hash = if metadata.len()
                <= u64::try_from(VERIFY_PROVENANCE_FILE_SCAN_LIMIT_BYTES).unwrap_or(u64::MAX)
            {
                std::fs::read(&path)
                    .ok()
                    .map(|bytes| format!("blake3:{}", blake3::hash(&bytes).to_hex()))
            } else {
                None
            };
            provenance_referent_report(
                &uri.to_string(),
                uri.scheme(),
                VerifyProvenanceReferentStatus::Verified,
                "file_referent_present".to_owned(),
                hash,
                None,
            )
        }
    }
}

fn verify_file_span_provenance_referent(
    uri: &ProvenanceUri,
    path: &Path,
    span: LineSpan,
) -> VerifyProvenanceReferentReport {
    let end = span.end.unwrap_or(span.start);
    let Ok(file) = std::fs::File::open(path) else {
        return provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "file_referent_missing".to_owned(),
            None,
            Some(format!("Restore or update {}", path.display())),
        );
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut line_number = 0_u64;
    let mut bytes_seen = 0_usize;
    let mut selected = Vec::<u8>::new();

    loop {
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                return provenance_referent_report(
                    &uri.to_string(),
                    uri.scheme(),
                    VerifyProvenanceReferentStatus::Unverifiable,
                    format!("file_span_read_error: {error}"),
                    None,
                    Some("Retry after the file can be read cleanly.".to_owned()),
                );
            }
        };
        bytes_seen = bytes_seen.saturating_add(read);
        if bytes_seen > VERIFY_PROVENANCE_FILE_SCAN_LIMIT_BYTES {
            return provenance_referent_report(
                &uri.to_string(),
                uri.scheme(),
                VerifyProvenanceReferentStatus::Unverifiable,
                "file_span_scan_limit_exceeded".to_owned(),
                None,
                Some("Narrow the provenance span or raise the verifier scan limit.".to_owned()),
            );
        }
        line_number = line_number.saturating_add(1);
        if line_number >= span.start && line_number <= end {
            selected.extend_from_slice(line.as_bytes());
        }
        if line_number >= end {
            break;
        }
    }

    if line_number < end {
        return provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::EvidenceDrift,
            format!("file_span_missing: expected_through_line_{end}, observed_{line_number}"),
            None,
            Some("Update the file provenance line span or revalidate the memory.".to_owned()),
        );
    }

    provenance_referent_report(
        &uri.to_string(),
        uri.scheme(),
        VerifyProvenanceReferentStatus::Verified,
        "file_span_present".to_owned(),
        Some(format!("blake3:{}", blake3::hash(&selected).to_hex())),
        None,
    )
}

fn verify_ee_memory_provenance_referent(
    uri: &ProvenanceUri,
    database: Option<&DbConnection>,
    memory_id: &str,
) -> VerifyProvenanceReferentReport {
    let Some(database) = database else {
        return provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Unverifiable,
            "ee_memory_recheck_requires_database".to_owned(),
            None,
            Some("Run provenance verification from an initialized workspace database.".to_owned()),
        );
    };
    match database.get_memory(memory_id) {
        Ok(Some(memory)) if memory.tombstoned_at.is_none() && memory.valid_to.is_none() => {
            provenance_referent_report(
                &uri.to_string(),
                uri.scheme(),
                VerifyProvenanceReferentStatus::Verified,
                "ee_memory_referent_present".to_owned(),
                memory.provenance_chain_hash,
                None,
            )
        }
        Ok(Some(_)) => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::EvidenceDrift,
            "ee_memory_referent_no_longer_live".to_owned(),
            None,
            Some(
                "Review the superseded or tombstoned memory before trusting this evidence."
                    .to_owned(),
            ),
        ),
        Ok(None) => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "ee_memory_referent_missing".to_owned(),
            None,
            Some("Restore the memory or replace the provenance URI.".to_owned()),
        ),
        Err(error) => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Unverifiable,
            format!("ee_memory_lookup_error: {error}"),
            None,
            Some("Run ee doctor before retrying provenance verification.".to_owned()),
        ),
    }
}

fn verify_git_sha_provenance_referent(
    uri: &ProvenanceUri,
    workspace_path: &Path,
    revision: &str,
) -> VerifyProvenanceReferentReport {
    let commit_ref = format!("{revision}^{{commit}}");
    match run_bounded_verify_step_command(
        "git",
        &["cat-file", "-e", &commit_ref],
        workspace_path,
        VERIFY_PROVENANCE_GIT_TIMEOUT,
        VERIFY_STEP_OUTPUT_LIMIT_BYTES,
    ) {
        Ok(output) if output.status.success() => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Verified,
            "git_commit_reachable".to_owned(),
            None,
            None,
        ),
        Ok(_) => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "git_commit_unreachable".to_owned(),
            None,
            Some("Fetch the missing commit or revalidate memories that cite it.".to_owned()),
        ),
        Err(error) => provenance_referent_report(
            &uri.to_string(),
            uri.scheme(),
            VerifyProvenanceReferentStatus::Unverifiable,
            format!("git_recheck_error: {}", error.message),
            None,
            Some("Retry after git is available for the workspace.".to_owned()),
        ),
    }
}

fn verify_artifact_provenance_referent(
    uri: &ProvenanceUri,
    workspace_path: &Path,
    scheme: &str,
    body: &str,
) -> VerifyProvenanceReferentReport {
    let path = provenance_workspace_path(workspace_path, body);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => provenance_referent_report(
            &uri.to_string(),
            scheme,
            VerifyProvenanceReferentStatus::Verified,
            "artifact_referent_present".to_owned(),
            if metadata.len()
                <= u64::try_from(VERIFY_PROVENANCE_FILE_SCAN_LIMIT_BYTES).unwrap_or(u64::MAX)
            {
                std::fs::read(&path)
                    .ok()
                    .map(|bytes| format!("blake3:{}", blake3::hash(&bytes).to_hex()))
            } else {
                None
            },
            None,
        ),
        Ok(_) => provenance_referent_report(
            &uri.to_string(),
            scheme,
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "artifact_referent_not_regular_file".to_owned(),
            None,
            Some(format!(
                "Point the provenance URI at a regular artifact file, not {}",
                path.display()
            )),
        ),
        Err(_) => provenance_referent_report(
            &uri.to_string(),
            scheme,
            VerifyProvenanceReferentStatus::EvidenceMissing,
            "artifact_referent_missing".to_owned(),
            None,
            Some(format!("Restore or update {}", path.display())),
        ),
    }
}

fn verify_web_provenance_referent(
    uri: &ProvenanceUri,
    allow_network: bool,
) -> VerifyProvenanceReferentReport {
    let reason = if allow_network {
        "network_recheck_not_implemented"
    } else {
        "network_recheck_disabled"
    };
    provenance_referent_report(
        &uri.to_string(),
        uri.scheme(),
        VerifyProvenanceReferentStatus::Unverifiable,
        reason.to_owned(),
        None,
        Some("Network provenance rechecks are opt-in and require a network resolver.".to_owned()),
    )
}

fn provenance_workspace_path(workspace_path: &Path, raw_path: &str) -> PathBuf {
    let path = Path::new(raw_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_path.join(path)
    }
}

fn provenance_memory_is_due(
    memory: &crate::db::StoredMemory,
    stale_after_days: u32,
    now: DateTime<Utc>,
) -> bool {
    if stale_after_days == 0 {
        return true;
    }
    let Some(verified_at) = memory.provenance_verified_at.as_deref() else {
        return true;
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(verified_at) else {
        return true;
    };
    now.signed_duration_since(parsed.with_timezone(&Utc))
        >= ChronoDuration::days(i64::from(stale_after_days))
}

fn provenance_referent_report(
    uri: &str,
    scheme: &str,
    status: VerifyProvenanceReferentStatus,
    reason: String,
    referent_hash: Option<String>,
    repair: Option<String>,
) -> VerifyProvenanceReferentReport {
    VerifyProvenanceReferentReport {
        schema: VERIFY_PROVENANCE_REFERENT_SCHEMA_V1,
        uri: uri.to_owned(),
        scheme: scheme.to_owned(),
        status,
        reason,
        referent_hash,
        repair,
    }
}

// ============================================================================
// Verification Runner
// ============================================================================

pub fn record_verification_evidence(
    options: VerificationRecordOptions<'_>,
) -> Result<VerificationRecordReport, DomainError> {
    record_verification_evidence_with_authority(
        options,
        VerificationEvidenceAuthority::CallerAuthored,
    )
}

/// Persist evidence normalized by one of the specialized proof parsers.
///
/// This is crate-private on purpose: external library callers can submit
/// generic evidence, but cannot label their own normalized envelope as having
/// passed an in-tree proof parser.
pub(crate) fn record_validated_verification_evidence(
    options: VerificationRecordOptions<'_>,
    authority: VerificationEvidenceAuthority,
) -> Result<VerificationRecordReport, DomainError> {
    record_verification_evidence_with_authority(options, authority)
}

fn record_verification_evidence_with_authority(
    options: VerificationRecordOptions<'_>,
    authority: VerificationEvidenceAuthority,
) -> Result<VerificationRecordReport, DomainError> {
    if options.target_type.trim().is_empty() {
        return Err(DomainError::Usage {
            message: "verification record target type must not be empty".to_owned(),
            repair: Some("pass --target-type memory or --target-type pack".to_owned()),
        });
    }
    if options.target_id.trim().is_empty() {
        return Err(DomainError::Usage {
            message: "verification record target id must not be empty".to_owned(),
            repair: Some("pass --target-id <memory-or-pack-id>".to_owned()),
        });
    }
    let mut evidence = options.evidence;
    validate_verification_evidence_authority(&evidence, authority)?;
    normalize_validated_verification_evidence(&mut evidence, authority);
    validate_verification_record(&evidence)?;
    let content_hash =
        verification_evidence_content_hash(&evidence).map_err(|message| DomainError::Storage {
            message,
            repair: Some("inspect the verification evidence JSON and retry".to_owned()),
        })?;

    let connection = open_verification_database(options.database_path)?;
    let workspace_id = ensure_verification_workspace(&connection, options.workspace_path)?;
    if let Some(existing) = find_existing_verification_ingest(
        &connection,
        &content_hash,
        options.target_type,
        options.target_id,
        authority,
    )? {
        return Ok(VerificationRecordReport {
            schema: VERIFY_RECORD_REPORT_SCHEMA_V1,
            command: "verification ingest",
            version: build_info().version,
            audit_id: existing.audit_id,
            content_hash,
            workspace_id,
            target_type: options.target_type.trim().to_owned(),
            target_id: options.target_id.trim().to_owned(),
            persisted: false,
            replayed: true,
            authority: authority.as_str(),
            pass_authority_validated: authority.can_authorize_pass(),
            degradations: vec!["degraded.verification_idempotent_replay".to_owned()],
            evidence: existing.record,
        });
    }

    let audit_id = generate_audit_id();
    let details = VerificationAuditDetails::new(content_hash.clone(), &evidence, authority);
    let details = serde_json::to_string(&details).map_err(|error| DomainError::Storage {
        message: format!("Failed to serialize verification evidence: {error}"),
        repair: Some("inspect the verification evidence JSON and retry".to_owned()),
    })?;

    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.clone()),
                actor: options
                    .actor
                    .map(str::trim)
                    .filter(|actor| !actor.is_empty())
                    .map(str::to_owned),
                action: audit_actions::VERIFICATION_INGEST.to_owned(),
                target_type: Some(options.target_type.trim().to_owned()),
                target_id: Some(options.target_id.trim().to_owned()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to record verification audit row: {error}"),
            repair: Some("ee audit timeline --surface verification --json".to_owned()),
        })?;

    Ok(VerificationRecordReport {
        schema: VERIFY_RECORD_REPORT_SCHEMA_V1,
        command: "verification ingest",
        version: build_info().version,
        audit_id,
        content_hash,
        workspace_id,
        target_type: options.target_type.trim().to_owned(),
        target_id: options.target_id.trim().to_owned(),
        persisted: true,
        replayed: false,
        authority: authority.as_str(),
        pass_authority_validated: authority.can_authorize_pass(),
        degradations: Vec::new(),
        evidence,
    })
}

pub fn verification_closure_guidance_from_ledger(
    options: &VerificationClosureGuidanceOptions<'_>,
) -> Result<VerificationClosureGuidanceReport, DomainError> {
    let connection = open_verification_database(options.database_path)?;
    let records = if let Some(bead_id) = options.bead_id {
        list_verification_records_for_bead(&connection, bead_id)?
    } else {
        list_verification_records(&connection, None)?
    };
    let guidance =
        verification_closure_guidance(options.bead_id, &options.requirements, records.as_slice());

    Ok(VerificationClosureGuidanceReport {
        schema: VERIFY_CLOSURE_GUIDANCE_REPORT_SCHEMA_V1,
        command: "verification closure-guidance",
        version: build_info().version,
        bead_id: options.bead_id.map(str::to_owned),
        evidence_count: records.len(),
        guidance,
    })
}

pub fn verification_records_for_target(
    connection: &DbConnection,
    target_type: &str,
    target_id: &str,
) -> Result<Vec<VerificationEvidenceRecord>, String> {
    let entries = connection
        .list_audit_by_target(target_type, target_id, None)
        .map_err(|error| format!("failed to query verification audit rows: {error}"))?;
    parse_verification_audit_entries(entries)
}

fn open_verification_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    let connection =
        DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee init --workspace .".to_owned()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database before verification ledger access: {error}"),
        repair: Some("ee migrate run --workspace .".to_owned()),
    })?;
    Ok(connection)
}

fn ensure_verification_workspace(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DomainError> {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    crate::core::workspace::ensure_bound_workspace(
        connection,
        &super::workspace::stable_workspace_id(&canonical),
        &[canonical.as_path(), workspace_path],
    )
}

fn validate_verification_record(record: &VerificationEvidenceRecord) -> Result<(), DomainError> {
    if record.schema != VERIFICATION_EVIDENCE_SCHEMA_V1 {
        return Err(DomainError::Usage {
            message: format!(
                "verification evidence schema must be {}, got {}",
                VERIFICATION_EVIDENCE_SCHEMA_V1, record.schema
            ),
            repair: Some("regenerate the evidence with the current ee schema".to_owned()),
        });
    }
    if record.verification_id.trim().is_empty() {
        return Err(DomainError::Usage {
            message: "verification evidence verificationId must not be empty".to_owned(),
            repair: Some("set verificationId to a stable ver_* identifier".to_owned()),
        });
    }
    if record.gate_name.trim().is_empty() {
        return Err(DomainError::Usage {
            message: "verification evidence gateName must not be empty".to_owned(),
            repair: Some("set gateName to the gate being recorded".to_owned()),
        });
    }
    if record.command.trim().is_empty() {
        return Err(DomainError::Usage {
            message: "verification evidence command must not be empty".to_owned(),
            repair: Some("record the command that produced this evidence".to_owned()),
        });
    }
    Ok(())
}

fn validate_verification_evidence_authority(
    record: &VerificationEvidenceRecord,
    authority: VerificationEvidenceAuthority,
) -> Result<(), DomainError> {
    let source_is_verification = record.producer.source_system.as_str() == "verification";
    let valid = match authority {
        VerificationEvidenceAuthority::CallerAuthored => true,
        VerificationEvidenceAuthority::ValidatedRunRecord => {
            source_is_verification
                && record.command.starts_with("verification_run ")
                && record.producer.run.run_id.is_some()
        }
        VerificationEvidenceAuthority::ValidatedRchVerify => {
            source_is_verification
                && record.producer.run.run_id.as_deref() == Some(RCH_VERIFY_SCHEMA_V1)
                && record.offload.offload_tool.as_deref() == Some("rch")
        }
        VerificationEvidenceAuthority::ValidatedGithubActions => {
            source_is_verification
                && record.offload.offload_tool.as_deref() == Some("github_actions")
        }
    };
    if valid {
        return Ok(());
    }

    Err(DomainError::Usage {
        message: format!(
            "verification evidence does not match its validated parser authority ({authority:?})"
        ),
        repair: Some("parse the original proof with the matching in-tree verifier".to_owned()),
    })
}

fn normalize_validated_verification_evidence(
    record: &mut VerificationEvidenceRecord,
    authority: VerificationEvidenceAuthority,
) {
    if authority != VerificationEvidenceAuthority::ValidatedRunRecord {
        return;
    }
    let Some(substrate) = record
        .command
        .strip_prefix("verification_run ")
        .and_then(|summary| summary.split_whitespace().next())
        .filter(|substrate| {
            matches!(
                *substrate,
                "remote_artifact" | "github_actions_artifact" | "remote_build_artifact"
            )
        })
        .map(str::to_owned)
    else {
        return;
    };

    // A downloaded artifact was built and exercised remotely even when it was
    // later consumed on this host. Preserve that distinction for closure and
    // posture instead of allowing the normalized record to look like a local
    // source build.
    record.offload.required_remote = true;
    record.offload.remote_required_env = None;
    record.offload.offload_tool = Some(substrate);
    record.offload.fallback_detected = false;
    record.offload.fallback_reason = None;
}

#[derive(Clone, Debug)]
struct ParsedVerificationAuditEntry {
    audit_id: String,
    content_hash: String,
    target_type: Option<String>,
    target_id: Option<String>,
    authority: VerificationEvidenceAuthority,
    record: VerificationEvidenceRecord,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerificationAuditDetails {
    schema: String,
    content_hash: String,
    #[serde(default)]
    authority: VerificationEvidenceAuthority,
    producer: ProducerMetadata,
    status: VerificationStatus,
    evidence: VerificationEvidenceRecord,
}

impl VerificationAuditDetails {
    fn new(
        content_hash: String,
        evidence: &VerificationEvidenceRecord,
        authority: VerificationEvidenceAuthority,
    ) -> Self {
        Self {
            schema: VERIFICATION_LEDGER_ENTRY_SCHEMA_V1.to_owned(),
            content_hash,
            authority,
            producer: evidence.producer.clone(),
            status: evidence.status,
            evidence: evidence.clone(),
        }
    }
}

fn verification_evidence_content_hash(
    record: &VerificationEvidenceRecord,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(record)
        .map_err(|error| format!("Failed to canonicalize verification evidence: {error}"))?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn is_verification_audit_action(action: &str) -> bool {
    action == audit_actions::VERIFICATION_INGEST || action == LEGACY_VERIFICATION_RECORD_ACTION
}

fn list_verification_audit_entries(
    connection: &DbConnection,
) -> Result<Vec<crate::db::StoredAuditEntry>, DomainError> {
    let mut entries = connection
        .list_audit_by_action(audit_actions::VERIFICATION_INGEST, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query verification ledger: {error}"),
            repair: Some("ee audit timeline --surface verification --json".to_owned()),
        })?;

    if audit_actions::VERIFICATION_INGEST != LEGACY_VERIFICATION_RECORD_ACTION {
        let legacy_entries = connection
            .list_audit_by_action(LEGACY_VERIFICATION_RECORD_ACTION, None)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query legacy verification ledger: {error}"),
                repair: Some("ee audit timeline --surface verification --json".to_owned()),
            })?;
        entries.extend(legacy_entries);
    }

    Ok(entries)
}

fn find_existing_verification_ingest(
    connection: &DbConnection,
    content_hash: &str,
    target_type: &str,
    target_id: &str,
    authority: VerificationEvidenceAuthority,
) -> Result<Option<ParsedVerificationAuditEntry>, DomainError> {
    let entries = list_verification_audit_entries(connection)?;
    let parsed = parse_verification_audit_entries_with_metadata(entries).map_err(|message| {
        DomainError::Storage {
            message,
            repair: Some("ee audit verify --json".to_owned()),
        }
    })?;
    let target_type = target_type.trim();
    let target_id = target_id.trim();
    Ok(parsed.into_iter().find(|entry| {
        entry.content_hash == content_hash
            && entry.target_type.as_deref() == Some(target_type)
            && entry.target_id.as_deref() == Some(target_id)
            && entry.authority == authority
    }))
}

fn list_verification_records_for_bead(
    connection: &DbConnection,
    bead_id: &str,
) -> Result<Vec<VerificationEvidenceRecord>, DomainError> {
    list_verification_records(connection, Some(bead_id))
}

fn list_verification_records(
    connection: &DbConnection,
    bead_id: Option<&str>,
) -> Result<Vec<VerificationEvidenceRecord>, DomainError> {
    let entries = list_verification_audit_entries(connection)?;
    let records =
        parse_verification_audit_entries(entries).map_err(|message| DomainError::Storage {
            message,
            repair: Some("ee audit verify --json".to_owned()),
        })?;
    if let Some(bead_id) = bead_id {
        Ok(records
            .into_iter()
            .filter(|record| record.bead_id.as_deref() == Some(bead_id))
            .collect())
    } else {
        Ok(records)
    }
}

fn parse_verification_audit_entries(
    entries: Vec<crate::db::StoredAuditEntry>,
) -> Result<Vec<VerificationEvidenceRecord>, String> {
    Ok(parse_verification_audit_entries_with_metadata(entries)?
        .into_iter()
        .map(|mut entry| {
            if !entry.authority.can_authorize_pass() && entry.record.is_authoritative_pass() {
                entry.record.status = VerificationStatus::Unknown;
            }
            entry.record
        })
        .collect())
}

fn parse_verification_audit_entries_with_metadata(
    entries: Vec<crate::db::StoredAuditEntry>,
) -> Result<Vec<ParsedVerificationAuditEntry>, String> {
    let mut records = Vec::new();
    for entry in entries
        .into_iter()
        .filter(|entry| is_verification_audit_action(&entry.action))
    {
        let Some(details) = entry.details else {
            return Err(format!(
                "verification audit row {} is missing details",
                entry.id
            ));
        };
        let (record, content_hash, authority) =
            match serde_json::from_str::<VerificationAuditDetails>(&details) {
                Ok(details) if details.schema == VERIFICATION_LEDGER_ENTRY_SCHEMA_V1 => {
                    (details.evidence, details.content_hash, details.authority)
                }
                Ok(details) => {
                    return Err(format!(
                        "verification audit row {} has unsupported details schema {}",
                        entry.id, details.schema
                    ));
                }
                Err(_) => {
                    let record = serde_json::from_str::<VerificationEvidenceRecord>(&details)
                        .map_err(|error| {
                            format!(
                                "verification audit row {} has invalid evidence JSON: {error}",
                                entry.id
                            )
                        })?;
                    let content_hash = verification_evidence_content_hash(&record)?;
                    (
                        record,
                        content_hash,
                        VerificationEvidenceAuthority::CallerAuthored,
                    )
                }
            };
        records.push(ParsedVerificationAuditEntry {
            audit_id: entry.id,
            content_hash,
            target_type: entry.target_type,
            target_id: entry.target_id,
            authority,
            record,
        });
    }
    records.sort_by(|left, right| {
        left.record
            .finished_at
            .cmp(&right.record.finished_at)
            .then_with(|| left.record.started_at.cmp(&right.record.started_at))
            .then_with(|| {
                left.record
                    .verification_id
                    .cmp(&right.record.verification_id)
            })
    });
    Ok(records)
}

#[must_use]
pub fn gather_verification_posture(workspace_path: Option<&Path>) -> VerificationPostureReport {
    gather_verification_posture_with_connection(workspace_path, None)
}

#[must_use]
pub(crate) fn gather_verification_posture_with_connection(
    workspace_path: Option<&Path>,
    connection: Option<&DbConnection>,
) -> VerificationPostureReport {
    let Some(workspace_path) = workspace_path else {
        return VerificationPostureReport::not_inspected();
    };
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return VerificationPostureReport::unavailable(
            "not_initialized",
            "verification_ledger_missing",
            "ee init --workspace .",
        );
    }

    let owned_connection;
    let connection = if let Some(connection) = connection {
        connection
    } else {
        match DbConnection::open_file(&database_path) {
            Ok(connection) => {
                owned_connection = connection;
                &owned_connection
            }
            Err(_) => {
                return VerificationPostureReport::unavailable(
                    "unavailable",
                    "verification_ledger_unreadable",
                    "ee doctor --workspace . --json",
                );
            }
        }
    };
    match list_verification_records(connection, None) {
        Ok(records) => VerificationPostureReport::from_records(Utc::now(), records.as_slice()),
        Err(_) => VerificationPostureReport::unavailable(
            "unavailable",
            "verification_ledger_query_failed",
            "ee audit timeline --surface verification --json",
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerificationAgeBucket {
    Recent,
    Stale,
    Unknown,
    Malformed,
}

fn verification_record_age_bucket(
    now: DateTime<Utc>,
    record: &VerificationEvidenceRecord,
) -> VerificationAgeBucket {
    let timestamp = record.finished_at.as_ref().or(record.started_at.as_ref());
    let Some(timestamp) = timestamp else {
        return VerificationAgeBucket::Unknown;
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(timestamp) else {
        return VerificationAgeBucket::Malformed;
    };
    let age = now.signed_duration_since(parsed.with_timezone(&Utc));
    if age <= ChronoDuration::hours(i64::from(VERIFICATION_POSTURE_WINDOW_HOURS)) {
        VerificationAgeBucket::Recent
    } else {
        VerificationAgeBucket::Stale
    }
}

fn verification_remote_required(record: &VerificationEvidenceRecord) -> bool {
    record.offload.required_remote
        || record.offload.offload_tool.as_deref() == Some("rch")
        || record.command.contains("RCH_REQUIRE_REMOTE=1")
        || record.command.contains("rch exec")
}

fn verification_record_has_artifact_manifest(record: &VerificationEvidenceRecord) -> bool {
    record
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "remote_artifact_attestation")
}

fn verification_record_mentions_rch_topology(record: &VerificationEvidenceRecord) -> bool {
    let mut haystack = String::with_capacity(
        record.command.len()
            + record
                .offload
                .fallback_reason
                .as_ref()
                .map_or(0, String::len)
            + record
                .output_summary
                .stdout_tail
                .as_ref()
                .map_or(0, String::len)
            + record
                .output_summary
                .stderr_tail
                .as_ref()
                .map_or(0, String::len),
    );
    haystack.push_str(&record.command);
    if let Some(reason) = record.offload.fallback_reason.as_ref() {
        haystack.push(' ');
        haystack.push_str(reason);
    }
    if let Some(stdout) = record.output_summary.stdout_tail.as_ref() {
        haystack.push(' ');
        haystack.push_str(stdout);
    }
    if let Some(stderr) = record.output_summary.stderr_tail.as_ref() {
        haystack.push(' ');
        haystack.push_str(stderr);
    }
    let haystack = haystack.to_ascii_lowercase();
    [
        "rch-e104",
        "rch-e327",
        "all_workers",
        "all workers",
        "preflight",
        "topology",
        "worker unavailable",
        "worker pressure",
        "path-dep",
        "path dep",
        "canonical remote root",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

fn verification_posture_status(report: &VerificationPostureReport) -> &'static str {
    if report.record_count == 0 {
        "no_evidence"
    } else if report.advisory_counts.topology_blocked > 0 {
        "blocked"
    } else if report.advisory_counts.local_disallowed > 0
        || report.advisory_counts.remote_failed > 0
        || report.advisory_counts.missing_artifact_manifest > 0
        || report.evidence_health.malformed_timestamp_count > 0
    {
        "degraded_recoverable"
    } else if report.advisory_counts.remote_in_flight > 0 {
        "initializing"
    } else if report.recent_reusable_run_count > 0 {
        "ok"
    } else {
        "stale"
    }
}

fn verification_evidence_health_status(report: &VerificationPostureReport) -> &'static str {
    if report.record_count == 0 {
        "empty"
    } else if report.evidence_health.issue_count == 0 {
        "healthy"
    } else if report.evidence_health.topology_blocked_count > 0 {
        "blocked"
    } else {
        "degraded"
    }
}

fn verification_evidence_health_reason(report: &VerificationPostureReport) -> Option<&'static str> {
    if report.record_count == 0 {
        Some("no_verification_evidence_recorded")
    } else if report.evidence_health.topology_blocked_count > 0 {
        Some("rch_topology_or_worker_preflight_blocked")
    } else if report.evidence_health.local_disallowed_count > 0 {
        Some("remote_required_gate_used_local_fallback")
    } else if report.evidence_health.missing_artifact_manifest_count > 0 {
        Some("artifact_manifest_missing")
    } else if report.evidence_health.malformed_timestamp_count > 0 {
        Some("malformed_verification_timestamp")
    } else {
        None
    }
}

fn verification_posture_recovery_actions(
    report: &VerificationPostureReport,
) -> Vec<VerificationPostureRecoveryAction> {
    let mut actions = Vec::new();
    if report.record_count == 0 || report.advisory_counts.missing_artifact_manifest > 0 {
        actions.push(VerificationPostureRecoveryAction {
            priority: 1,
            kind: "import_j1_log".to_owned(),
            command: Some(
                "ee verify broker lookup --runs-jsonl <j1.jsonl> --command-hash <hash> --json"
                    .to_owned(),
            ),
            message: "Inspect retained J1 test-event logs so artifact manifests can back verification reuse.".to_owned(),
            related_bead_id: None,
        });
    }
    if report.advisory_counts.topology_blocked > 0 {
        actions.push(VerificationPostureRecoveryAction {
            priority: 2,
            kind: "inspect_topology_diagnostic".to_owned(),
            command: Some("ee status --workspace . --json | jq '.data.rchWorkerPressure'".to_owned()),
            message: "Use bd-1zb7k.13 worker-pressure topology diagnostics before launching another remote Cargo gate.".to_owned(),
            related_bead_id: Some("bd-1zb7k.13".to_owned()),
        });
    }
    if report.advisory_counts.remote_in_flight > 0 {
        actions.push(VerificationPostureRecoveryAction {
            priority: 3,
            kind: "wait_for_active_agent".to_owned(),
            command: Some("br list --status in_progress --json".to_owned()),
            message: "An equivalent remote-required verification appears in flight; wait or coordinate before duplicating it.".to_owned(),
            related_bead_id: None,
        });
    }
    if report.advisory_counts.local_disallowed > 0
        || report.advisory_counts.remote_failed > 0
        || report.stale_run_count > 0
    {
        actions.push(VerificationPostureRecoveryAction {
            priority: 4,
            kind: "rerun_through_rch".to_owned(),
            command: Some("scripts/rch_verify.sh -- <cargo command>".to_owned()),
            message:
                "Rerun stale, failed, or local-fallback Cargo evidence through required-remote RCH."
                    .to_owned(),
            related_bead_id: None,
        });
    }
    actions
}

fn saturating_len_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[must_use]
pub fn default_rch_cargo_closure_requirements() -> Vec<VerificationGateRequirement> {
    rch_cargo_closure_requirements()
}

#[must_use]
pub fn verification_response_json(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V2,
        "success": true,
        "data": data,
        "degraded": [],
    })
}

/// Run the verification pipeline.
#[must_use]
pub fn run_verification(options: &VerifyOptions) -> VerifyReport {
    let version = build_info().version;
    let workspace_path = options
        .workspace_path
        .clone()
        .unwrap_or_else(|| ".".to_string());

    let steps_to_run = options
        .steps
        .clone()
        .unwrap_or_else(|| VerifyStep::ALL.to_vec());

    let start = Instant::now();
    let mut results: Vec<StepResult> = Vec::new();
    let mut had_failure = false;

    for step in &steps_to_run {
        if options.fail_fast && had_failure {
            results.push(StepResult::skipped(*step, "fail-fast after prior failure"));
            continue;
        }

        if options.dry_run {
            results.push(StepResult::skipped(*step, "dry-run mode"));
            continue;
        }

        let result = run_step(*step, &workspace_path);
        if !result.passed {
            had_failure = true;
        }
        results.push(result);
    }

    let total_duration = start.elapsed();

    let passed_count = results.iter().filter(|r| r.passed && !r.skipped).count();
    let failed_count = results.iter().filter(|r| !r.passed).count();
    let skipped_count = results.iter().filter(|r| r.skipped).count();

    VerifyReport {
        version,
        workspace_path,
        all_passed: failed_count == 0,
        total_duration_ms: duration_millis_saturating(total_duration),
        steps: results,
        failed_count,
        passed_count,
        skipped_count,
    }
}

fn run_step(step: VerifyStep, workspace_path: &str) -> StepResult {
    let start = Instant::now();

    let (program, args) = match step {
        VerifyStep::Format => ("cargo", vec!["fmt", "--check"]),
        VerifyStep::Clippy => (
            "cargo",
            vec!["clippy", "--all-targets", "--", "-D", "warnings"],
        ),
        VerifyStep::Test => ("cargo", vec!["test"]),
        VerifyStep::ForbiddenDeps => ("cargo", vec!["test", "forbidden_deps"]),
    };

    let result = run_bounded_verify_step_command(
        program,
        &args,
        Path::new(workspace_path),
        VERIFY_STEP_TIMEOUT,
        VERIFY_STEP_OUTPUT_LIMIT_BYTES,
    );

    let duration = start.elapsed();

    match result {
        Ok(output) => {
            if output.status.success() {
                StepResult::passed(step, duration, output.stdout, output.stderr)
            } else {
                StepResult::failed(
                    step,
                    duration,
                    output.stdout,
                    output.stderr,
                    output.status.code(),
                )
            }
        }
        Err(e) => StepResult::failed(
            step,
            duration,
            String::new(),
            format!("Failed to execute command: {}", e.message),
            None,
        ),
    }
}

#[derive(Debug)]
struct BoundedVerifyStepOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct BoundedVerifyStepError {
    message: String,
}

fn run_bounded_verify_step_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
    output_limit_bytes: usize,
) -> Result<BoundedVerifyStepOutput, BoundedVerifyStepError> {
    let timeout = timeout.max(Duration::from_millis(1));
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| BoundedVerifyStepError {
            message: error.to_string(),
        })?;

    let stdout = child.stdout.take().ok_or_else(|| BoundedVerifyStepError {
        message: "failed to capture stdout pipe".to_string(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| BoundedVerifyStepError {
        message: "failed to capture stderr pipe".to_string(),
    })?;
    let stdout_thread = thread::spawn(move || read_pipe_limited(stdout, output_limit_bytes));
    let stderr_thread = thread::spawn(move || read_pipe_limited(stderr, output_limit_bytes));

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return Err(BoundedVerifyStepError {
                        message: format!("command timed out after {} ms", timeout.as_millis()),
                    });
                }
                thread::sleep(Duration::from_millis(10).min(timeout.saturating_sub(elapsed)));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(BoundedVerifyStepError {
                    message: error.to_string(),
                });
            }
        }
    };

    // A panicked reader thread must fail the step: fabricating empty
    // captured output would mislead the proof surface this step feeds.
    let stdout =
        output_bytes_to_string(stdout_thread.join().map_err(|_| BoundedVerifyStepError {
            message: "verify step stdout reader thread panicked".to_string(),
        })?);
    let stderr =
        output_bytes_to_string(stderr_thread.join().map_err(|_| BoundedVerifyStepError {
            message: "verify step stderr reader thread panicked".to_string(),
        })?);

    Ok(BoundedVerifyStepOutput {
        status,
        stdout,
        stderr,
    })
}

fn read_pipe_limited<R: Read>(mut reader: R, limit: usize) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        let remaining = limit.saturating_sub(output.len());
        if remaining > 0 {
            output.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    output
}

fn output_bytes_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// Check if a path should be gitignored based on artifact policy.
#[must_use]
pub fn should_gitignore(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    for rule in ARTIFACT_RULES {
        if !rule.versioned {
            let trimmed = rule.pattern.trim_end_matches('/');
            if path_str == trimmed || path_str.starts_with(&format!("{trimmed}/")) {
                return true;
            }
        }
    }
    false
}

/// Get patterns that should be in .gitignore.
#[must_use]
pub fn gitignore_patterns() -> Vec<&'static str> {
    ARTIFACT_RULES
        .iter()
        .filter(|r| !r.versioned)
        .map(|r| r.pattern)
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestResult, ensure, ensure_equal};

    #[test]
    fn verify_step_strings_are_stable() -> TestResult {
        ensure_equal(&VerifyStep::Format.as_str(), &"format", "format")?;
        ensure_equal(&VerifyStep::Clippy.as_str(), &"clippy", "clippy")?;
        ensure_equal(&VerifyStep::Test.as_str(), &"test", "test")?;
        ensure_equal(
            &VerifyStep::ForbiddenDeps.as_str(),
            &"forbidden_deps",
            "forbidden_deps",
        )
    }

    #[test]
    fn verify_step_all_contains_all_steps() -> TestResult {
        ensure_equal(&VerifyStep::ALL.len(), &4, "step count")
    }

    #[test]
    fn all_steps_are_required() -> TestResult {
        for step in VerifyStep::ALL {
            ensure(step.is_required(), format!("{} should be required", step))?;
        }
        Ok(())
    }

    #[test]
    fn verification_response_json_includes_clean_degraded_array() -> TestResult {
        let response = verification_response_json(serde_json::json!({
            "command": "verification show"
        }));

        ensure_equal(
            &response.get("schema").and_then(serde_json::Value::as_str),
            &Some(RESPONSE_SCHEMA_V2),
            "response schema",
        )?;
        ensure_equal(
            &response.get("success").and_then(serde_json::Value::as_bool),
            &Some(true),
            "success",
        )?;
        ensure_equal(
            &response.get("degraded"),
            &Some(&serde_json::json!([])),
            "clean degraded array",
        )
    }

    #[test]
    #[cfg(unix)]
    fn verify_step_command_timeout_is_bounded() -> TestResult {
        let error = run_bounded_verify_step_command(
            "sh",
            &["-c", "sleep 2"],
            Path::new("."),
            Duration::from_millis(20),
            1024,
        )
        .expect_err("sleeping command should time out");

        ensure(
            error.message.contains("timed out"),
            "timeout error should mention timeout",
        )
    }

    #[test]
    #[cfg(unix)]
    fn verify_step_command_output_is_capped() -> TestResult {
        let output = run_bounded_verify_step_command(
            "sh",
            &["-c", "printf abcdef; printf ghijkl >&2"],
            Path::new("."),
            Duration::from_secs(1),
            3,
        )
        .map_err(|error| error.message)?;

        ensure(output.status.success(), "command succeeds")?;
        ensure_equal(&output.stdout, &"abc".to_string(), "stdout capped")?;
        ensure_equal(&output.stderr, &"ghi".to_string(), "stderr capped")
    }

    #[test]
    fn provenance_referent_verifies_file_span_and_hashes_selected_lines() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("notes.md"), "alpha\nbravo\ncharlie\n")
            .map_err(|error| error.to_string())?;

        let report = verify_provenance_referent(
            "file://notes.md#L2-3",
            &VerifyProvenanceReferentOptions {
                workspace_path: temp.path(),
                database: None,
                allow_network: false,
            },
        );

        ensure_equal(
            &report.status,
            &VerifyProvenanceReferentStatus::Verified,
            "file span verified",
        )?;
        ensure_equal(
            &report.reason.as_str(),
            &"file_span_present",
            "file span reason",
        )?;
        let expected_hash = format!("blake3:{}", blake3::hash(b"bravo\ncharlie\n").to_hex());
        ensure_equal(
            &report.referent_hash.as_deref(),
            &Some(expected_hash.as_str()),
            "selected span hash",
        )
    }

    #[test]
    fn provenance_referent_marks_missing_file_as_evidence_missing() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;

        let report = verify_provenance_referent(
            "file://missing.md#L1",
            &VerifyProvenanceReferentOptions {
                workspace_path: temp.path(),
                database: None,
                allow_network: false,
            },
        );

        ensure_equal(
            &report.status,
            &VerifyProvenanceReferentStatus::EvidenceMissing,
            "missing file status",
        )?;
        ensure_equal(
            &report.reason.as_str(),
            &"file_referent_missing",
            "missing file reason",
        )
    }

    #[test]
    #[cfg(unix)]
    fn provenance_referent_refuses_symlinked_file_referent_bd_3n8tt() -> TestResult {
        // bd-3n8tt: a file:// referent that traverses a symlink (leaf or parent)
        // must be refused, not followed — matching the memory-freshness contract.
        // Otherwise the verifier hashes/opens the symlink TARGET as if it were
        // the workspace evidence file.
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("outside.md"), "secret evidence\n")
            .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(
            temp.path().join("outside.md"),
            temp.path().join("linked.md"),
        )
        .map_err(|error| error.to_string())?;

        for uri in ["file://linked.md#L1", "file://linked.md"] {
            let report = verify_provenance_referent(
                uri,
                &VerifyProvenanceReferentOptions {
                    workspace_path: temp.path(),
                    database: None,
                    allow_network: false,
                },
            );
            ensure_equal(
                &report.status,
                &VerifyProvenanceReferentStatus::Unverifiable,
                "symlinked referent must not verify",
            )?;
            ensure_equal(
                &report.reason.as_str(),
                &"file_referent_symlinked",
                "symlinked referent reason",
            )?;
        }
        Ok(())
    }

    #[test]
    fn provenance_referent_marks_missing_file_span_as_drift() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("notes.md"), "alpha\n")
            .map_err(|error| error.to_string())?;

        let report = verify_provenance_referent(
            "file://notes.md#L2-3",
            &VerifyProvenanceReferentOptions {
                workspace_path: temp.path(),
                database: None,
                allow_network: false,
            },
        );

        ensure_equal(
            &report.status,
            &VerifyProvenanceReferentStatus::EvidenceDrift,
            "missing span status",
        )?;
        ensure(
            report.reason.contains("file_span_missing"),
            "missing span reason",
        )
    }

    #[test]
    fn provenance_referent_keeps_web_checks_opt_in() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;

        let report = verify_provenance_referent(
            "https://example.com/evidence",
            &VerifyProvenanceReferentOptions {
                workspace_path: temp.path(),
                database: None,
                allow_network: false,
            },
        );
        let json = report.data_json();

        ensure_equal(
            &report.status,
            &VerifyProvenanceReferentStatus::Unverifiable,
            "web status",
        )?;
        ensure_equal(
            &report.reason.as_str(),
            &"network_recheck_disabled",
            "web reason",
        )?;
        ensure_equal(
            &json
                .get("status")
                .and_then(serde_json::Value::as_str)
                .ok_or("json status")?,
            &"unverifiable",
            "json status",
        )
    }

    #[test]
    fn provenance_referent_classifies_reflection_scheme_as_first_party_unverifiable() -> TestResult
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;

        let report = verify_provenance_referent(
            "ee-reflect://reflect_req_0123456789abcdef",
            &VerifyProvenanceReferentOptions {
                workspace_path: temp.path(),
                database: None,
                allow_network: false,
            },
        );

        ensure_equal(
            &report.scheme.as_str(),
            &"ee-reflect",
            "reflection provenance scheme",
        )?;
        ensure_equal(
            &report.status,
            &VerifyProvenanceReferentStatus::Unverifiable,
            "reflection status",
        )?;
        ensure_equal(
            &report.reason.as_str(),
            &"reflection_recheck_requires_request_ledger",
            "reflection reason",
        )?;
        ensure(
            report
                .repair
                .as_deref()
                .is_some_and(|repair| repair.contains("reflection request ledger")),
            "reflection repair names ledger lookup",
        )
    }

    #[test]
    fn bounded_provenance_scan_checks_only_due_memories() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("due.md"), "due\n").map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("recent.md"), "recent\n")
            .map_err(|error| error.to_string())?;
        let connection = provenance_fixture_connection(temp.path())?;
        let workspace_id = "wsp_verify_provenance_fixture0";
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            "mem_00000000000000000000009101",
            "due",
            "file://due.md#L1",
        )?;
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            "mem_00000000000000000000009102",
            "recent",
            "file://recent.md#L1",
        )?;
        connection
            .execute_raw(
                "UPDATE memories SET provenance_verification_status = 'verified', \
                 provenance_verified_at = '2026-06-06T12:00:00Z' \
                 WHERE id = 'mem_00000000000000000000009102'",
            )
            .map_err(|error| error.to_string())?;
        let now = DateTime::parse_from_rfc3339("2026-06-07T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);

        let report = verify_bounded_provenance(VerifyProvenanceOptions {
            workspace_path: temp.path(),
            database: &connection,
            workspace_id,
            memory_id: None,
            stale_after_days: 7,
            limit: 10,
            allow_network: false,
            dry_run: true,
            now,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(&report.inspected_count, &2, "inspected")?;
        ensure_equal(&report.due_count, &1, "due")?;
        ensure_equal(&report.checked_count, &1, "checked")?;
        ensure_equal(&report.skipped_recent_count, &1, "recent skipped")?;
        ensure_equal(&report.verified_count, &1, "verified")?;
        ensure_equal(
            &report.records[0].memory_id.as_str(),
            &"mem_00000000000000000000009101",
            "checked memory",
        )?;
        ensure_equal(
            &report.data_json()["schema"],
            &serde_json::json!(VERIFY_PROVENANCE_REPORT_SCHEMA_V1),
            "report schema",
        )
    }

    #[test]
    fn bounded_provenance_scan_forces_explicit_memory_id() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("recent.md"), "recent\n")
            .map_err(|error| error.to_string())?;
        let connection = provenance_fixture_connection(temp.path())?;
        let workspace_id = "wsp_verify_provenance_fixture0";
        let memory_id = "mem_00000000000000000000009103";
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            memory_id,
            "recent",
            "file://recent.md#L1",
        )?;
        connection
            .execute_raw(
                "UPDATE memories SET provenance_verification_status = 'verified', \
                 provenance_verified_at = '2026-06-06T12:00:00Z' \
                 WHERE id = 'mem_00000000000000000000009103'",
            )
            .map_err(|error| error.to_string())?;
        let now = DateTime::parse_from_rfc3339("2026-06-07T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);

        let report = verify_bounded_provenance(VerifyProvenanceOptions {
            workspace_path: temp.path(),
            database: &connection,
            workspace_id,
            memory_id: Some(memory_id),
            stale_after_days: 7,
            limit: 10,
            allow_network: false,
            dry_run: true,
            now,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(&report.due_count, &1, "explicit memory due")?;
        ensure_equal(&report.checked_count, &1, "explicit memory checked")?;
        ensure_equal(&report.skipped_recent_count, &0, "no recent skip")?;
        ensure_equal(
            &report.records[0].referent.status,
            &VerifyProvenanceReferentStatus::Verified,
            "explicit referent verified",
        )
    }

    #[test]
    fn bounded_provenance_persists_status_and_respects_stale_window() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("fresh.md"), "fresh\n")
            .map_err(|error| error.to_string())?;
        let connection = provenance_fixture_connection(temp.path())?;
        let workspace_id = "wsp_verify_provenance_fixture0";
        let memory_id = "mem_00000000000000000000009108";
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            memory_id,
            "fresh",
            "file://fresh.md#L1",
        )?;
        let now = DateTime::parse_from_rfc3339("2026-06-07T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);

        let report = verify_bounded_provenance(VerifyProvenanceOptions {
            workspace_path: temp.path(),
            database: &connection,
            workspace_id,
            memory_id: Some(memory_id),
            stale_after_days: 7,
            limit: 10,
            allow_network: false,
            dry_run: false,
            now,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(&report.checked_count, &1, "checked")?;
        ensure_equal(&report.verified_count, &1, "verified")?;
        ensure_equal(&report.mutation_count, &1, "verification status mutation")?;
        let mutation = report.records[0]
            .mutation
            .as_ref()
            .ok_or_else(|| "expected verification status mutation".to_owned())?;
        ensure(mutation.persisted, "verification status persisted")?;
        ensure(
            mutation.verification_status_updated,
            "verification status updated",
        )?;
        ensure_equal(
            &mutation.previous_verification_status.as_str(),
            &"unverified",
            "previous verification status",
        )?;
        ensure_equal(
            &mutation.new_verification_status.as_str(),
            &PROVENANCE_STATUS_VERIFIED,
            "new verification status",
        )?;
        ensure_equal(
            &mutation.new_verified_at.as_deref(),
            &Some("2026-06-07T12:00:00+00:00"),
            "new verified_at",
        )?;
        let stored = connection
            .get_memory(memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory removed".to_owned())?;
        ensure_equal(
            &stored.provenance_verification_status.as_str(),
            &PROVENANCE_STATUS_VERIFIED,
            "stored verification status",
        )?;
        ensure_equal(
            &stored.provenance_verified_at.as_deref(),
            &Some("2026-06-07T12:00:00+00:00"),
            "stored verified_at",
        )?;
        ensure(
            stored
                .provenance_verification_note
                .as_deref()
                .is_some_and(|note| note.contains("file_span_present")),
            "stored note mentions referent reason",
        )?;

        let later = DateTime::parse_from_rfc3339("2026-06-08T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        let second_report = verify_bounded_provenance(VerifyProvenanceOptions {
            workspace_path: temp.path(),
            database: &connection,
            workspace_id,
            memory_id: None,
            stale_after_days: 7,
            limit: 10,
            allow_network: false,
            dry_run: false,
            now: later,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(&second_report.inspected_count, &1, "second inspected")?;
        ensure_equal(&second_report.due_count, &0, "second due")?;
        ensure_equal(&second_report.checked_count, &0, "second checked")?;
        ensure_equal(
            &second_report.skipped_recent_count,
            &1,
            "second skipped recent",
        )
    }

    #[test]
    fn bounded_provenance_marks_changed_file_span_as_drift() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("source.md"), "original cited evidence\n")
            .map_err(|error| error.to_string())?;
        let connection = provenance_fixture_connection(temp.path())?;
        let workspace_id = "wsp_verify_provenance_fixture0";
        let memory_id = "mem_00000000000000000000009104";
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            memory_id,
            "original cited evidence",
            "file://source.md#L1",
        )?;
        std::fs::write(temp.path().join("source.md"), "changed cited evidence\n")
            .map_err(|error| error.to_string())?;
        let now = DateTime::parse_from_rfc3339("2026-06-07T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);

        let report = verify_bounded_provenance(VerifyProvenanceOptions {
            workspace_path: temp.path(),
            database: &connection,
            workspace_id,
            memory_id: Some(memory_id),
            stale_after_days: 7,
            limit: 10,
            allow_network: false,
            dry_run: false,
            now,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(&report.checked_count, &1, "checked")?;
        ensure_equal(&report.evidence_drift_count, &1, "drift count")?;
        let referent = &report.records[0].referent;
        ensure_equal(
            &referent.status,
            &VerifyProvenanceReferentStatus::EvidenceDrift,
            "changed cited content status",
        )?;
        ensure_equal(
            &referent.reason.as_str(),
            &"file_referent_content_drift",
            "changed cited content reason",
        )?;
        ensure_equal(
            &report.data_json()["referents"][0]["action"],
            &serde_json::json!("demote_and_revalidate"),
            "flat referent action",
        )?;
        ensure_equal(&report.mutation_count, &1, "mutation count")?;
        ensure_equal(&report.trust_demotion_count, &1, "trust demotion count")?;
        ensure_equal(
            &report.curation_candidate_count,
            &1,
            "curation candidate count",
        )?;
        ensure_equal(&report.audit_count, &2, "audit count")?;
        let mutation = report.records[0]
            .mutation
            .as_ref()
            .ok_or_else(|| "expected persisted provenance mutation".to_owned())?;
        ensure(mutation.persisted, "mutation persisted")?;
        ensure(mutation.trust_class_updated, "trust class updated")?;
        ensure_equal(
            &mutation.previous_trust_class.as_str(),
            &"human_explicit",
            "previous trust class",
        )?;
        ensure_equal(
            &mutation.new_trust_class.as_str(),
            &"agent_assertion",
            "new trust class",
        )?;
        ensure_equal(
            &mutation.candidate_status.as_deref(),
            &Some("created"),
            "candidate created",
        )?;
        let candidate_id = mutation
            .candidate_id
            .as_deref()
            .ok_or_else(|| "candidate id missing".to_owned())?;
        let stored = connection
            .get_memory(memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory removed".to_owned())?;
        ensure(stored.tombstoned_at.is_none(), "memory not tombstoned")?;
        ensure_equal(
            &stored.trust_class.as_str(),
            &"agent_assertion",
            "stored trust class demoted",
        )?;
        ensure_equal(
            &stored.provenance_chain_hash,
            &Some(crate::db::compute_memory_provenance_chain_hash(&stored)),
            "stored provenance chain hash tracks demoted trust class",
        )?;
        ensure_equal(
            &stored.provenance_verification_status.as_str(),
            &PROVENANCE_STATUS_MISMATCH,
            "stored verification status mismatch",
        )?;
        ensure_equal(
            &stored.provenance_verified_at.as_deref(),
            &Some("2026-06-07T12:00:00+00:00"),
            "drift verified_at persisted",
        )?;
        let candidates = connection
            .list_curation_candidates(
                workspace_id,
                Some(CandidateType::Deprecate.as_str()),
                Some("pending"),
                Some(memory_id),
            )
            .map_err(|error| error.to_string())?;
        ensure_equal(&candidates.len(), &1, "one curation candidate")?;
        ensure_equal(
            &candidates[0].id.as_str(),
            &candidate_id,
            "candidate id persisted",
        )?;
        ensure(
            candidates[0].reason.contains("Provenance re-verification"),
            "candidate reason mentions provenance re-verification",
        )?;
        let memory_audits = connection
            .list_audit_by_target("memory", memory_id, Some(10))
            .map_err(|error| error.to_string())?;
        ensure(
            memory_audits
                .iter()
                .any(|entry| entry.action == audit_actions::TRUST_CLASS_TRANSITION),
            "trust transition audit persisted",
        )?;
        let candidate_audits = connection
            .list_audit_by_target("curation_candidate", candidate_id, Some(10))
            .map_err(|error| error.to_string())?;
        ensure(
            candidate_audits
                .iter()
                .any(|entry| entry.action == audit_actions::CURATION_CANDIDATE_CREATE),
            "candidate create audit persisted",
        )?;

        let second_now = DateTime::parse_from_rfc3339("2026-06-07T12:05:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        let second_report = verify_bounded_provenance(VerifyProvenanceOptions {
            workspace_path: temp.path(),
            database: &connection,
            workspace_id,
            memory_id: Some(memory_id),
            stale_after_days: 7,
            limit: 10,
            allow_network: false,
            dry_run: false,
            now: second_now,
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(&second_report.mutation_count, &1, "repeat mutation report")?;
        ensure_equal(&second_report.audit_count, &0, "repeat audit count")?;
        ensure_equal(
            &second_report.trust_demotion_count,
            &0,
            "repeat trust demotion count",
        )?;
        ensure_equal(
            &second_report.curation_candidate_count,
            &0,
            "repeat curation candidate count",
        )?;
        let second_mutation = second_report.records[0]
            .mutation
            .as_ref()
            .ok_or_else(|| "expected repeat provenance mutation report".to_owned())?;
        ensure(
            second_mutation.persisted,
            "repeat mutation persists verification status",
        )?;
        ensure(
            second_mutation.verification_status_updated,
            "repeat mutation updates verification status",
        )?;
        ensure_equal(
            &second_mutation.candidate_status.as_deref(),
            &Some("already_exists"),
            "repeat candidate status",
        )
    }

    #[test]
    fn bounded_provenance_does_not_mutate_tombstoned_memory() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(temp.path().join("source.md"), "original cited evidence\n")
            .map_err(|error| error.to_string())?;
        let connection = provenance_fixture_connection(temp.path())?;
        let workspace_id = "wsp_verify_provenance_fixture0";
        let memory_id = "mem_00000000000000000000009105";
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            memory_id,
            "original cited evidence",
            "file://source.md#L1",
        )?;
        ensure(
            connection
                .tombstone_memory(memory_id)
                .map_err(|error| error.to_string())?,
            "fixture memory tombstoned",
        )?;
        std::fs::write(temp.path().join("source.md"), "changed cited evidence\n")
            .map_err(|error| error.to_string())?;
        let now = DateTime::parse_from_rfc3339("2026-06-07T12:10:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);

        let report = verify_bounded_provenance(VerifyProvenanceOptions {
            workspace_path: temp.path(),
            database: &connection,
            workspace_id,
            memory_id: Some(memory_id),
            stale_after_days: 7,
            limit: 10,
            allow_network: false,
            dry_run: false,
            now,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.checked_count,
            &1,
            "explicit tombstoned memory checked",
        )?;
        ensure_equal(
            &report.evidence_drift_count,
            &1,
            "tombstoned referent still classified",
        )?;
        ensure_equal(&report.mutation_count, &0, "no tombstoned mutation")?;
        ensure_equal(&report.audit_count, &0, "no tombstoned audit")?;
        ensure(
            report.records[0].mutation.is_none(),
            "tombstoned memory has no mutation report",
        )?;
        let candidates = connection
            .list_curation_candidates(
                workspace_id,
                Some(CandidateType::Deprecate.as_str()),
                Some("pending"),
                Some(memory_id),
            )
            .map_err(|error| error.to_string())?;
        ensure(
            candidates.is_empty(),
            "tombstoned memory does not get a curation candidate",
        )?;
        let audits = connection
            .list_audit_by_target("memory", memory_id, Some(10))
            .map_err(|error| error.to_string())?;
        ensure(
            audits
                .iter()
                .all(|entry| entry.action != audit_actions::TRUST_CLASS_TRANSITION),
            "tombstoned memory does not get a trust transition audit",
        )
    }

    #[test]
    fn provenance_reverify_rechecks_tombstone_state_inside_write_transaction() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let connection = provenance_fixture_connection(temp.path())?;
        let workspace_id = "wsp_verify_provenance_fixture0";
        let memory_id = "mem_00000000000000000000009106";
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            memory_id,
            "original cited evidence",
            "file://source.md#L1",
        )?;
        let stale_snapshot = connection
            .get_memory(memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "fixture memory missing".to_owned())?;
        ensure(
            stale_snapshot.tombstoned_at.is_none(),
            "stale snapshot starts live",
        )?;
        ensure(
            connection
                .tombstone_memory(memory_id)
                .map_err(|error| error.to_string())?,
            "fixture memory tombstoned after snapshot",
        )?;
        let referent = provenance_referent_report(
            "file://source.md#L1",
            "file",
            VerifyProvenanceReferentStatus::EvidenceDrift,
            "file_referent_content_drift".to_owned(),
            None,
            None,
        );

        let mutation = apply_provenance_reverify_action(
            &connection,
            workspace_id,
            &stale_snapshot,
            &referent,
            "2026-06-07T12:15:00Z",
            false,
        )
        .map_err(|error| error.to_string())?;

        ensure(
            mutation.is_none(),
            "stale snapshot does not mutate tombstoned memory",
        )?;
        let candidates = connection
            .list_curation_candidates(
                workspace_id,
                Some(CandidateType::Deprecate.as_str()),
                Some("pending"),
                Some(memory_id),
            )
            .map_err(|error| error.to_string())?;
        ensure(
            candidates.is_empty(),
            "stale snapshot does not create curation candidate",
        )?;
        let audits = connection
            .list_audit_by_target("memory", memory_id, Some(10))
            .map_err(|error| error.to_string())?;
        ensure(
            audits
                .iter()
                .all(|entry| entry.action != audit_actions::TRUST_CLASS_TRANSITION),
            "stale snapshot does not create trust transition audit",
        )
    }

    #[test]
    fn provenance_reverify_dry_run_rechecks_tombstone_state_before_planning() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let connection = provenance_fixture_connection(temp.path())?;
        let workspace_id = "wsp_verify_provenance_fixture0";
        let memory_id = "mem_00000000000000000000009107";
        insert_provenance_fixture_memory_with_content(
            &connection,
            workspace_id,
            memory_id,
            "original cited evidence",
            "file://source.md#L1",
        )?;
        let stale_snapshot = connection
            .get_memory(memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "fixture memory missing".to_owned())?;
        ensure(
            connection
                .tombstone_memory(memory_id)
                .map_err(|error| error.to_string())?,
            "fixture memory tombstoned after snapshot",
        )?;
        let referent = provenance_referent_report(
            "file://source.md#L1",
            "file",
            VerifyProvenanceReferentStatus::EvidenceDrift,
            "file_referent_content_drift".to_owned(),
            None,
            None,
        );

        let mutation = apply_provenance_reverify_action(
            &connection,
            workspace_id,
            &stale_snapshot,
            &referent,
            "2026-06-07T12:20:00Z",
            true,
        )
        .map_err(|error| error.to_string())?;

        ensure(
            mutation.is_none(),
            "dry run does not plan tombstoned memory mutation",
        )
    }

    fn provenance_fixture_connection(workspace_path: &Path) -> Result<DbConnection, String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_verify_provenance_fixture0",
                &crate::db::CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().to_string(),
                    name: Some("verify provenance fixture".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    fn insert_provenance_fixture_memory_with_content(
        connection: &DbConnection,
        workspace_id: &str,
        memory_id: &str,
        content: &str,
        provenance_uri: &str,
    ) -> Result<(), String> {
        connection
            .insert_memory(
                memory_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "episodic".to_owned(),
                    kind: "note".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: Some(provenance_uri.to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    #[test]
    fn artifact_policy_has_expected_rules() -> TestResult {
        ensure(ARTIFACT_RULES.len() >= 4, "at least 4 rules")?;

        let patterns: Vec<&str> = ARTIFACT_RULES.iter().map(|r| r.pattern).collect();
        ensure(patterns.contains(&"target/"), "has target/")?;
        ensure(patterns.contains(&".ee/"), "has .ee/")?;
        ensure(patterns.contains(&"tests/fixtures/"), "has tests/fixtures/")
    }

    #[test]
    fn target_is_not_versioned() -> TestResult {
        let target_rule = ARTIFACT_RULES
            .iter()
            .find(|r| r.pattern == "target/")
            .ok_or("target rule not found")?;
        ensure(!target_rule.versioned, "target should not be versioned")?;
        ensure(target_rule.ci_cached, "target should be CI cached")
    }

    #[test]
    fn test_fixtures_are_versioned() -> TestResult {
        let fixtures_rule = ARTIFACT_RULES
            .iter()
            .find(|r| r.pattern == "tests/fixtures/")
            .ok_or("fixtures rule not found")?;
        ensure(fixtures_rule.versioned, "fixtures should be versioned")
    }

    #[test]
    fn gitignore_patterns_excludes_versioned() -> TestResult {
        let patterns = gitignore_patterns();
        ensure(
            !patterns.contains(&"tests/fixtures/"),
            "fixtures not ignored",
        )?;
        ensure(patterns.contains(&"target/"), "target is ignored")
    }

    #[test]
    fn should_gitignore_detects_target() -> TestResult {
        ensure(
            should_gitignore(Path::new("target/debug/ee")),
            "target/debug/ee should be ignored",
        )
    }

    #[test]
    fn should_gitignore_allows_fixtures() -> TestResult {
        ensure(
            !should_gitignore(Path::new("tests/fixtures/agent_detect/codex")),
            "fixtures should not be ignored",
        )
    }

    #[test]
    fn dry_run_skips_all_steps() -> TestResult {
        let options = VerifyOptions {
            dry_run: true,
            ..Default::default()
        };
        let report = run_verification(&options);

        ensure_equal(&report.skipped_count, &4, "all skipped")?;
        ensure(report.all_passed, "dry run passes")
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure_equal(
            &VERIFY_REPORT_SCHEMA_V1,
            &"ee.verify.report.v1",
            "verify schema",
        )?;
        ensure_equal(
            &ARTIFACT_POLICY_SCHEMA_V1,
            &"ee.artifact_policy.v1",
            "artifact schema",
        )
    }

    #[test]
    fn verify_report_json_has_required_fields() -> TestResult {
        let options = VerifyOptions {
            dry_run: true,
            ..Default::default()
        };
        let report = run_verification(&options);
        let json = report.data_json();

        ensure(json.get("command").is_some(), "has command")?;
        ensure(json.get("allPassed").is_some(), "has allPassed")?;
        ensure(json.get("steps").is_some(), "has steps")
    }

    #[test]
    fn verification_posture_counts_rch_advisories_and_recovery_actions() -> TestResult {
        let now = DateTime::parse_from_rfc3339("2026-05-13T01:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        let mut records = crate::models::sample_verification_evidence_records();
        let mut reusable = records
            .iter()
            .find(|record| record.offload.required_remote)
            .ok_or("sample remote record exists")?
            .clone();
        reusable.verification_id = "ver_reusable_remote_0000001".to_owned();
        reusable.status = VerificationStatus::Passed;
        reusable.exit_code = Some(0);
        reusable.started_at = Some("2026-05-13T00:10:00Z".to_owned());
        reusable.finished_at = Some("2026-05-13T00:10:42Z".to_owned());
        reusable.output_summary = crate::models::VerificationOutputSummary::empty();
        reusable.artifacts = vec![crate::models::VerificationArtifactRef::new(
            "remote_artifact_verification_hash",
            "remote_artifact_attestation",
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        )];
        records.push(reusable);

        let report = VerificationPostureReport::from_records(now, records.as_slice());

        ensure_equal(&report.status.as_str(), &"blocked", "aggregate status")?;
        ensure_equal(&report.record_count, &6, "record count")?;
        ensure_equal(
            &report.advisory_counts.remote_success,
            &1,
            "remote success count",
        )?;
        ensure_equal(
            &report.recent_reusable_run_count,
            &1,
            "recent reusable run count",
        )?;
        ensure_equal(
            &report.advisory_counts.remote_failed,
            &1,
            "remote failed count",
        )?;
        ensure_equal(
            &report.advisory_counts.local_disallowed,
            &1,
            "local disallowed count",
        )?;
        ensure_equal(
            &report.advisory_counts.topology_blocked,
            &1,
            "topology blocked count",
        )?;
        ensure(
            report.recovery_actions.iter().any(|action| {
                action.kind == "inspect_topology_diagnostic"
                    && action.related_bead_id.as_deref() == Some("bd-1zb7k.13")
            }),
            "topology recovery action links bd-1zb7k.13",
        )
    }

    #[test]
    fn artifact_policy_report_json_has_rules() -> TestResult {
        let report = artifact_policy_report();
        let json = report.data_json();

        ensure(json.get("rules").is_some(), "has rules")?;
        let rules = json.get("rules").and_then(|v| v.as_array());
        let Some(rules) = rules else {
            return Err("rules is array".to_string());
        };
        ensure(rules.len() >= 4, "at least 4 rules")
    }

    #[test]
    fn record_verification_evidence_writes_audit_ledger() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let evidence = crate::models::sample_verification_evidence_records()
            .into_iter()
            .next()
            .ok_or("sample evidence exists")?;
        let report = record_verification_evidence(VerificationRecordOptions {
            database_path: &database_path,
            workspace_path: temp.path(),
            target_type: "memory",
            target_id: "mem_verifyledger0000000000001",
            actor: Some("codex:test"),
            evidence: evidence.clone(),
        })
        .map_err(|error| error.to_string())?;

        ensure(report.persisted, "record report is persisted")?;
        ensure(!report.replayed, "first record is not replayed")?;
        ensure(
            report.content_hash.starts_with("blake3:"),
            "content hash is blake3",
        )?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let audit_entries = connection
            .list_audit_by_action(audit_actions::VERIFICATION_INGEST, None)
            .map_err(|error| error.to_string())?;
        ensure_equal(&audit_entries.len(), &1, "one ingest audit row")?;
        let details: serde_json::Value = serde_json::from_str(
            audit_entries[0]
                .details
                .as_deref()
                .ok_or("ingest audit row has details")?,
        )
        .map_err(|error| error.to_string())?;
        ensure_equal(
            &details
                .get("schema")
                .and_then(serde_json::Value::as_str)
                .ok_or("ledger detail schema")?,
            &VERIFICATION_LEDGER_ENTRY_SCHEMA_V1,
            "ledger detail schema",
        )?;
        ensure_equal(
            &details
                .get("contentHash")
                .and_then(serde_json::Value::as_str)
                .ok_or("ledger detail content hash")?,
            &report.content_hash.as_str(),
            "ledger detail content hash",
        )?;
        ensure_equal(
            &report.authority,
            &"caller_authored",
            "public report authority",
        )?;
        ensure(
            !report.pass_authority_validated,
            "caller-authored report does not claim validated pass authority",
        )?;
        let records = verification_records_for_target(
            &connection,
            "memory",
            "mem_verifyledger0000000000001",
        )?;
        ensure_equal(&records.len(), &1, "one linked verification record")?;
        ensure_equal(
            &records[0].verification_id,
            &evidence.verification_id,
            "verification id",
        )?;
        let data = report.data_json();
        ensure_equal(
            &data
                .get("beadsSummary")
                .and_then(serde_json::Value::as_str)
                .ok_or("record report has beads summary")?,
            &verification_evidence_beads_summary(&evidence).as_str(),
            "beads summary",
        )?;

        let replay = record_verification_evidence(VerificationRecordOptions {
            database_path: &database_path,
            workspace_path: temp.path(),
            target_type: "memory",
            target_id: "mem_verifyledger0000000000001",
            actor: Some("codex:test"),
            evidence,
        })
        .map_err(|error| error.to_string())?;
        ensure(!replay.persisted, "replay does not persist a duplicate")?;
        ensure(replay.replayed, "replay is flagged")?;
        ensure_equal(&replay.audit_id, &report.audit_id, "replay audit id")?;
        ensure_equal(
            &replay.degradations,
            &vec!["degraded.verification_idempotent_replay".to_owned()],
            "replay degradation",
        )?;
        let audit_entries = connection
            .list_audit_by_action(audit_actions::VERIFICATION_INGEST, None)
            .map_err(|error| error.to_string())?;
        ensure_equal(&audit_entries.len(), &1, "idempotent replay keeps one row")
    }

    #[test]
    fn closure_guidance_consumes_audit_ledger_and_rejects_fallback() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let evidence = crate::models::sample_verification_evidence_records()
            .into_iter()
            .find(|record| record.status == crate::models::VerificationStatus::FallbackDetected)
            .ok_or("sample fallback evidence exists")?;
        let ingest = record_verification_evidence(VerificationRecordOptions {
            database_path: &database_path,
            workspace_path: temp.path(),
            target_type: "memory",
            target_id: "mem_verifyledger0000000000002",
            actor: Some("codex:test"),
            evidence,
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(
            &ingest.authority,
            &"caller_authored",
            "caller-authored ingest authority",
        )?;
        ensure(
            !ingest.pass_authority_validated,
            "caller-authored ingest remains advisory",
        )?;

        let report =
            verification_closure_guidance_from_ledger(&VerificationClosureGuidanceOptions {
                database_path: &database_path,
                bead_id: Some("bd-example"),
                requirements: vec![VerificationGateRequirement::new(
                    "cargo test producer",
                    Some("cargo test --lib producer"),
                    true,
                )],
            })
            .map_err(|error| error.to_string())?;

        ensure(
            !report.guidance.can_close,
            "fallback evidence rejects closure",
        )?;
        ensure_equal(&report.evidence_count, &1, "one evidence record")?;
        ensure(
            report.guidance.rejected_reasons[0].contains("local fallback"),
            "rejection explains fallback",
        )
    }

    #[test]
    fn caller_authored_pass_cannot_close_a_verification_gate() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let evidence = crate::models::sample_verification_evidence_records()
            .into_iter()
            .find(|record| record.is_authoritative_pass())
            .ok_or("sample pass evidence exists")?;
        let gate_name = evidence.gate_name.clone();
        let command = evidence.command.clone();

        record_verification_evidence(VerificationRecordOptions {
            database_path: &database_path,
            workspace_path: temp.path(),
            target_type: "memory",
            target_id: "mem_caller_authored_pass",
            actor: Some("codex:test"),
            evidence,
        })
        .map_err(|error| error.to_string())?;

        let report =
            verification_closure_guidance_from_ledger(&VerificationClosureGuidanceOptions {
                database_path: &database_path,
                bead_id: None,
                requirements: vec![VerificationGateRequirement::new(
                    &gate_name,
                    Some(&command),
                    false,
                )],
            })
            .map_err(|error| error.to_string())?;

        ensure(
            !report.guidance.can_close,
            "caller-authored pass stays advisory",
        )?;
        ensure_equal(
            &report.guidance.assessments[0].matched_status,
            &Some(VerificationStatus::Unknown),
            "caller-authored pass is authority-bounded on read",
        )
    }

    #[test]
    fn validated_rch_pass_can_close_a_verification_gate() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let proof = serde_json::json!({
            "schema": RCH_VERIFY_SCHEMA_V1,
            "command_text": "cargo test --lib verification",
            "command_hash": "sha256:validated-rch-pass",
            "command_kind": "cargo_test",
            "status": "remote_pass",
            "exit_code": 0,
            "worker_id": "vmi123",
            "remote_required": true
        });
        let evidence = crate::models::verification_evidence_record_from_rch_verify(&proof)
            .map_err(|error| error.to_string())?;

        let ingest = record_validated_verification_evidence(
            VerificationRecordOptions {
                database_path: &database_path,
                workspace_path: temp.path(),
                target_type: "memory",
                target_id: "mem_validated_rch_pass",
                actor: Some("codex:test"),
                evidence,
            },
            VerificationEvidenceAuthority::ValidatedRchVerify,
        )
        .map_err(|error| error.to_string())?;
        ensure_equal(
            &ingest.authority,
            &"validated_rch_verify",
            "validated RCH ingest authority",
        )?;
        ensure(
            ingest.pass_authority_validated,
            "validated RCH ingest advertises pass authority",
        )?;

        let report =
            verification_closure_guidance_from_ledger(&VerificationClosureGuidanceOptions {
                database_path: &database_path,
                bead_id: None,
                requirements: vec![VerificationGateRequirement::new(
                    "cargo_test",
                    Some("cargo test --lib verification"),
                    true,
                )],
            })
            .map_err(|error| error.to_string())?;

        ensure(
            report.guidance.can_close,
            "validated RCH pass retains closure authority",
        )?;
        ensure_equal(
            &report.guidance.assessments[0].matched_status,
            &Some(VerificationStatus::Passed),
            "validated RCH pass status",
        )
    }

    #[test]
    fn provenance_reverify_action_enforces_no_silent_mutation() {
        use ProvenanceReverifyAction as Action;
        use VerifyProvenanceReferentStatus as Status;

        assert_eq!(Status::Verified.reverify_action(), Action::None);
        // Conservatism: an unverifiable referent is advisory only, never demoted.
        assert_eq!(Status::Unverifiable.reverify_action(), Action::Advisory);
        assert!(!Status::Unverifiable.reverify_action().demotes());
        // Gone / drifted -> audited demotion + revalidation candidate (never removal).
        assert_eq!(
            Status::EvidenceMissing.reverify_action(),
            Action::DemoteAndRevalidate
        );
        assert_eq!(
            Status::EvidenceDrift.reverify_action(),
            Action::DemoteAndRevalidate
        );
        assert!(Status::EvidenceMissing.reverify_action().demotes());
    }

    #[test]
    fn provenance_reverify_demotes_peer_attestation_when_origin_evidence_fails() {
        assert_eq!(
            provenance_reverify_demoted_trust_class("peer_human_attested"),
            Some("agent_assertion")
        );
    }
}
