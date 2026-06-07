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
use crate::db::{
    CreateAuditInput, CreateWorkspaceInput, DbConnection, WorkspaceScopeFields, audit_actions,
    generate_audit_id,
};
use crate::models::{
    DomainError, LineSpan, ProducerMetadata, ProvenanceUri, RESPONSE_SCHEMA_V2,
    VERIFICATION_EVIDENCE_SCHEMA_V1, VerificationClosureGuidance, VerificationEvidenceRecord,
    VerificationGateRequirement, VerificationStatus, rch_cargo_closure_requirements,
    verification_closure_guidance, verification_evidence_beads_summary,
};

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for verification reports.
pub const VERIFY_REPORT_SCHEMA_V1: &str = "ee.verify.report.v1";
pub const VERIFY_RECORD_REPORT_SCHEMA_V1: &str = "ee.verify.record_report.v1";
pub const VERIFY_CLOSURE_GUIDANCE_REPORT_SCHEMA_V1: &str = "ee.verify.closure_guidance_report.v1";
pub const VERIFY_PROVENANCE_REFERENT_SCHEMA_V1: &str = "ee.verify.provenance_referent.v1";
pub const VERIFICATION_LEDGER_ENTRY_SCHEMA_V1: &str = "ee.verification.ledger_entry.v1";
pub const VERIFICATION_POSTURE_SCHEMA_V1: &str = "ee.verification.posture.v1";
const LEGACY_VERIFICATION_RECORD_ACTION: &str = "verification.record";
const VERIFICATION_POSTURE_WINDOW_HOURS: u32 = 24;
const VERIFY_STEP_TIMEOUT: Duration = Duration::from_secs(60);
const VERIFY_STEP_OUTPUT_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const VERIFY_PROVENANCE_FILE_SCAN_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const VERIFY_PROVENANCE_GIT_TIMEOUT: Duration = Duration::from_secs(5);

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
            "{verb}\n  ID: {}\n  Audit: {}\n  Content hash: {}\n  Target: {}:{}\n  Status: {}\n  Beads summary: {}\n",
            self.evidence.verification_id,
            self.audit_id,
            self.content_hash,
            self.target_type,
            self.target_id,
            self.evidence.status.as_str(),
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
            "repair": self.repair,
        })
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
    validate_verification_record(&options.evidence)?;
    let content_hash =
        verification_evidence_content_hash(&options.evidence).map_err(|message| {
            DomainError::Storage {
                message,
                repair: Some("inspect the verification evidence JSON and retry".to_owned()),
            }
        })?;

    let connection = open_verification_database(options.database_path)?;
    let workspace_id = ensure_verification_workspace(&connection, options.workspace_path)?;
    if let Some(existing) = find_existing_verification_ingest(
        &connection,
        &content_hash,
        options.target_type,
        options.target_id,
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
            degradations: vec!["degraded.verification_idempotent_replay".to_owned()],
            evidence: existing.record,
        });
    }

    let audit_id = generate_audit_id();
    let details = VerificationAuditDetails::new(content_hash.clone(), &options.evidence);
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
        degradations: Vec::new(),
        evidence: options.evidence,
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
    let workspace_path = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_key = workspace_path.to_string_lossy().into_owned();
    if let Some(existing) = connection
        .get_workspace_by_path(&workspace_key)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query verification workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
    {
        return Ok(existing.id);
    }

    let workspace_id = super::workspace::stable_workspace_id(&workspace_path);
    let input = CreateWorkspaceInput {
        path: workspace_key,
        name: workspace_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned),
    };
    connection
        .upsert_workspace_with_scope(&workspace_id, &input, &WorkspaceScopeFields::standalone())
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to ensure workspace for verification ledger: {error}"),
            repair: Some("ee init --workspace .".to_owned()),
        })?;
    if let Some(existing) =
        connection
            .get_workspace(&workspace_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query ensured verification workspace: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?
    {
        return Ok(existing.id);
    }
    if let Some(existing) = connection
        .get_workspace_by_path(&workspace_path.to_string_lossy())
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query ensured verification workspace path: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
    {
        return Ok(existing.id);
    }

    Err(DomainError::Storage {
        message:
            "Failed to ensure workspace for verification ledger: workspace row was not inserted"
                .to_owned(),
        repair: Some("ee init --workspace .".to_owned()),
    })
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

#[derive(Clone, Debug)]
struct ParsedVerificationAuditEntry {
    audit_id: String,
    content_hash: String,
    target_type: Option<String>,
    target_id: Option<String>,
    record: VerificationEvidenceRecord,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerificationAuditDetails {
    schema: String,
    content_hash: String,
    producer: ProducerMetadata,
    status: VerificationStatus,
    evidence: VerificationEvidenceRecord,
}

impl VerificationAuditDetails {
    fn new(content_hash: String, evidence: &VerificationEvidenceRecord) -> Self {
        Self {
            schema: VERIFICATION_LEDGER_ENTRY_SCHEMA_V1.to_owned(),
            content_hash,
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
        .map(|entry| entry.record)
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
        let (record, content_hash) =
            match serde_json::from_str::<VerificationAuditDetails>(&details) {
                Ok(details) if details.schema == VERIFICATION_LEDGER_ENTRY_SCHEMA_V1 => {
                    (details.evidence, details.content_hash)
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
                    (record, content_hash)
                }
            };
        records.push(ParsedVerificationAuditEntry {
            audit_id: entry.id,
            content_hash,
            target_type: entry.target_type,
            target_id: entry.target_id,
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
        .any(|artifact| artifact.kind.contains("manifest") || artifact.path.contains("manifest"))
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

    let stdout = output_bytes_to_string(stdout_thread.join().unwrap_or_default());
    let stderr = output_bytes_to_string(stderr_thread.join().unwrap_or_default());

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
            "target/verify/artifact_manifest.json",
            "artifact_manifest",
            Some("blake3:manifest"),
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
        record_verification_evidence(VerificationRecordOptions {
            database_path: &database_path,
            workspace_path: temp.path(),
            target_type: "memory",
            target_id: "mem_verifyledger0000000000002",
            actor: Some("codex:test"),
            evidence,
        })
        .map_err(|error| error.to_string())?;

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
}
