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
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::build_info;
use crate::db::{
    CreateAuditInput, CreateWorkspaceInput, DbConnection, WorkspaceScopeFields, audit_actions,
    generate_audit_id,
};
use crate::models::{
    DomainError, ProducerMetadata, RESPONSE_SCHEMA_V1, VERIFICATION_EVIDENCE_SCHEMA_V1,
    VerificationClosureGuidance, VerificationEvidenceRecord, VerificationGateRequirement,
    VerificationStatus, rch_cargo_closure_requirements, verification_closure_guidance,
};

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for verification reports.
pub const VERIFY_REPORT_SCHEMA_V1: &str = "ee.verify.report.v1";
pub const VERIFY_RECORD_REPORT_SCHEMA_V1: &str = "ee.verify.record_report.v1";
pub const VERIFY_CLOSURE_GUIDANCE_REPORT_SCHEMA_V1: &str = "ee.verify.closure_guidance_report.v1";
pub const VERIFICATION_LEDGER_ENTRY_SCHEMA_V1: &str = "ee.verification.ledger_entry.v1";
const LEGACY_VERIFICATION_RECORD_ACTION: &str = "verification.record";

/// Schema for artifact policy.
pub const ARTIFACT_POLICY_SCHEMA_V1: &str = "ee.artifact_policy.v1";

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
            duration_ms: duration.as_millis() as u64,
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
            duration_ms: duration.as_millis() as u64,
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
            "{verb}\n  ID: {}\n  Audit: {}\n  Content hash: {}\n  Target: {}:{}\n  Status: {}\n",
            self.evidence.verification_id,
            self.audit_id,
            self.content_hash,
            self.target_type,
            self.target_id,
            self.evidence.status.as_str()
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
            "verificationEvidence": self.evidence,
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
pub fn default_rch_cargo_closure_requirements() -> Vec<VerificationGateRequirement> {
    rch_cargo_closure_requirements()
}

#[must_use]
pub fn verification_response_json(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V1,
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
        total_duration_ms: total_duration.as_millis() as u64,
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

    let result = Command::new(program)
        .args(&args)
        .current_dir(workspace_path)
        .output();

    let duration = start.elapsed();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code();

            if output.status.success() {
                StepResult::passed(step, duration, stdout, stderr)
            } else {
                StepResult::failed(step, duration, stdout, stderr, exit_code)
            }
        }
        Err(e) => StepResult::failed(
            step,
            duration,
            String::new(),
            format!("Failed to execute command: {e}"),
            None,
        ),
    }
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
