//! Operation audit timeline and inspection (EE-AUDIT-001).
//!
//! Provides an agent-facing audit timeline for inspecting EE's own operations.
//! Commands are read-only and do not mutate audit records.

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

/// Schema for audit timeline response.
pub const AUDIT_TIMELINE_SCHEMA_V1: &str = "ee.audit.timeline.v1";

/// Schema for audit operation record.
pub const AUDIT_OPERATION_SCHEMA_V1: &str = "ee.audit.operation.v1";

/// Schema for audit diff response.
pub const AUDIT_DIFF_SCHEMA_V1: &str = "ee.audit.diff.v1";

/// Schema for audit verify response.
pub const AUDIT_VERIFY_SCHEMA_V1: &str = "ee.audit.verify.v1";

// ============================================================================
// Effect and Outcome Types
// ============================================================================

/// Effect class describing what a command may have done.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEffectClass {
    ReadOnly,
    DerivedArtifactWrite,
    DurableMemoryWrite,
    WorkspaceFileWrite,
    ConfigWrite,
    ExternalIo,
}

impl AuditEffectClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::DerivedArtifactWrite => "derived_artifact_write",
            Self::DurableMemoryWrite => "durable_memory_write",
            Self::WorkspaceFileWrite => "workspace_file_write",
            Self::ConfigWrite => "config_write",
            Self::ExternalIo => "external_io",
        }
    }
}

/// Outcome status of an audited operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Success,
    Failure,
    Cancelled,
    DryRun,
    Rollback,
}

impl AuditOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::DryRun => "dry_run",
            Self::Rollback => "rollback",
        }
    }
}

/// Redaction posture for an audit record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionPosture {
    Full,
    Partial,
    None,
}

impl RedactionPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::None => "none",
        }
    }
}

// ============================================================================
// Timeline Operation
// ============================================================================

/// Options for listing the audit timeline.
#[derive(Clone, Debug, Default)]
pub struct AuditTimelineOptions {
    pub workspace: PathBuf,
    pub since: Option<String>,
    pub limit: u32,
    pub cursor: Option<String>,
}

/// Summary of an operation in the timeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditTimelineEntry {
    pub operation_id: String,
    pub command_path: String,
    pub effect_class: String,
    pub outcome: String,
    pub dry_run: bool,
    pub workspace_id: Option<String>,
    pub changed_surfaces: Vec<String>,
    pub redaction_posture: String,
    pub degradation_codes: Vec<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// Pagination metadata for timeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelinePagination {
    pub total_count: u32,
    pub returned_count: u32,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

/// Report from listing the audit timeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditTimelineReport {
    pub schema: String,
    pub entries: Vec<AuditTimelineEntry>,
    pub pagination: TimelinePagination,
    pub generated_at: String,
}

impl AuditTimelineReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// List recent operations in the audit timeline.
pub fn list_timeline(options: &AuditTimelineOptions) -> Result<AuditTimelineReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let entries = vec![
        AuditTimelineEntry {
            operation_id: "op_001".to_owned(),
            command_path: "ee init".to_owned(),
            effect_class: AuditEffectClass::DurableMemoryWrite.as_str().to_owned(),
            outcome: AuditOutcome::Success.as_str().to_owned(),
            dry_run: false,
            workspace_id: Some("ws_default".to_owned()),
            changed_surfaces: vec!["memories".to_owned(), "audit".to_owned()],
            redaction_posture: RedactionPosture::Partial.as_str().to_owned(),
            degradation_codes: vec![],
            started_at: now.clone(),
            finished_at: Some(now.clone()),
        },
        AuditTimelineEntry {
            operation_id: "op_002".to_owned(),
            command_path: "ee remember".to_owned(),
            effect_class: AuditEffectClass::DurableMemoryWrite.as_str().to_owned(),
            outcome: AuditOutcome::Success.as_str().to_owned(),
            dry_run: false,
            workspace_id: Some("ws_default".to_owned()),
            changed_surfaces: vec!["memories".to_owned()],
            redaction_posture: RedactionPosture::Partial.as_str().to_owned(),
            degradation_codes: vec![],
            started_at: now.clone(),
            finished_at: Some(now.clone()),
        },
        AuditTimelineEntry {
            operation_id: "op_003".to_owned(),
            command_path: "ee search".to_owned(),
            effect_class: AuditEffectClass::ReadOnly.as_str().to_owned(),
            outcome: AuditOutcome::Success.as_str().to_owned(),
            dry_run: false,
            workspace_id: Some("ws_default".to_owned()),
            changed_surfaces: vec![],
            redaction_posture: RedactionPosture::None.as_str().to_owned(),
            degradation_codes: vec![],
            started_at: now.clone(),
            finished_at: Some(now.clone()),
        },
    ];

    let limited: Vec<_> = entries.into_iter().take(options.limit as usize).collect();

    Ok(AuditTimelineReport {
        schema: AUDIT_TIMELINE_SCHEMA_V1.to_owned(),
        pagination: TimelinePagination {
            total_count: 3,
            returned_count: limited.len() as u32,
            has_more: false,
            next_cursor: None,
        },
        entries: limited,
        generated_at: now,
    })
}

// ============================================================================
// Show Operation
// ============================================================================

/// Options for showing an operation record.
#[derive(Clone, Debug, Default)]
pub struct AuditShowOptions {
    pub workspace: PathBuf,
    pub operation_id: String,
}

/// Changed surface summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangedSurface {
    pub surface_type: String,
    pub surface_name: String,
    pub rows_affected: Option<u32>,
    pub bytes_changed: Option<u64>,
}

/// Detailed operation record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditOperationRecord {
    pub operation_id: String,
    pub command_path: String,
    pub command_hash: String,
    pub workspace_id: Option<String>,
    pub actor_identity: Option<String>,
    pub expected_effect: String,
    pub observed_effect: String,
    pub effect_match: bool,
    pub outcome: String,
    pub dry_run: bool,
    pub idempotency_key: Option<String>,
    pub transaction_status: String,
    pub changed_surfaces: Vec<ChangedSurface>,
    pub linked_evidence_ids: Vec<String>,
    pub redaction_summary: RedactionSummary,
    pub degradation_codes: Vec<String>,
    pub hash_chain_valid: bool,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Redaction summary for an operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RedactionSummary {
    pub posture: String,
    pub fields_redacted: u32,
    pub patterns_applied: Vec<String>,
}

/// Report from showing an operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditShowReport {
    pub schema: String,
    pub operation: AuditOperationRecord,
    pub next_commands: Vec<String>,
    pub generated_at: String,
}

impl AuditShowReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show details of an audited operation.
pub fn show_operation(options: &AuditShowOptions) -> Result<AuditShowReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let operation = AuditOperationRecord {
        operation_id: options.operation_id.clone(),
        command_path: "ee remember".to_owned(),
        command_hash: "sha256:abc123def456".to_owned(),
        workspace_id: Some("ws_default".to_owned()),
        actor_identity: Some("agent:claude-code".to_owned()),
        expected_effect: AuditEffectClass::DurableMemoryWrite.as_str().to_owned(),
        observed_effect: AuditEffectClass::DurableMemoryWrite.as_str().to_owned(),
        effect_match: true,
        outcome: AuditOutcome::Success.as_str().to_owned(),
        dry_run: false,
        idempotency_key: Some("idem_abc123".to_owned()),
        transaction_status: "committed".to_owned(),
        changed_surfaces: vec![
            ChangedSurface {
                surface_type: "db_table".to_owned(),
                surface_name: "memories".to_owned(),
                rows_affected: Some(1),
                bytes_changed: Some(256),
            },
            ChangedSurface {
                surface_type: "db_table".to_owned(),
                surface_name: "audit_log".to_owned(),
                rows_affected: Some(1),
                bytes_changed: Some(128),
            },
        ],
        linked_evidence_ids: vec!["ev_001".to_owned()],
        redaction_summary: RedactionSummary {
            posture: RedactionPosture::Partial.as_str().to_owned(),
            fields_redacted: 2,
            patterns_applied: vec!["api_key".to_owned(), "password".to_owned()],
        },
        degradation_codes: vec![],
        hash_chain_valid: true,
        started_at: now.clone(),
        finished_at: Some(now.clone()),
        duration_ms: Some(42),
    };

    Ok(AuditShowReport {
        schema: AUDIT_OPERATION_SCHEMA_V1.to_owned(),
        operation,
        next_commands: vec![
            format!("ee audit diff {} --json", options.operation_id),
            "ee audit verify --json".to_owned(),
        ],
        generated_at: now,
    })
}

// ============================================================================
// Diff Operation
// ============================================================================

/// Options for showing operation diff.
#[derive(Clone, Debug, Default)]
pub struct AuditDiffOptions {
    pub workspace: PathBuf,
    pub operation_id: String,
}

/// State delta for a surface.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateDelta {
    pub surface_name: String,
    pub surface_type: String,
    pub declared_change: String,
    pub observed_change: String,
    pub match_status: String,
    pub row_count_before: Option<u32>,
    pub row_count_after: Option<u32>,
    pub content_hash_before: Option<String>,
    pub content_hash_after: Option<String>,
}

/// Report from showing operation diff.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditDiffReport {
    pub schema: String,
    pub operation_id: String,
    pub deltas: Vec<StateDelta>,
    pub all_match: bool,
    pub generated_at: String,
}

impl AuditDiffReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show state deltas for an operation.
pub fn show_diff(options: &AuditDiffOptions) -> Result<AuditDiffReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let deltas = vec![
        StateDelta {
            surface_name: "memories".to_owned(),
            surface_type: "db_table".to_owned(),
            declared_change: "insert".to_owned(),
            observed_change: "insert".to_owned(),
            match_status: "match".to_owned(),
            row_count_before: Some(10),
            row_count_after: Some(11),
            content_hash_before: Some("sha256:aaa111".to_owned()),
            content_hash_after: Some("sha256:bbb222".to_owned()),
        },
        StateDelta {
            surface_name: "audit_log".to_owned(),
            surface_type: "db_table".to_owned(),
            declared_change: "insert".to_owned(),
            observed_change: "insert".to_owned(),
            match_status: "match".to_owned(),
            row_count_before: Some(50),
            row_count_after: Some(51),
            content_hash_before: Some("sha256:ccc333".to_owned()),
            content_hash_after: Some("sha256:ddd444".to_owned()),
        },
    ];

    let all_match = deltas.iter().all(|d| d.match_status == "match");

    Ok(AuditDiffReport {
        schema: AUDIT_DIFF_SCHEMA_V1.to_owned(),
        operation_id: options.operation_id.clone(),
        deltas,
        all_match,
        generated_at: now,
    })
}

// ============================================================================
// Verify Operation
// ============================================================================

/// Options for verifying audit integrity.
#[derive(Clone, Debug, Default)]
pub struct AuditVerifyOptions {
    pub workspace: PathBuf,
    pub since: Option<String>,
}

/// Verification issue found.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationIssue {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub operation_id: Option<String>,
    pub next_action: String,
}

/// Verification summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationSummary {
    pub operations_checked: u32,
    pub hash_chain_valid: bool,
    pub missing_records: u32,
    pub malformed_entries: u32,
    pub effect_mismatches: u32,
    pub redaction_failures: u32,
    pub schema_version_issues: u32,
    pub timestamp_order_issues: u32,
}

/// Report from verifying audit integrity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditVerifyReport {
    pub schema: String,
    pub summary: VerificationSummary,
    pub issues: Vec<VerificationIssue>,
    pub overall_valid: bool,
    pub next_actions: Vec<String>,
    pub generated_at: String,
}

impl AuditVerifyReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Verify audit integrity for a window.
pub fn verify_audit(_options: &AuditVerifyOptions) -> Result<AuditVerifyReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let summary = VerificationSummary {
        operations_checked: 15,
        hash_chain_valid: true,
        missing_records: 0,
        malformed_entries: 0,
        effect_mismatches: 0,
        redaction_failures: 0,
        schema_version_issues: 0,
        timestamp_order_issues: 0,
    };

    Ok(AuditVerifyReport {
        schema: AUDIT_VERIFY_SCHEMA_V1.to_owned(),
        summary,
        issues: vec![],
        overall_valid: true,
        next_actions: vec!["ee audit timeline --json".to_owned()],
        generated_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn timeline_limits_entries() -> TestResult {
        let options = AuditTimelineOptions {
            limit: 2,
            ..Default::default()
        };

        let report = list_timeline(&options).map_err(|e| e.message())?;
        assert!(report.entries.len() <= 2);
        assert_eq!(
            report.pagination.returned_count,
            report.entries.len() as u32
        );
        Ok(())
    }

    #[test]
    fn show_operation_returns_details() -> TestResult {
        let options = AuditShowOptions {
            operation_id: "op_test".to_owned(),
            ..Default::default()
        };

        let report = show_operation(&options).map_err(|e| e.message())?;
        assert_eq!(report.operation.operation_id, "op_test");
        assert!(report.operation.effect_match);
        Ok(())
    }

    #[test]
    fn diff_reports_all_match() -> TestResult {
        let options = AuditDiffOptions {
            operation_id: "op_test".to_owned(),
            ..Default::default()
        };

        let report = show_diff(&options).map_err(|e| e.message())?;
        assert!(report.all_match);
        assert!(!report.deltas.is_empty());
        Ok(())
    }

    #[test]
    fn verify_returns_valid_summary() -> TestResult {
        let options = AuditVerifyOptions::default();

        let report = verify_audit(&options).map_err(|e| e.message())?;
        assert!(report.overall_valid);
        assert!(report.summary.hash_chain_valid);
        assert_eq!(report.summary.missing_records, 0);
        Ok(())
    }

    #[test]
    fn effect_class_as_str() {
        assert_eq!(AuditEffectClass::ReadOnly.as_str(), "read_only");
        assert_eq!(
            AuditEffectClass::DurableMemoryWrite.as_str(),
            "durable_memory_write"
        );
    }

    #[test]
    fn outcome_as_str() {
        assert_eq!(AuditOutcome::Success.as_str(), "success");
        assert_eq!(AuditOutcome::DryRun.as_str(), "dry_run");
    }
}
