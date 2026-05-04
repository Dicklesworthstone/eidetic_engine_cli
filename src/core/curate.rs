//! Curation queue read services.
//!
//! `ee curate candidates` exposes the auditable proposal queue without
//! validating or applying candidates. Validation and durable mutation are
//! separate explicit commands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::curate::{
    CandidateInput, CandidateSource, CandidateStatus, CandidateType, CandidateValidationError,
    ReviewQueueState, validate_candidate, validate_review_queue_transition,
};
use crate::db::{
    ApplyMemoryCurationInput, CreateAuditInput, CurationCandidateReviewUpdate, DbConnection,
    StoredCurationCandidate, StoredCurationTtlPolicy, StoredMemory, audit_actions,
    default_curation_ttl_policy_id_for_review_state, generate_audit_id,
};
use crate::models::{DomainError, MemoryId, WorkspaceId};

/// Stable schema for `ee curate candidates` response data.
pub const CURATE_CANDIDATES_SCHEMA_V1: &str = "ee.curate.candidates.v1";
/// Stable schema for `ee curate validate` response data.
pub const CURATE_VALIDATE_SCHEMA_V1: &str = "ee.curate.validate.v1";
/// Stable schema for `ee curate apply` response data.
pub const CURATE_APPLY_SCHEMA_V1: &str = "ee.curate.apply.v1";
/// Stable schema for explicit curation lifecycle review commands.
pub const CURATE_REVIEW_SCHEMA_V1: &str = "ee.curate.review.v1";
/// Stable schema for deterministic TTL disposition reports.
pub const CURATE_DISPOSITION_SCHEMA_V1: &str = "ee.curate.disposition.v1";

const MAX_CANDIDATE_LIST_LIMIT: u32 = 1000;
const DEFAULT_SNOOZE_SECONDS: u64 = 90 * 24 * 60 * 60;

/// Options for listing curation candidates through `ee curate candidates`.
#[derive(Clone, Debug)]
pub struct CurateCandidatesOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Optional candidate type filter.
    pub candidate_type: Option<&'a str>,
    /// Optional status filter. `None` lists all statuses.
    pub status: Option<&'a str>,
    /// Optional target memory filter.
    pub target_memory_id: Option<&'a str>,
    /// Maximum number of candidates to return.
    pub limit: u32,
    /// Number of filtered candidates to skip.
    pub offset: u32,
    /// Sort mode for queue presentation.
    pub sort: &'a str,
    /// Group likely duplicates contiguously in the result ordering.
    pub group_duplicates: bool,
}

/// Options for validating one curation candidate through `ee curate validate`.
#[derive(Clone, Debug)]
pub struct CurateValidateOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Candidate ID using the `curate_*` storage ID format.
    pub candidate_id: &'a str,
    /// Optional actor to persist in review/audit metadata.
    pub actor: Option<&'a str>,
    /// Validate and report without mutating the curation candidate.
    pub dry_run: bool,
}

/// Options for applying one approved curation candidate.
#[derive(Clone, Debug)]
pub struct CurateApplyOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Candidate ID using the `curate_*` storage ID format.
    pub candidate_id: &'a str,
    /// Optional actor to persist in apply/audit metadata.
    pub actor: Option<&'a str>,
    /// Preview the durable mutation without writing memory, candidate, or audit rows.
    pub dry_run: bool,
}

/// Explicit curation review lifecycle action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurateReviewAction {
    Accept,
    Reject,
    Snooze,
    Merge,
}

impl CurateReviewAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::Snooze => "snooze",
            Self::Merge => "merge",
        }
    }

    #[must_use]
    pub const fn command_name(self) -> &'static str {
        match self {
            Self::Accept => "curate accept",
            Self::Reject => "curate reject",
            Self::Snooze => "curate snooze",
            Self::Merge => "curate merge",
        }
    }

    #[must_use]
    pub const fn audit_action(self) -> &'static str {
        match self {
            Self::Accept => audit_actions::CURATION_CANDIDATE_ACCEPT,
            Self::Reject => audit_actions::CURATION_CANDIDATE_REJECT,
            Self::Snooze => audit_actions::CURATION_CANDIDATE_SNOOZE,
            Self::Merge => audit_actions::CURATION_CANDIDATE_MERGE,
        }
    }
}

/// Options for an explicit curation review lifecycle command.
#[derive(Clone, Debug)]
pub struct CurateReviewOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Candidate ID using the `curate_*` storage ID format.
    pub candidate_id: &'a str,
    /// Lifecycle command being executed.
    pub action: CurateReviewAction,
    /// Optional actor to persist in review/audit metadata.
    pub actor: Option<&'a str>,
    /// Preview without updating candidate status, review state, or audit rows.
    pub dry_run: bool,
    /// RFC 3339 timestamp for `ee curate snooze`.
    pub snoozed_until: Option<&'a str>,
    /// Target candidate ID for `ee curate merge <source> <target>`.
    pub merge_into_candidate_id: Option<&'a str>,
}

/// Options for deterministic TTL disposition over the curation queue.
#[derive(Clone, Debug)]
pub struct CurateDispositionOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Actor recorded in audit metadata when applying transitions.
    pub actor: Option<&'a str>,
    /// Apply deterministic transitions. Defaults to false for dry-run behavior.
    pub apply: bool,
    /// Optional frozen clock for tests and deterministic replay.
    pub now_rfc3339: Option<&'a str>,
}

/// Result of listing curation candidates.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidatesReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub total_count: usize,
    pub returned_count: usize,
    pub limit: u32,
    pub offset: u32,
    pub truncated: bool,
    pub durable_mutation: bool,
    pub filter: CurateCandidatesFilter,
    pub candidates: Vec<CurateCandidateSummary>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

/// Result of validating one curation candidate.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateValidateReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub candidate_id: String,
    pub candidate: CurateCandidateSummary,
    pub validation: CurateValidateResult,
    pub mutation: CurateValidateMutation,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

/// Result of applying one approved curation candidate.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateApplyReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub candidate_id: String,
    pub candidate: CurateCandidateSummary,
    pub application: CurateApplyResult,
    pub mutation: CurateApplyMutation,
    pub target_before: Option<CurateApplyMemoryState>,
    pub target_after: Option<CurateApplyMemoryState>,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

/// Result of an explicit curation review lifecycle command.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateReviewReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub candidate_id: String,
    pub candidate: CurateCandidateSummary,
    pub review: CurateReviewResult,
    pub mutation: CurateReviewMutation,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

/// Result of deterministic curation TTL disposition.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateDispositionReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub dry_run: bool,
    pub apply: bool,
    pub durable_mutation: bool,
    pub summary: CurateDispositionSummary,
    pub policies: Vec<CurateTtlPolicySummary>,
    pub decisions: Vec<CurateDispositionDecision>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

impl CurateValidateReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"curate validate","status":"serialization_failed"}}"#,
                CURATE_VALIDATE_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run { "DRY RUN" } else { "VALIDATED" };
        let mut output = format!("{mode}: {}\n\n", self.candidate_id);
        output.push_str(&format!("  status: {}\n", self.validation.status));
        output.push_str(&format!("  decision: {}\n", self.validation.decision));
        output.push_str(&format!(
            "  transition: {} -> {}\n",
            self.mutation.from_status, self.mutation.to_status
        ));
        if !self.validation.errors.is_empty() {
            output.push_str("  errors:\n");
            for issue in &self.validation.errors {
                output.push_str(&format!("    - {}: {}\n", issue.code, issue.message));
            }
        }
        if !self.validation.warnings.is_empty() {
            output.push_str("  warnings:\n");
            for issue in &self.validation.warnings {
                output.push_str(&format!("    - {}: {}\n", issue.code, issue.message));
            }
        }
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "CURATE_VALIDATE|id={}|status={}|decision={}|from={}|to={}|dry_run={}|persisted={}",
            self.candidate_id,
            self.validation.status,
            self.validation.decision,
            self.mutation.from_status,
            self.mutation.to_status,
            self.dry_run,
            self.mutation.persisted
        )
    }
}

impl CurateApplyReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"curate apply","status":"serialization_failed"}}"#,
                CURATE_APPLY_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run {
            "DRY RUN"
        } else if self.mutation.persisted {
            "APPLIED"
        } else {
            "UNCHANGED"
        };
        let mut output = format!("{mode}: {}\n\n", self.candidate_id);
        output.push_str(&format!("  status: {}\n", self.application.status));
        output.push_str(&format!("  decision: {}\n", self.application.decision));
        output.push_str(&format!(
            "  transition: {} -> {}\n",
            self.mutation.from_status, self.mutation.to_status
        ));
        if !self.application.changes.is_empty() {
            output.push_str("  changes:\n");
            for change in &self.application.changes {
                output.push_str(&format!(
                    "    - {}: {} -> {}\n",
                    change.field,
                    change.before.as_deref().unwrap_or("<none>"),
                    change.after.as_deref().unwrap_or("<none>")
                ));
            }
        }
        if !self.application.errors.is_empty() {
            output.push_str("  errors:\n");
            for issue in &self.application.errors {
                output.push_str(&format!("    - {}: {}\n", issue.code, issue.message));
            }
        }
        if !self.application.warnings.is_empty() {
            output.push_str("  warnings:\n");
            for issue in &self.application.warnings {
                output.push_str(&format!("    - {}: {}\n", issue.code, issue.message));
            }
        }
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "CURATE_APPLY|id={}|status={}|decision={}|from={}|to={}|dry_run={}|persisted={}|changes={}",
            self.candidate_id,
            self.application.status,
            self.application.decision,
            self.mutation.from_status,
            self.mutation.to_status,
            self.dry_run,
            self.mutation.persisted,
            self.application.changes.len()
        )
    }
}

impl CurateReviewReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"{}","status":"serialization_failed"}}"#,
                CURATE_REVIEW_SCHEMA_V1, self.command
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run {
            "DRY RUN"
        } else if self.mutation.persisted {
            "REVIEWED"
        } else {
            "UNCHANGED"
        };
        let mut output = format!("{mode}: {}\n\n", self.candidate_id);
        output.push_str(&format!("  action: {}\n", self.review.action));
        output.push_str(&format!("  status: {}\n", self.review.status));
        output.push_str(&format!("  decision: {}\n", self.review.decision));
        output.push_str(&format!(
            "  status transition: {} -> {}\n",
            self.mutation.from_status, self.mutation.to_status
        ));
        output.push_str(&format!(
            "  review state: {} -> {}\n",
            self.mutation.from_review_state, self.mutation.to_review_state
        ));
        if let Some(until) = &self.mutation.snoozed_until {
            output.push_str(&format!("  snoozed until: {until}\n"));
        }
        if let Some(target) = &self.mutation.merged_into_candidate_id {
            output.push_str(&format!("  merged into: {target}\n"));
        }
        if !self.review.errors.is_empty() {
            output.push_str("  errors:\n");
            for issue in &self.review.errors {
                output.push_str(&format!("    - {}: {}\n", issue.code, issue.message));
            }
        }
        if !self.review.warnings.is_empty() {
            output.push_str("  warnings:\n");
            for issue in &self.review.warnings {
                output.push_str(&format!("    - {}: {}\n", issue.code, issue.message));
            }
        }
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "CURATE_REVIEW|command={}|id={}|action={}|status={}|decision={}|from_status={}|to_status={}|from_review_state={}|to_review_state={}|dry_run={}|persisted={}",
            self.command,
            self.candidate_id,
            self.review.action,
            self.review.status,
            self.review.decision,
            self.mutation.from_status,
            self.mutation.to_status,
            self.mutation.from_review_state,
            self.mutation.to_review_state,
            self.dry_run,
            self.mutation.persisted
        )
    }
}

impl CurateDispositionReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"curate disposition","status":"serialization_failed"}}"#,
                CURATE_DISPOSITION_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.apply { "APPLY" } else { "DRY RUN" };
        let mut output = format!(
            "{mode}: curation disposition ({} candidates, {} due)\n\n",
            self.summary.total_candidates, self.summary.due_count
        );
        for decision in &self.decisions {
            if decision.decision == "not_due" {
                continue;
            }
            output.push_str(&format!(
                "  {} [{}] action={} decision={}\n",
                decision.candidate_id, decision.review_state, decision.action, decision.decision
            ));
            if let Some(due_at) = &decision.due_at {
                output.push_str(&format!("    due: {due_at}\n"));
            }
            if let Some(transition) = &decision.planned_transition {
                output.push_str(&format!(
                    "    transition: {}/{} -> {}/{}\n",
                    transition.from_status,
                    transition.from_review_state,
                    transition.to_status,
                    transition.to_review_state
                ));
            }
        }
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "CURATE_DISPOSITION|total={}|due={}|applied={}|prompts={}|escalations={}|dry_run={}",
            self.summary.total_candidates,
            self.summary.due_count,
            self.summary.applied_count,
            self.summary.prompt_count,
            self.summary.escalation_count,
            self.dry_run
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateReviewResult {
    pub status: String,
    pub decision: String,
    pub action: String,
    pub errors: Vec<CurateValidationIssue>,
    pub warnings: Vec<CurateValidationIssue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateReviewMutation {
    pub from_status: String,
    pub to_status: String,
    pub from_review_state: String,
    pub to_review_state: String,
    pub persisted: bool,
    pub reviewed_at: Option<String>,
    pub reviewed_by: Option<String>,
    pub snoozed_until: Option<String>,
    pub merged_into_candidate_id: Option<String>,
    pub audit_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateValidateResult {
    pub status: String,
    pub decision: String,
    pub errors: Vec<CurateValidationIssue>,
    pub warnings: Vec<CurateValidationIssue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateValidationIssue {
    pub code: String,
    pub message: String,
    pub repair: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateValidateMutation {
    pub from_status: String,
    pub to_status: String,
    pub persisted: bool,
    pub reviewed_at: Option<String>,
    pub reviewed_by: Option<String>,
    pub audit_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateApplyResult {
    pub status: String,
    pub decision: String,
    pub candidate_type: String,
    pub target_memory_id: String,
    pub changes: Vec<CurateApplyChange>,
    pub errors: Vec<CurateValidationIssue>,
    pub warnings: Vec<CurateValidationIssue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateApplyChange {
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateApplyMemoryState {
    pub id: String,
    pub content: String,
    pub confidence: f32,
    pub trust_class: String,
    pub tombstoned: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateApplyMutation {
    pub from_status: String,
    pub to_status: String,
    pub persisted: bool,
    pub applied_at: Option<String>,
    pub applied_by: Option<String>,
    pub audit_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateDispositionSummary {
    pub total_candidates: usize,
    pub due_count: usize,
    pub applied_count: usize,
    pub prompt_count: usize,
    pub escalation_count: usize,
    pub blocked_count: usize,
    pub next_scheduled_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateTtlPolicySummary {
    pub id: String,
    pub review_state: String,
    pub threshold_seconds: u64,
    pub action: String,
    pub requires_evidence_count: u32,
    pub requires_distinct_sessions: u32,
    pub requires_no_harmful_within_seconds: Option<u64>,
    pub auto_promote_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateDispositionDecision {
    pub candidate_id: String,
    pub policy_id: String,
    pub review_state: String,
    pub status: String,
    pub action: String,
    pub decision: String,
    pub state_entered_at: Option<String>,
    pub due_at: Option<String>,
    pub ttl_elapsed_seconds: Option<i64>,
    pub ttl_threshold_seconds: u64,
    pub evidence_count: u32,
    pub distinct_session_count: u32,
    pub auto_promote_enabled: bool,
    pub gate_status: String,
    pub planned_transition: Option<CurateDispositionTransition>,
    pub audit: Option<CurateDispositionAuditPlan>,
    pub errors: Vec<CurateValidationIssue>,
    pub warnings: Vec<CurateValidationIssue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateDispositionTransition {
    pub from_status: String,
    pub to_status: String,
    pub from_review_state: String,
    pub to_review_state: String,
    pub snoozed_until: Option<String>,
    pub ttl_policy_id: String,
    pub persisted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateDispositionAuditPlan {
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub audit_id: Option<String>,
}

impl CurateCandidatesReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"curate candidates","status":"serialization_failed"}}"#,
                CURATE_CANDIDATES_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Curation candidates ({} total", self.total_count);
        if self.truncated {
            output.push_str(", showing batch");
        }
        output.push_str(")\n\n");
        if self.candidates.is_empty() {
            output.push_str("  No curation candidates found.\n");
            return output;
        }
        for candidate in &self.candidates {
            output.push_str(&format!(
                "  {} [{}] confidence={:.2}\n",
                candidate.id, candidate.status, candidate.confidence
            ));
            output.push_str(&format!(
                "    type={}, target={}\n",
                candidate.candidate_type, candidate.target_memory_id
            ));
            output.push_str(&format!("    reason={}\n\n", candidate.reason));
        }
        output.push_str("Next:\n  ee curate validate <CANDIDATE_ID>\n");
        output
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "CURATE_CANDIDATES|total={}|returned={}|status={}|type={}|mutated={}",
            self.total_count,
            self.returned_count,
            self.filter.status.as_deref().unwrap_or("all"),
            self.filter.candidate_type.as_deref().unwrap_or("all"),
            self.durable_mutation
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidatesFilter {
    #[serde(rename = "type")]
    pub candidate_type: Option<String>,
    pub status: Option<String>,
    pub target_memory_id: Option<String>,
    pub sort: String,
    pub group_duplicates: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidateSummary {
    pub id: String,
    #[serde(rename = "type")]
    pub candidate_type: String,
    pub target_memory_id: String,
    pub proposed_content: Option<String>,
    pub proposed_confidence: Option<f32>,
    pub proposed_trust_class: Option<String>,
    pub confidence: f32,
    pub status: String,
    pub review_state: String,
    pub reason: String,
    pub source: CurateCandidateSource,
    pub evidence: Vec<CurateCandidateEvidence>,
    pub validation: CurateCandidateValidation,
    pub scope: String,
    pub scope_key: String,
    pub created_at: String,
    pub reviewed_at: Option<String>,
    pub reviewed_by: Option<String>,
    pub applied_at: Option<String>,
    pub ttl_expires_at: Option<String>,
    pub snoozed_until: Option<String>,
    pub merged_into_candidate_id: Option<String>,
    pub state_entered_at: Option<String>,
    pub last_action_at: Option<String>,
    pub ttl_policy_id: Option<String>,
    pub requires_validate: bool,
    pub requires_apply: bool,
    pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidateSource {
    pub source_type: String,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidateEvidence {
    #[serde(rename = "type")]
    pub evidence_type: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidateValidation {
    pub status: String,
    pub warnings: Vec<String>,
    pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidatesDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

#[derive(Clone, Debug)]
struct PreparedCurateRead {
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
}

/// List curation candidates for the selected workspace.
pub fn list_curation_candidates(
    options: &CurateCandidatesOptions<'_>,
) -> Result<CurateCandidatesReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let candidate_type = parse_optional_candidate_type(options.candidate_type)?;
    let status = parse_optional_status(options.status)?;
    let target_memory_id = parse_optional_memory_id(options.target_memory_id)?;
    validate_list_window(options.limit)?;

    let connection = open_existing_database(&prepared.database_path)?;
    let stored = connection
        .list_curation_candidates(
            &prepared.workspace_id,
            candidate_type.as_deref(),
            status.as_deref(),
            target_memory_id.as_deref(),
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list curation candidates: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let now = Utc::now().to_rfc3339();
    let sort_mode = parse_curate_candidate_sort_mode(options.sort)?;
    let mut stored = if status.as_deref() == Some(CandidateStatus::Pending.as_str()) {
        stored
            .into_iter()
            .filter(|candidate| !candidate_hidden_from_default_queue(candidate, &now))
            .collect::<Vec<_>>()
    } else {
        stored
    };
    sort_curate_candidates(&mut stored, sort_mode, options.group_duplicates);

    let total_count = stored.len();
    let offset = usize::try_from(options.offset).map_err(|_| {
        curate_usage_error(
            "curate candidates offset is too large".to_owned(),
            "ee curate candidates --help",
        )
    })?;
    let limit = usize::try_from(options.limit).map_err(|_| {
        curate_usage_error(
            "curate candidates limit is too large".to_owned(),
            "ee curate candidates --help",
        )
    })?;
    let candidates = stored
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|candidate| candidate_summary_from_stored(candidate, &prepared.workspace_path))
        .collect::<Vec<_>>();
    let returned_count = candidates.len();
    let truncated = offset.saturating_add(returned_count) < total_count;
    let next_action = candidates.first().map_or_else(
        || "no pending curation candidates".to_owned(),
        |candidate| candidate.next_action.clone(),
    );

    Ok(CurateCandidatesReport {
        schema: CURATE_CANDIDATES_SCHEMA_V1,
        command: "curate candidates",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        total_count,
        returned_count,
        limit: options.limit,
        offset: options.offset,
        truncated,
        durable_mutation: false,
        filter: CurateCandidatesFilter {
            candidate_type,
            status,
            target_memory_id,
            sort: sort_mode.as_str().to_owned(),
            group_duplicates: options.group_duplicates,
        },
        candidates,
        degraded: Vec::new(),
        next_action,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CurateCandidateSortMode {
    ReviewState,
    CreatedAt,
    Confidence,
}

impl CurateCandidateSortMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReviewState => "review_state",
            Self::CreatedAt => "created_at",
            Self::Confidence => "confidence",
        }
    }
}

fn parse_curate_candidate_sort_mode(raw: &str) -> Result<CurateCandidateSortMode, DomainError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "review_state" | "review-state" | "state" | "queue" => {
            Ok(CurateCandidateSortMode::ReviewState)
        }
        "created_at" | "created-at" | "created" | "time" => Ok(CurateCandidateSortMode::CreatedAt),
        "confidence" | "score" => Ok(CurateCandidateSortMode::Confidence),
        _ => Err(curate_usage_error(
            format!(
                "Unknown curate candidates sort mode `{raw}`; expected review_state, created_at, or confidence"
            ),
            "ee curate candidates --help",
        )),
    }
}

fn sort_curate_candidates(
    stored: &mut [StoredCurationCandidate],
    sort_mode: CurateCandidateSortMode,
    group_duplicates: bool,
) {
    stored.sort_by(|left, right| {
        if group_duplicates {
            let left_group = duplicate_group_key(left);
            let right_group = duplicate_group_key(right);
            let cmp = left_group.cmp(&right_group);
            if !cmp.is_eq() {
                return cmp;
            }
        }

        let cmp = match sort_mode {
            CurateCandidateSortMode::ReviewState => review_state_rank(&left.review_state)
                .cmp(&review_state_rank(&right.review_state))
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id)),
            CurateCandidateSortMode::CreatedAt => right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id)),
            CurateCandidateSortMode::Confidence => right
                .confidence
                .partial_cmp(&left.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id)),
        };
        if cmp.is_eq() {
            left.id.cmp(&right.id)
        } else {
            cmp
        }
    });
}

fn duplicate_group_key(candidate: &StoredCurationCandidate) -> (String, String, String) {
    (
        candidate.target_memory_id.clone(),
        candidate.candidate_type.clone(),
        candidate
            .proposed_content
            .clone()
            .unwrap_or_else(|| candidate.reason.clone()),
    )
}

fn review_state_rank(review_state: &str) -> u8 {
    match review_state {
        "new" => 0,
        "needs_evidence" => 1,
        "needs_scope" => 2,
        "duplicate" => 3,
        "snoozed" => 4,
        "accepted" => 5,
        "rejected" => 6,
        "merged" => 7,
        "superseded" => 8,
        "expired" => 9,
        "applied" => 10,
        _ => 255,
    }
}

/// Validate one curation candidate and record the curation review decision.
pub fn validate_curation_candidate(
    options: &CurateValidateOptions<'_>,
) -> Result<CurateValidateReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let candidate_id = validate_curate_candidate_id(options.candidate_id)?;
    let reviewed_by = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ee")
        .to_owned();

    let connection = open_existing_database(&prepared.database_path)?;
    let stored = connection
        .get_curation_candidate(&prepared.workspace_id, &candidate_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load curation candidate: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "curation candidate".to_owned(),
            id: candidate_id.clone(),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    let target_memory = connection
        .get_memory(&stored.target_memory_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load target memory: {error}"),
            repair: Some("ee memory show <memory-id> --json".to_owned()),
        })?;

    let now = Utc::now().to_rfc3339();
    let decision = evaluate_candidate_for_validation(&stored, target_memory.as_ref(), &now);
    let from_status = stored.status.clone();
    let mut reviewed_at = None;
    let mut persisted = false;
    let mut audit_id = None;

    if decision.should_persist && !options.dry_run {
        let audit = persist_candidate_validation(
            &connection,
            &prepared.workspace_id,
            &stored,
            &decision.to_status,
            &now,
            &reviewed_by,
            &decision,
        )?;
        reviewed_at = Some(now.clone());
        persisted = true;
        audit_id = Some(audit);
    } else if decision.should_persist || options.dry_run {
        reviewed_at = Some(now.clone());
    }

    let mut candidate = candidate_summary_from_stored(stored, &prepared.workspace_path);
    candidate.validation = CurateCandidateValidation {
        status: decision.validation.status.clone(),
        warnings: decision
            .validation
            .warnings
            .iter()
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect(),
        next_action: decision.next_action.clone(),
    };
    if persisted {
        candidate.status = decision.to_status.clone();
        candidate.review_state = review_state_for_status_text(&decision.to_status).to_owned();
        candidate.reviewed_at = reviewed_at.clone();
        candidate.reviewed_by = Some(reviewed_by.clone());
        candidate.requires_validate =
            candidate_requires_validate(&candidate.status, &candidate.review_state);
        candidate.requires_apply =
            candidate_requires_apply(&candidate.status, &candidate.review_state);
        candidate.next_action = decision.next_action.clone();
    }

    let durable_mutation = persisted;
    Ok(CurateValidateReport {
        schema: CURATE_VALIDATE_SCHEMA_V1,
        command: "curate validate",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        candidate_id,
        candidate,
        validation: decision.validation,
        mutation: CurateValidateMutation {
            from_status,
            to_status: decision.to_status,
            persisted,
            reviewed_at,
            reviewed_by: if decision.should_persist || options.dry_run {
                Some(reviewed_by)
            } else {
                None
            },
            audit_id,
        },
        dry_run: options.dry_run,
        durable_mutation,
        degraded: Vec::new(),
        next_action: decision.next_action,
    })
}

/// Apply one approved curation candidate to its target memory.
pub fn apply_curation_candidate(
    options: &CurateApplyOptions<'_>,
) -> Result<CurateApplyReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let candidate_id = validate_curate_candidate_id(options.candidate_id)?;
    let applied_by = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ee")
        .to_owned();

    let connection = open_existing_database(&prepared.database_path)?;
    let stored = connection
        .get_curation_candidate(&prepared.workspace_id, &candidate_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load curation candidate: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "curation candidate".to_owned(),
            id: candidate_id.clone(),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    let target_memory = connection
        .get_memory(&stored.target_memory_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load target memory: {error}"),
            repair: Some("ee memory show <memory-id> --json".to_owned()),
        })?;

    let now = Utc::now().to_rfc3339();
    let decision = evaluate_candidate_for_apply(&stored, target_memory.as_ref(), &now);
    let from_status = stored.status.clone();
    let mut applied_at = None;
    let mut persisted = false;
    let mut audit_id = None;

    if decision.should_persist && !options.dry_run {
        let audit = persist_candidate_application(
            &connection,
            &prepared.workspace_id,
            &stored,
            &decision,
            &now,
            &applied_by,
        )?;
        applied_at = Some(now.clone());
        persisted = true;
        audit_id = Some(audit);
    } else if decision.should_persist || options.dry_run {
        applied_at = Some(now.clone());
    }

    let mut candidate = candidate_summary_from_stored(stored, &prepared.workspace_path);
    if persisted {
        candidate.status = CandidateStatus::Applied.as_str().to_owned();
        candidate.review_state = ReviewQueueState::Applied.as_str().to_owned();
        candidate.applied_at = applied_at.clone();
        candidate.requires_validate = false;
        candidate.requires_apply = false;
        candidate.next_action = "no action required".to_owned();
    }

    let mut application = decision.application;
    if persisted {
        application.status = "applied".to_owned();
    } else if decision.should_persist && options.dry_run {
        application.status = "would_apply".to_owned();
    }

    Ok(CurateApplyReport {
        schema: CURATE_APPLY_SCHEMA_V1,
        command: "curate apply",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        candidate_id,
        candidate,
        application,
        mutation: CurateApplyMutation {
            from_status,
            to_status: decision.to_status,
            persisted,
            applied_at,
            applied_by: if decision.should_persist || options.dry_run {
                Some(applied_by)
            } else {
                None
            },
            audit_id,
        },
        target_before: decision.target_before,
        target_after: decision.target_after,
        dry_run: options.dry_run,
        durable_mutation: persisted,
        degraded: Vec::new(),
        next_action: decision.next_action,
    })
}

/// Execute an explicit curation review lifecycle command.
pub fn review_curation_candidate(
    options: &CurateReviewOptions<'_>,
) -> Result<CurateReviewReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let candidate_id = validate_curate_candidate_id(options.candidate_id)?;
    let reviewed_by = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ee")
        .to_owned();
    let merge_into_candidate_id = parse_merge_target_candidate_id(options)?;
    let snoozed_until = parse_snoozed_until(options)?;

    let connection = open_existing_database(&prepared.database_path)?;
    let stored = connection
        .get_curation_candidate(&prepared.workspace_id, &candidate_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load curation candidate: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "curation candidate".to_owned(),
            id: candidate_id.clone(),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    let merge_target = if let Some(target_id) = merge_into_candidate_id.as_deref() {
        Some(load_merge_target_candidate(
            &connection,
            &prepared.workspace_id,
            target_id,
        )?)
    } else {
        None
    };

    let now = Utc::now().to_rfc3339();
    let decision = evaluate_candidate_for_review(
        &stored,
        options.action,
        snoozed_until.as_deref(),
        merge_into_candidate_id.as_deref(),
        merge_target.as_ref(),
        &now,
    );
    let from_status = stored.status.clone();
    let from_review_state = stored.review_state.clone();
    let mut reviewed_at = None;
    let mut persisted = false;
    let mut audit_id = None;

    if decision.should_persist && !options.dry_run {
        let audit = persist_candidate_review(
            &connection,
            &prepared.workspace_id,
            &stored,
            options.action,
            &decision,
            &now,
            &reviewed_by,
        )?;
        reviewed_at = Some(now.clone());
        persisted = true;
        audit_id = Some(audit);
    } else if decision.should_persist || options.dry_run {
        reviewed_at = Some(now.clone());
    }

    let mut candidate = candidate_summary_from_stored(stored, &prepared.workspace_path);
    if persisted {
        candidate.status = decision.to_status.clone();
        candidate.review_state = decision.to_review_state.clone();
        candidate.reviewed_at = reviewed_at.clone();
        candidate.reviewed_by = Some(reviewed_by.clone());
        candidate.snoozed_until = decision.snoozed_until.clone();
        candidate.merged_into_candidate_id = decision.merged_into_candidate_id.clone();
        candidate.requires_validate =
            candidate_requires_validate(&candidate.status, &candidate.review_state);
        candidate.requires_apply =
            candidate_requires_apply(&candidate.status, &candidate.review_state);
        candidate.next_action = decision.next_action.clone();
    }

    Ok(CurateReviewReport {
        schema: CURATE_REVIEW_SCHEMA_V1,
        command: options.action.command_name(),
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        candidate_id,
        candidate,
        review: decision.review,
        mutation: CurateReviewMutation {
            from_status,
            to_status: decision.to_status,
            from_review_state,
            to_review_state: decision.to_review_state,
            persisted,
            reviewed_at,
            reviewed_by: if decision.should_persist || options.dry_run {
                Some(reviewed_by)
            } else {
                None
            },
            snoozed_until: decision.snoozed_until,
            merged_into_candidate_id: decision.merged_into_candidate_id,
            audit_id,
        },
        dry_run: options.dry_run,
        durable_mutation: persisted,
        degraded: Vec::new(),
        next_action: decision.next_action,
    })
}

/// Evaluate and optionally apply deterministic TTL disposition rules.
pub fn run_curation_disposition(
    options: &CurateDispositionOptions<'_>,
) -> Result<CurateDispositionReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let actor = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ee")
        .to_owned();
    let now = parse_or_current_time(options.now_rfc3339)?;

    let connection = open_existing_database(&prepared.database_path)?;
    let candidates = connection
        .list_curation_candidates(&prepared.workspace_id, None, None, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list curation candidates: {error}"),
            repair: Some("ee curate candidates --all --json".to_owned()),
        })?;
    let policies =
        connection
            .list_curation_ttl_policies()
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to list curation TTL policies: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;
    let policy_map = policies
        .iter()
        .map(|policy| (policy.id.as_str(), policy))
        .collect::<BTreeMap<_, _>>();

    let mut degraded = Vec::new();
    let mut decisions = Vec::new();
    for candidate in &candidates {
        let decision = evaluate_candidate_for_disposition(
            candidate,
            &policy_map,
            &now,
            options.apply,
            &actor,
            &connection,
            &mut degraded,
        )?;
        decisions.push(decision);
    }
    decisions.sort_by(|left, right| {
        left.due_at
            .cmp(&right.due_at)
            .then_with(|| left.candidate_id.cmp(&right.candidate_id))
    });

    let summary = disposition_summary(&decisions, candidates.len());
    let durable_mutation = decisions.iter().any(|decision| {
        decision
            .planned_transition
            .as_ref()
            .is_some_and(|t| t.persisted)
    });
    let next_action = if options.apply {
        "ee status --json".to_owned()
    } else if summary.due_count > 0 {
        "ee curate disposition --apply --json".to_owned()
    } else {
        "no action required".to_owned()
    };

    Ok(CurateDispositionReport {
        schema: CURATE_DISPOSITION_SCHEMA_V1,
        command: "curate disposition",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        dry_run: !options.apply,
        apply: options.apply,
        durable_mutation,
        summary,
        policies: policies.iter().map(policy_summary).collect(),
        decisions,
        degraded,
        next_action,
    })
}

#[derive(Clone, Debug)]
struct ValidationDecision {
    validation: CurateValidateResult,
    to_status: String,
    should_persist: bool,
    next_action: String,
}

#[derive(Clone, Debug)]
struct ApplyDecision {
    application: CurateApplyResult,
    to_status: String,
    should_persist: bool,
    memory_update: Option<ApplyMemoryCurationInput>,
    tombstone_memory: bool,
    target_before: Option<CurateApplyMemoryState>,
    target_after: Option<CurateApplyMemoryState>,
    next_action: String,
}

#[derive(Clone, Debug)]
struct ReviewDecision {
    review: CurateReviewResult,
    to_status: String,
    to_review_state: String,
    should_persist: bool,
    snoozed_until: Option<String>,
    merged_into_candidate_id: Option<String>,
    next_action: String,
}

fn evaluate_candidate_for_validation(
    stored: &StoredCurationCandidate,
    target_memory: Option<&StoredMemory>,
    now_rfc3339: &str,
) -> ValidationDecision {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let current_status = parse_stored_status(&stored.status, &mut errors);

    if let Some(status) = current_status
        && status.is_terminal()
    {
        errors.push(validation_issue(
            "candidate_status_terminal",
            format!(
                "Candidate is already in terminal status {}.",
                status.as_str()
            ),
            "No validation transition is available for terminal candidates.",
        ));
        return blocked_validation(stored, errors, warnings);
    }

    if let Some(expires_at) = &stored.ttl_expires_at {
        match timestamp_has_expired(expires_at, now_rfc3339) {
            Ok(true) => {
                errors.push(validation_issue(
                    CandidateValidationError::CandidateExpired.code(),
                    "Candidate TTL has expired.",
                    "Create or review a fresh curation candidate.",
                ));
                return ValidationDecision {
                    validation: CurateValidateResult {
                        status: "failed".to_owned(),
                        decision: "expired".to_owned(),
                        errors,
                        warnings,
                    },
                    to_status: CandidateStatus::Expired.as_str().to_owned(),
                    should_persist: current_status
                        .is_some_and(|status| status.can_transition_to(CandidateStatus::Expired)),
                    next_action: "no action required".to_owned(),
                };
            }
            Ok(false) => {}
            Err(message) => errors.push(validation_issue(
                "invalid_ttl_timestamp",
                message,
                "Store ttl_expires_at as an RFC 3339 timestamp.",
            )),
        }
    }

    validate_target_memory(stored, target_memory, &mut errors);

    let candidate_type = CandidateType::from_str(&stored.candidate_type).map_err(|error| {
        validation_issue(
            "invalid_candidate_type",
            error.to_string(),
            "Regenerate the candidate with a supported candidate type.",
        )
    });
    let source_type = CandidateSource::from_str(&stored.source_type).map_err(|error| {
        validation_issue(
            "invalid_candidate_source",
            error.to_string(),
            "Regenerate the candidate with a supported source type.",
        )
    });

    match (candidate_type, source_type) {
        (Ok(candidate_type), Ok(source_type)) => {
            let input = CandidateInput {
                workspace_id: stored.workspace_id.clone(),
                candidate_type,
                target_memory_id: stored.target_memory_id.clone(),
                proposed_content: stored.proposed_content.clone(),
                proposed_confidence: stored.proposed_confidence,
                proposed_trust_class: stored.proposed_trust_class.clone(),
                source_type,
                source_id: stored.source_id.clone(),
                reason: stored.reason.clone(),
                confidence: stored.confidence,
                ttl_seconds: None,
            };
            if let Err(error) = validate_candidate(input, now_rfc3339) {
                errors.push(validation_issue(
                    error.code(),
                    error.to_string(),
                    validation_repair(&error),
                ));
            }
        }
        (Err(issue), Ok(_)) | (Ok(_), Err(issue)) => errors.push(issue),
        (Err(type_issue), Err(source_issue)) => {
            errors.push(type_issue);
            errors.push(source_issue);
        }
    }

    if warnings.is_empty() && stored.confidence < 0.50 {
        warnings.push(validation_issue(
            "low_candidate_confidence",
            format!(
                "Candidate confidence {:.2} is below the conservative review threshold.",
                stored.confidence
            ),
            "Review provenance before applying this candidate.",
        ));
    }

    let target_status = if errors.is_empty() {
        CandidateStatus::Approved
    } else {
        CandidateStatus::Rejected
    };
    let should_persist = current_status
        .is_some_and(|status| status != target_status && status.can_transition_to(target_status));
    let decision = if errors.is_empty() {
        "approved"
    } else {
        "rejected"
    };
    let status = if errors.is_empty() {
        "passed"
    } else {
        "failed"
    };
    let next_action = if target_status == CandidateStatus::Approved {
        format!("ee curate apply {}", stored.id)
    } else {
        "no action required".to_owned()
    };

    ValidationDecision {
        validation: CurateValidateResult {
            status: status.to_owned(),
            decision: decision.to_owned(),
            errors,
            warnings,
        },
        to_status: target_status.as_str().to_owned(),
        should_persist,
        next_action,
    }
}

fn evaluate_candidate_for_apply(
    stored: &StoredCurationCandidate,
    target_memory: Option<&StoredMemory>,
    now_rfc3339: &str,
) -> ApplyDecision {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let target_before = target_memory.map(memory_state_from_stored);
    let current_status = parse_stored_status(&stored.status, &mut errors);

    match current_status {
        Some(CandidateStatus::Approved) => {}
        Some(CandidateStatus::Pending) => {
            errors.push(validation_issue(
                "candidate_requires_validation",
                "Candidate must be approved before it can be applied.",
                format!("Run `ee curate validate {}` first.", stored.id),
            ));
            return blocked_apply(
                stored,
                target_before,
                errors,
                warnings,
                format!("ee curate validate {}", stored.id),
            );
        }
        Some(CandidateStatus::Applied) => {
            warnings.push(validation_issue(
                "candidate_already_applied",
                "Candidate has already been applied.",
                "No apply action is required.",
            ));
            return ApplyDecision {
                application: CurateApplyResult {
                    status: "already_applied".to_owned(),
                    decision: "unchanged".to_owned(),
                    candidate_type: stored.candidate_type.clone(),
                    target_memory_id: stored.target_memory_id.clone(),
                    changes: Vec::new(),
                    errors,
                    warnings,
                },
                to_status: CandidateStatus::Applied.as_str().to_owned(),
                should_persist: false,
                memory_update: None,
                tombstone_memory: false,
                target_before: target_before.clone(),
                target_after: target_before,
                next_action: "no action required".to_owned(),
            };
        }
        Some(status @ (CandidateStatus::Rejected | CandidateStatus::Expired)) => {
            errors.push(validation_issue(
                "candidate_status_terminal",
                format!("Candidate is in terminal status {}.", status.as_str()),
                "No apply transition is available for this candidate.",
            ));
            return blocked_apply(
                stored,
                target_before,
                errors,
                warnings,
                "no action required".to_owned(),
            );
        }
        None => {
            return blocked_apply(
                stored,
                target_before,
                errors,
                warnings,
                "ee curate candidates --json".to_owned(),
            );
        }
    }

    if let Some(expires_at) = &stored.ttl_expires_at {
        match timestamp_has_expired(expires_at, now_rfc3339) {
            Ok(true) => errors.push(validation_issue(
                CandidateValidationError::CandidateExpired.code(),
                "Candidate TTL has expired.",
                "Create or review a fresh curation candidate.",
            )),
            Ok(false) => {}
            Err(message) => errors.push(validation_issue(
                "invalid_ttl_timestamp",
                message,
                "Store ttl_expires_at as an RFC 3339 timestamp.",
            )),
        }
    }

    validate_target_memory(stored, target_memory, &mut errors);

    let candidate_type = match CandidateType::from_str(&stored.candidate_type) {
        Ok(candidate_type) => candidate_type,
        Err(error) => {
            errors.push(validation_issue(
                "invalid_candidate_type",
                error.to_string(),
                "Regenerate the candidate with a supported candidate type.",
            ));
            return blocked_apply(
                stored,
                target_before,
                errors,
                warnings,
                "ee curate candidates --json".to_owned(),
            );
        }
    };

    let Some(target_memory) = target_memory else {
        return blocked_apply(
            stored,
            target_before,
            errors,
            warnings,
            "no action required".to_owned(),
        );
    };

    if !errors.is_empty() {
        return blocked_apply(
            stored,
            target_before,
            errors,
            warnings,
            "no action required".to_owned(),
        );
    }

    let mut target_after = memory_state_from_stored(target_memory);
    let mut changes = Vec::new();
    let mut memory_update = None;
    let mut tombstone_memory = false;

    match candidate_type {
        CandidateType::Tombstone | CandidateType::Retract => {
            push_apply_change(
                &mut changes,
                "tombstoned",
                Some("false".to_owned()),
                Some("true".to_owned()),
            );
            target_after.tombstoned = true;
            tombstone_memory = true;
        }
        CandidateType::Consolidate
        | CandidateType::Supersede
        | CandidateType::Merge
        | CandidateType::Split => {
            let proposed_content = stored.proposed_content.as_deref().map(str::trim);
            match proposed_content.filter(|value| !value.is_empty()) {
                Some(content) => {
                    push_apply_change(
                        &mut changes,
                        "content",
                        Some(target_memory.content.clone()),
                        Some(content.to_owned()),
                    );
                    target_after.content = content.to_owned();
                }
                None => errors.push(validation_issue(
                    CandidateValidationError::ContentRequiredForType { candidate_type }.code(),
                    format!("proposed content is required for {candidate_type} candidates"),
                    "Validate or recreate the candidate with proposed content.",
                )),
            }
        }
        CandidateType::Promote | CandidateType::Deprecate => {
            if stored.proposed_content.is_some() {
                warnings.push(validation_issue(
                    "proposed_content_ignored_for_type",
                    format!("Proposed content is ignored for {candidate_type} candidates."),
                    "Use consolidate, supersede, merge, or split when content should change.",
                ));
            }
        }
    }

    if let Some(confidence) = stored.proposed_confidence {
        push_apply_change(
            &mut changes,
            "confidence",
            Some(format_score(target_memory.confidence)),
            Some(format_score(confidence)),
        );
        target_after.confidence = confidence;
    }
    if let Some(trust_class) = &stored.proposed_trust_class {
        push_apply_change(
            &mut changes,
            "trustClass",
            Some(target_memory.trust_class.clone()),
            Some(trust_class.clone()),
        );
        target_after.trust_class = trust_class.clone();
    }

    if !errors.is_empty() {
        return blocked_apply(
            stored,
            target_before,
            errors,
            warnings,
            "ee curate validate <CANDIDATE_ID>".to_owned(),
        );
    }

    if changes.is_empty() {
        errors.push(validation_issue(
            "curation_candidate_no_effect",
            "Candidate does not change the target memory.",
            "Reject the candidate or recreate it with a concrete memory mutation.",
        ));
        return blocked_apply(
            stored,
            target_before,
            errors,
            warnings,
            "no action required".to_owned(),
        );
    }

    if !tombstone_memory {
        memory_update = Some(ApplyMemoryCurationInput {
            workspace_id: stored.workspace_id.clone(),
            content: target_after.content.clone(),
            confidence: target_after.confidence,
            trust_class: target_after.trust_class.clone(),
        });
    }

    ApplyDecision {
        application: CurateApplyResult {
            status: "ready".to_owned(),
            decision: if tombstone_memory {
                "tombstone_memory".to_owned()
            } else {
                "update_memory".to_owned()
            },
            candidate_type: candidate_type.as_str().to_owned(),
            target_memory_id: stored.target_memory_id.clone(),
            changes,
            errors,
            warnings,
        },
        to_status: CandidateStatus::Applied.as_str().to_owned(),
        should_persist: current_status
            .is_some_and(|status| status.can_transition_to(CandidateStatus::Applied)),
        memory_update,
        tombstone_memory,
        target_before,
        target_after: Some(target_after),
        next_action: "no action required".to_owned(),
    }
}

fn evaluate_candidate_for_review(
    stored: &StoredCurationCandidate,
    action: CurateReviewAction,
    snoozed_until: Option<&str>,
    merge_into_candidate_id: Option<&str>,
    merge_target: Option<&StoredCurationCandidate>,
    now_rfc3339: &str,
) -> ReviewDecision {
    let mut errors = Vec::new();
    let warnings = Vec::new();
    let current_status = parse_stored_status(&stored.status, &mut errors);
    let current_review_state = parse_stored_review_state(&stored.review_state, &mut errors);
    let target_status = target_status_for_review_action(action);
    let target_review_state = target_review_state_for_review_action(action);
    let target_status_text = target_status.as_str().to_owned();
    let target_review_state_text = target_review_state.as_str().to_owned();
    let snoozed_until = snoozed_until.map(str::to_owned);
    let merged_into_candidate_id = merge_into_candidate_id.map(str::to_owned);

    if review_action_already_done(
        stored,
        action,
        &target_status_text,
        &target_review_state_text,
        snoozed_until.as_deref(),
        merged_into_candidate_id.as_deref(),
    ) {
        return unchanged_review(
            action,
            stored,
            format!("already_{}", action.as_str()),
            "unchanged".to_owned(),
            warnings,
        );
    }

    if let Some(status) = current_status
        && status.is_terminal()
    {
        errors.push(validation_issue(
            "candidate_status_terminal",
            format!(
                "Candidate is already in terminal status {}.",
                status.as_str()
            ),
            "No review transition is available for terminal candidates.",
        ));
    }

    if let Some(review_state) = current_review_state {
        if let Err(error) = validate_review_queue_transition(review_state, target_review_state) {
            errors.push(validation_issue(
                error.code(),
                error.to_string(),
                "Refresh the review queue and choose an eligible candidate.",
            ));
        }
    }

    if let Some(status) = current_status
        && status != target_status
        && !status.can_transition_to(target_status)
    {
        errors.push(validation_issue(
            CandidateValidationError::InvalidStatusTransition {
                from: status,
                to: target_status,
            }
            .code(),
            format!("cannot transition from {status} to {target_status}"),
            "Refresh the review queue and choose an eligible candidate.",
        ));
    }

    if action == CurateReviewAction::Snooze {
        if let Some(until) = snoozed_until.as_deref() {
            match timestamp_has_expired(until, now_rfc3339) {
                Ok(true) => errors.push(validation_issue(
                    "snooze_until_not_future",
                    "Snooze timestamp must be later than the current review time.",
                    "Pass a future RFC 3339 timestamp to --until.",
                )),
                Ok(false) => {}
                Err(message) => errors.push(validation_issue(
                    "invalid_snooze_until",
                    message,
                    "Pass --until as an RFC 3339 timestamp.",
                )),
            }
        } else {
            errors.push(validation_issue(
                "snooze_until_required",
                "Snooze requires an --until timestamp.",
                "Run `ee curate snooze <candidate-id> --until <RFC3339>`.",
            ));
        }
    }

    if action == CurateReviewAction::Merge {
        if merge_target.is_none() {
            errors.push(validation_issue(
                "merge_target_missing",
                "Merge requires an existing target curation candidate.",
                "Run `ee curate candidates --all --json` and choose a target candidate.",
            ));
        }
        if merged_into_candidate_id.as_deref() == Some(stored.id.as_str()) {
            errors.push(validation_issue(
                "merge_target_self",
                "A curation candidate cannot be merged into itself.",
                "Choose a different merge target candidate.",
            ));
        }
    }

    if !errors.is_empty() {
        return blocked_review(
            action,
            stored,
            target_status_text,
            target_review_state_text,
            errors,
            warnings,
        );
    }

    let should_persist = current_status.is_some_and(|status| status != target_status)
        || current_review_state.is_some_and(|state| state != target_review_state)
        || stored.snoozed_until.as_deref() != snoozed_until.as_deref()
        || stored.merged_into_candidate_id.as_deref() != merged_into_candidate_id.as_deref();
    let next_action = next_action_for_review_transition(
        stored,
        action,
        &target_status_text,
        &target_review_state_text,
        snoozed_until.as_deref(),
    );

    ReviewDecision {
        review: CurateReviewResult {
            status: if should_persist { "ready" } else { "unchanged" }.to_owned(),
            decision: action.as_str().to_owned(),
            action: action.as_str().to_owned(),
            errors,
            warnings,
        },
        to_status: target_status_text,
        to_review_state: target_review_state_text,
        should_persist,
        snoozed_until,
        merged_into_candidate_id,
        next_action,
    }
}

fn parse_stored_status(
    raw: &str,
    errors: &mut Vec<CurateValidationIssue>,
) -> Option<CandidateStatus> {
    match CandidateStatus::from_str(raw) {
        Ok(status) => Some(status),
        Err(error) => {
            errors.push(validation_issue(
                "invalid_candidate_status",
                error.to_string(),
                "Regenerate the candidate with a supported status.",
            ));
            None
        }
    }
}

fn parse_stored_review_state(
    raw: &str,
    errors: &mut Vec<CurateValidationIssue>,
) -> Option<ReviewQueueState> {
    match ReviewQueueState::from_str(raw) {
        Ok(state) => Some(state),
        Err(error) => {
            errors.push(validation_issue(
                "invalid_review_state",
                error.to_string(),
                "Regenerate or migrate the candidate with a supported review state.",
            ));
            None
        }
    }
}

const fn target_status_for_review_action(action: CurateReviewAction) -> CandidateStatus {
    match action {
        CurateReviewAction::Accept => CandidateStatus::Approved,
        CurateReviewAction::Reject | CurateReviewAction::Merge => CandidateStatus::Rejected,
        CurateReviewAction::Snooze => CandidateStatus::Pending,
    }
}

const fn target_review_state_for_review_action(action: CurateReviewAction) -> ReviewQueueState {
    match action {
        CurateReviewAction::Accept => ReviewQueueState::Accepted,
        CurateReviewAction::Reject => ReviewQueueState::Rejected,
        CurateReviewAction::Snooze => ReviewQueueState::Snoozed,
        CurateReviewAction::Merge => ReviewQueueState::Merged,
    }
}

fn review_action_already_done(
    stored: &StoredCurationCandidate,
    action: CurateReviewAction,
    target_status: &str,
    target_review_state: &str,
    snoozed_until: Option<&str>,
    merged_into_candidate_id: Option<&str>,
) -> bool {
    match action {
        CurateReviewAction::Accept | CurateReviewAction::Reject => {
            stored.status == target_status && stored.review_state == target_review_state
        }
        CurateReviewAction::Snooze => {
            stored.status == target_status
                && stored.review_state == target_review_state
                && stored.snoozed_until.as_deref() == snoozed_until
        }
        CurateReviewAction::Merge => {
            stored.status == target_status
                && stored.review_state == target_review_state
                && stored.merged_into_candidate_id.as_deref() == merged_into_candidate_id
        }
    }
}

fn unchanged_review(
    action: CurateReviewAction,
    stored: &StoredCurationCandidate,
    status: String,
    decision: String,
    warnings: Vec<CurateValidationIssue>,
) -> ReviewDecision {
    ReviewDecision {
        review: CurateReviewResult {
            status,
            decision,
            action: action.as_str().to_owned(),
            errors: Vec::new(),
            warnings,
        },
        to_status: stored.status.clone(),
        to_review_state: stored.review_state.clone(),
        should_persist: false,
        snoozed_until: stored.snoozed_until.clone(),
        merged_into_candidate_id: stored.merged_into_candidate_id.clone(),
        next_action: next_action_for_candidate_fields(
            &stored.id,
            &stored.status,
            &stored.review_state,
            stored.snoozed_until.as_deref(),
        ),
    }
}

fn blocked_review(
    action: CurateReviewAction,
    stored: &StoredCurationCandidate,
    _to_status: String,
    _to_review_state: String,
    errors: Vec<CurateValidationIssue>,
    warnings: Vec<CurateValidationIssue>,
) -> ReviewDecision {
    ReviewDecision {
        review: CurateReviewResult {
            status: "blocked".to_owned(),
            decision: "unchanged".to_owned(),
            action: action.as_str().to_owned(),
            errors,
            warnings,
        },
        to_status: stored.status.clone(),
        to_review_state: stored.review_state.clone(),
        should_persist: false,
        snoozed_until: stored.snoozed_until.clone(),
        merged_into_candidate_id: stored.merged_into_candidate_id.clone(),
        next_action: next_action_for_candidate_fields(
            &stored.id,
            &stored.status,
            &stored.review_state,
            stored.snoozed_until.as_deref(),
        ),
    }
}

fn next_action_for_review_transition(
    stored: &StoredCurationCandidate,
    action: CurateReviewAction,
    status: &str,
    review_state: &str,
    snoozed_until: Option<&str>,
) -> String {
    match action {
        CurateReviewAction::Accept => format!("ee curate apply {} --json", stored.id),
        CurateReviewAction::Reject | CurateReviewAction::Merge => "no action required".to_owned(),
        CurateReviewAction::Snooze => {
            next_action_for_candidate_fields(&stored.id, status, review_state, snoozed_until)
        }
    }
}

fn evaluate_candidate_for_disposition(
    stored: &StoredCurationCandidate,
    policies: &BTreeMap<&str, &StoredCurationTtlPolicy>,
    now: &DateTime<Utc>,
    apply: bool,
    actor: &str,
    connection: &DbConnection,
    degraded: &mut Vec<CurateCandidatesDegradation>,
) -> Result<CurateDispositionDecision, DomainError> {
    let review_state = normalized_review_state(stored);
    let policy_id = stored
        .ttl_policy_id
        .as_deref()
        .unwrap_or_else(|| default_curation_ttl_policy_id_for_review_state(&review_state));
    let Some(policy) = policies.get(policy_id).copied() else {
        degraded.push(CurateCandidatesDegradation {
            code: "curation_ttl_policy_missing".to_owned(),
            severity: "medium".to_owned(),
            message: format!(
                "Candidate {} references missing TTL policy {policy_id}.",
                stored.id
            ),
            repair: "Run ee db migrate --json or recreate the curation policy table.".to_owned(),
        });
        return Ok(blocked_disposition(
            stored,
            policy_id,
            &review_state,
            "policy_missing",
            "Candidate TTL policy is missing.",
            "Run ee db migrate --json.",
        ));
    };

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let entered_raw = stored
        .state_entered_at
        .as_deref()
        .or(stored.reviewed_at.as_deref())
        .or(stored.applied_at.as_deref())
        .unwrap_or(stored.created_at.as_str());
    let state_entered = match DateTime::parse_from_rfc3339(entered_raw) {
        Ok(value) => value.with_timezone(&Utc),
        Err(error) => {
            return Ok(blocked_disposition(
                stored,
                policy_id,
                &review_state,
                "invalid_state_entered_at",
                &format!("Invalid curation state_entered_at `{entered_raw}`: {error}"),
                "Repair the curation candidate timestamp or recreate the candidate.",
            ));
        }
    };

    let threshold = duration_from_seconds(policy.threshold_seconds, "threshold_seconds")?;
    let due_at = state_entered + threshold;
    let elapsed = now.signed_duration_since(state_entered).num_seconds();
    let evidence_count = u32::from(stored.source_id.is_some());
    let distinct_session_count = distinct_session_count(stored);
    let mut transition = None;
    let mut audit = None;

    if elapsed < 0 {
        warnings.push(validation_issue(
            "curation_candidate_clock_drift",
            "Candidate state timestamp is in the future.",
            "Check system clocks before applying TTL disposition.",
        ));
        return Ok(CurateDispositionDecision {
            candidate_id: stored.id.clone(),
            policy_id: policy.id.clone(),
            review_state,
            status: stored.status.clone(),
            action: policy.action.clone(),
            decision: "clock_drift".to_owned(),
            state_entered_at: Some(entered_raw.to_owned()),
            due_at: Some(due_at.to_rfc3339()),
            ttl_elapsed_seconds: Some(elapsed),
            ttl_threshold_seconds: policy.threshold_seconds,
            evidence_count,
            distinct_session_count,
            auto_promote_enabled: policy.auto_promote_enabled,
            gate_status: "blocked".to_owned(),
            planned_transition: transition,
            audit,
            errors,
            warnings,
        });
    }

    if due_at > *now {
        return Ok(CurateDispositionDecision {
            candidate_id: stored.id.clone(),
            policy_id: policy.id.clone(),
            review_state,
            status: stored.status.clone(),
            action: policy.action.clone(),
            decision: "not_due".to_owned(),
            state_entered_at: Some(entered_raw.to_owned()),
            due_at: Some(due_at.to_rfc3339()),
            ttl_elapsed_seconds: Some(elapsed),
            ttl_threshold_seconds: policy.threshold_seconds,
            evidence_count,
            distinct_session_count,
            auto_promote_enabled: policy.auto_promote_enabled,
            gate_status: "not_evaluated".to_owned(),
            planned_transition: transition,
            audit,
            errors,
            warnings,
        });
    }

    let (decision, gate_status, target) = match policy.action.as_str() {
        "snooze" => (
            if apply { "applied" } else { "planned" },
            "passed",
            Some((
                CandidateStatus::Pending.as_str(),
                ReviewQueueState::Snoozed.as_str(),
                Some(
                    (now.to_owned()
                        + duration_from_seconds(DEFAULT_SNOOZE_SECONDS, "default_snooze_seconds")?)
                    .to_rfc3339(),
                ),
                default_curation_ttl_policy_id_for_review_state(ReviewQueueState::Snoozed.as_str()),
            )),
        ),
        "retire_with_audit" => (
            if apply { "applied" } else { "planned" },
            "passed",
            Some((
                CandidateStatus::Expired.as_str(),
                ReviewQueueState::Expired.as_str(),
                None,
                default_curation_ttl_policy_id_for_review_state(ReviewQueueState::Expired.as_str()),
            )),
        ),
        "prompt_promote" => {
            if !policy.auto_promote_enabled {
                warnings.push(validation_issue(
                    "auto_promote_disabled",
                    "Validated candidate reached its TTL, but auto-promote is disabled by policy.",
                    format!(
                        "Review manually with `ee curate apply {} --json`.",
                        stored.id
                    ),
                ));
            } else if evidence_count < policy.requires_evidence_count
                || distinct_session_count < policy.requires_distinct_sessions
            {
                warnings.push(validation_issue(
                    "auto_promote_evidence_gate",
                    "Validated candidate reached its TTL but lacks enough distinct evidence.",
                    "Collect more helpful outcomes before enabling promotion.",
                ));
            }
            ("prompt", "auto_prompt", None)
        }
        "escalate" => {
            degraded.push(CurateCandidatesDegradation {
                code: "curation_harmful_candidate_escalated".to_owned(),
                severity: "high".to_owned(),
                message: format!(
                    "Curation candidate {} requires harmful-feedback review.",
                    stored.id
                ),
                repair: format!(
                    "Resolve with `ee curate reject {} --json` or a replacement candidate.",
                    stored.id
                ),
            });
            ("escalated", "requires_human", None)
        }
        _ => {
            errors.push(validation_issue(
                "unknown_curation_ttl_action",
                format!("Unknown curation TTL action `{}`.", policy.action),
                "Repair the curation_ttl_policies table.",
            ));
            ("blocked", "blocked", None)
        }
    };

    if let Some((to_status, to_review_state, snoozed_until, ttl_policy_id)) = target {
        transition = Some(CurateDispositionTransition {
            from_status: stored.status.clone(),
            to_status: to_status.to_owned(),
            from_review_state: stored.review_state.clone(),
            to_review_state: to_review_state.to_owned(),
            snoozed_until: snoozed_until.clone(),
            ttl_policy_id: ttl_policy_id.to_owned(),
            persisted: false,
        });
        audit = Some(CurateDispositionAuditPlan {
            action: audit_actions::CURATION_CANDIDATE_DISPOSITION.to_owned(),
            target_type: "curation_candidate".to_owned(),
            target_id: stored.id.clone(),
            audit_id: None,
        });

        if apply && errors.is_empty() {
            let audit_id = persist_candidate_disposition(
                connection,
                stored,
                policy,
                to_status,
                to_review_state,
                snoozed_until.as_deref(),
                ttl_policy_id,
                now,
                actor,
                evidence_count,
                distinct_session_count,
            )?;
            if let Some(transition) = &mut transition {
                transition.persisted = true;
            }
            if let Some(audit) = &mut audit {
                audit.audit_id = Some(audit_id);
            }
        }
    }

    Ok(CurateDispositionDecision {
        candidate_id: stored.id.clone(),
        policy_id: policy.id.clone(),
        review_state,
        status: stored.status.clone(),
        action: policy.action.clone(),
        decision: decision.to_owned(),
        state_entered_at: Some(entered_raw.to_owned()),
        due_at: Some(due_at.to_rfc3339()),
        ttl_elapsed_seconds: Some(elapsed),
        ttl_threshold_seconds: policy.threshold_seconds,
        evidence_count,
        distinct_session_count,
        auto_promote_enabled: policy.auto_promote_enabled,
        gate_status: gate_status.to_owned(),
        planned_transition: transition,
        audit,
        errors,
        warnings,
    })
}

fn blocked_disposition(
    stored: &StoredCurationCandidate,
    policy_id: &str,
    review_state: &str,
    code: &str,
    message: &str,
    repair: &str,
) -> CurateDispositionDecision {
    CurateDispositionDecision {
        candidate_id: stored.id.clone(),
        policy_id: policy_id.to_owned(),
        review_state: review_state.to_owned(),
        status: stored.status.clone(),
        action: "unknown".to_owned(),
        decision: "blocked".to_owned(),
        state_entered_at: stored.state_entered_at.clone(),
        due_at: None,
        ttl_elapsed_seconds: None,
        ttl_threshold_seconds: 0,
        evidence_count: u32::from(stored.source_id.is_some()),
        distinct_session_count: distinct_session_count(stored),
        auto_promote_enabled: false,
        gate_status: "blocked".to_owned(),
        planned_transition: None,
        audit: None,
        errors: vec![validation_issue(code, message, repair)],
        warnings: Vec::new(),
    }
}

fn distinct_session_count(stored: &StoredCurationCandidate) -> u32 {
    if stored.source_type == "agent_inference" || stored.source_type == "feedback_event" {
        u32::from(stored.source_id.is_some())
    } else {
        0
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_candidate_disposition(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    policy: &StoredCurationTtlPolicy,
    to_status: &str,
    to_review_state: &str,
    snoozed_until: Option<&str>,
    ttl_policy_id: &str,
    now: &DateTime<Utc>,
    actor: &str,
    evidence_count: u32,
    distinct_session_count: u32,
) -> Result<String, DomainError> {
    connection.begin().map_err(|error| DomainError::Storage {
        message: format!("Failed to begin curation disposition transaction: {error}"),
        repair: Some("ee doctor".to_owned()),
    })?;

    let result = persist_candidate_disposition_inner(
        connection,
        stored,
        policy,
        to_status,
        to_review_state,
        snoozed_until,
        ttl_policy_id,
        now,
        actor,
        evidence_count,
        distinct_session_count,
    );

    match result {
        Ok(audit_id) => {
            connection.commit().map_err(|error| DomainError::Storage {
                message: format!("Failed to commit curation disposition: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?;
            Ok(audit_id)
        }
        Err(error) => {
            let _ = connection.rollback();
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_candidate_disposition_inner(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    policy: &StoredCurationTtlPolicy,
    to_status: &str,
    to_review_state: &str,
    snoozed_until: Option<&str>,
    ttl_policy_id: &str,
    now: &DateTime<Utc>,
    actor: &str,
    evidence_count: u32,
    distinct_session_count: u32,
) -> Result<String, DomainError> {
    let acted_at = now.to_rfc3339();
    let updated = connection
        .update_curation_candidate_review(
            &stored.workspace_id,
            &stored.id,
            CurationCandidateReviewUpdate {
                status: to_status,
                review_state: to_review_state,
                reviewed_at: &acted_at,
                reviewed_by: actor,
                snoozed_until,
                merged_into_candidate_id: stored.merged_into_candidate_id.as_deref(),
                ttl_policy_id: Some(ttl_policy_id),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to update curation disposition: {error}"),
            repair: Some("ee curate disposition --json".to_owned()),
        })?;
    if !updated {
        return Err(DomainError::NotFound {
            resource: "curation candidate".to_owned(),
            id: stored.id.clone(),
            repair: Some("ee curate candidates --all --json".to_owned()),
        });
    }

    let elapsed = stored
        .state_entered_at
        .as_deref()
        .and_then(|entered| DateTime::parse_from_rfc3339(entered).ok())
        .map(|entered| {
            now.signed_duration_since(entered.with_timezone(&Utc))
                .num_milliseconds()
        })
        .unwrap_or(0);
    let audit_id = generate_audit_id();
    let details = serde_json::json!({
        "schema": "ee.audit.curation_disposition.v1",
        "candidateId": stored.id.as_str(),
        "policyId": policy.id.as_str(),
        "fromStatus": stored.status.as_str(),
        "toStatus": to_status,
        "fromReviewState": stored.review_state.as_str(),
        "toReviewState": to_review_state,
        "ttlElapsedMs": elapsed,
        "ttlPolicyId": policy.id.as_str(),
        "ttlThresholdSeconds": policy.threshold_seconds,
        "evidenceCount": evidence_count,
        "distinctSessionCount": distinct_session_count,
        "deterministicRule": policy.action.as_str(),
    })
    .to_string();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(stored.workspace_id.clone()),
                actor: Some(actor.to_owned()),
                action: audit_actions::CURATION_CANDIDATE_DISPOSITION.to_owned(),
                target_type: Some("curation_candidate".to_owned()),
                target_id: Some(stored.id.clone()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to write curation disposition audit entry: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn disposition_summary(
    decisions: &[CurateDispositionDecision],
    total_candidates: usize,
) -> CurateDispositionSummary {
    let due = decisions
        .iter()
        .filter(|decision| decision.decision != "not_due")
        .count();
    let applied = decisions
        .iter()
        .filter(|decision| {
            decision
                .planned_transition
                .as_ref()
                .is_some_and(|transition| transition.persisted)
        })
        .count();
    let prompts = decisions
        .iter()
        .filter(|decision| decision.decision == "prompt")
        .count();
    let escalations = decisions
        .iter()
        .filter(|decision| decision.decision == "escalated")
        .count();
    let blocked = decisions
        .iter()
        .filter(|decision| decision.decision == "blocked" || decision.decision == "clock_drift")
        .count();
    let next_scheduled_at = decisions
        .iter()
        .filter(|decision| decision.decision == "not_due")
        .filter_map(|decision| decision.due_at.clone())
        .min();

    CurateDispositionSummary {
        total_candidates,
        due_count: due,
        applied_count: applied,
        prompt_count: prompts,
        escalation_count: escalations,
        blocked_count: blocked,
        next_scheduled_at,
    }
}

fn policy_summary(policy: &StoredCurationTtlPolicy) -> CurateTtlPolicySummary {
    CurateTtlPolicySummary {
        id: policy.id.clone(),
        review_state: policy.review_state.clone(),
        threshold_seconds: policy.threshold_seconds,
        action: policy.action.clone(),
        requires_evidence_count: policy.requires_evidence_count,
        requires_distinct_sessions: policy.requires_distinct_sessions,
        requires_no_harmful_within_seconds: policy.requires_no_harmful_within_seconds,
        auto_promote_enabled: policy.auto_promote_enabled,
    }
}

fn blocked_apply(
    stored: &StoredCurationCandidate,
    target_before: Option<CurateApplyMemoryState>,
    errors: Vec<CurateValidationIssue>,
    warnings: Vec<CurateValidationIssue>,
    next_action: String,
) -> ApplyDecision {
    ApplyDecision {
        application: CurateApplyResult {
            status: "blocked".to_owned(),
            decision: "unchanged".to_owned(),
            candidate_type: stored.candidate_type.clone(),
            target_memory_id: stored.target_memory_id.clone(),
            changes: Vec::new(),
            errors,
            warnings,
        },
        to_status: stored.status.clone(),
        should_persist: false,
        memory_update: None,
        tombstone_memory: false,
        target_before: target_before.clone(),
        target_after: target_before,
        next_action,
    }
}

fn blocked_validation(
    stored: &StoredCurationCandidate,
    errors: Vec<CurateValidationIssue>,
    warnings: Vec<CurateValidationIssue>,
) -> ValidationDecision {
    ValidationDecision {
        validation: CurateValidateResult {
            status: "blocked".to_owned(),
            decision: "unchanged".to_owned(),
            errors,
            warnings,
        },
        to_status: stored.status.clone(),
        should_persist: false,
        next_action: "no action required".to_owned(),
    }
}

fn validate_target_memory(
    stored: &StoredCurationCandidate,
    target_memory: Option<&StoredMemory>,
    errors: &mut Vec<CurateValidationIssue>,
) {
    match target_memory {
        Some(memory) if memory.workspace_id != stored.workspace_id => {
            errors.push(validation_issue(
                "target_memory_workspace_mismatch",
                format!(
                    "Target memory {} belongs to workspace {}, not {}.",
                    memory.id, memory.workspace_id, stored.workspace_id
                ),
                "Regenerate the candidate for the correct workspace.",
            ))
        }
        Some(memory) if memory.tombstoned_at.is_some() => errors.push(validation_issue(
            "target_memory_tombstoned",
            format!("Target memory {} is tombstoned.", memory.id),
            "Reject this candidate or create a candidate for an active memory.",
        )),
        Some(_) => {}
        None => errors.push(validation_issue(
            "target_memory_missing",
            format!("Target memory {} does not exist.", stored.target_memory_id),
            "Reject this candidate or recreate the missing memory first.",
        )),
    }
}

fn timestamp_has_expired(expires_at: &str, now_rfc3339: &str) -> Result<bool, String> {
    let expires = DateTime::parse_from_rfc3339(expires_at)
        .map_err(|error| format!("Invalid ttl_expires_at `{expires_at}`: {error}"))?
        .with_timezone(&Utc);
    let now = DateTime::parse_from_rfc3339(now_rfc3339)
        .map_err(|error| format!("Invalid validation timestamp `{now_rfc3339}`: {error}"))?
        .with_timezone(&Utc);
    Ok(expires <= now)
}

fn parse_or_current_time(raw: Option<&str>) -> Result<DateTime<Utc>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|error| {
                    curate_usage_error(
                        format!("invalid --now timestamp `{value}`: {error}"),
                        "ee curate disposition --help",
                    )
                })
        })
        .transpose()
        .map(|timestamp| timestamp.unwrap_or_else(Utc::now))
}

fn duration_from_seconds(seconds: u64, field: &str) -> Result<chrono::Duration, DomainError> {
    let seconds = i64::try_from(seconds).map_err(|_| DomainError::Storage {
        message: format!("Curation TTL {field} exceeds supported duration range."),
        repair: Some("Repair the curation_ttl_policies table.".to_owned()),
    })?;
    Ok(chrono::Duration::seconds(seconds))
}

fn validation_issue(
    code: impl Into<String>,
    message: impl Into<String>,
    repair: impl Into<String>,
) -> CurateValidationIssue {
    CurateValidationIssue {
        code: code.into(),
        message: message.into(),
        repair: repair.into(),
    }
}

fn validation_repair(error: &CandidateValidationError) -> &'static str {
    match error {
        CandidateValidationError::EmptyWorkspaceId
        | CandidateValidationError::EmptyTargetMemoryId
        | CandidateValidationError::EmptyReason
        | CandidateValidationError::MissingSourceEvidence => {
            "Regenerate the candidate with all required fields populated."
        }
        CandidateValidationError::ConfidenceOutOfRange { .. }
        | CandidateValidationError::ProposedConfidenceOutOfRange { .. } => {
            "Use confidence values between 0.0 and 1.0."
        }
        CandidateValidationError::InvalidProposedTrustClass { .. } => {
            "Use a supported trust class."
        }
        CandidateValidationError::ContentRequiredForType { .. } => {
            "Add proposed content before validating this candidate."
        }
        CandidateValidationError::ContentForbiddenForType { .. } => {
            "Remove proposed content for this candidate type."
        }
        CandidateValidationError::CandidateTooGeneric { .. } => {
            "Add concrete commands, files, error codes, metrics, or provenance."
        }
        CandidateValidationError::PromptInjectionFlagged { .. } => {
            "Quarantine the source evidence and recreate the candidate from trusted spans."
        }
        CandidateValidationError::InvalidStatusTransition { .. } => {
            "Refresh the queue and validate an eligible candidate."
        }
        CandidateValidationError::CandidateExpired => "Create or review a fresh candidate.",
        CandidateValidationError::CandidateAlreadyTerminal { .. } => {
            "No validation action is available for terminal candidates."
        }
    }
}

fn memory_state_from_stored(memory: &StoredMemory) -> CurateApplyMemoryState {
    CurateApplyMemoryState {
        id: memory.id.clone(),
        content: memory.content.clone(),
        confidence: memory.confidence,
        trust_class: memory.trust_class.clone(),
        tombstoned: memory.tombstoned_at.is_some(),
    }
}

fn push_apply_change(
    changes: &mut Vec<CurateApplyChange>,
    field: &str,
    before: Option<String>,
    after: Option<String>,
) {
    if before != after {
        changes.push(CurateApplyChange {
            field: field.to_owned(),
            before,
            after,
        });
    }
}

fn format_score(value: f32) -> String {
    format!("{value:.6}")
}

fn persist_candidate_validation(
    connection: &DbConnection,
    workspace_id: &str,
    stored: &StoredCurationCandidate,
    to_status: &str,
    reviewed_at: &str,
    reviewed_by: &str,
    decision: &ValidationDecision,
) -> Result<String, DomainError> {
    connection.begin().map_err(|error| DomainError::Storage {
        message: format!("Failed to begin curation validation transaction: {error}"),
        repair: Some("ee doctor".to_owned()),
    })?;

    let result = persist_candidate_validation_inner(
        connection,
        workspace_id,
        stored,
        to_status,
        reviewed_at,
        reviewed_by,
        decision,
    );

    match result {
        Ok(audit_id) => {
            connection.commit().map_err(|error| DomainError::Storage {
                message: format!("Failed to commit curation validation: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?;
            Ok(audit_id)
        }
        Err(error) => {
            let _ = connection.rollback();
            Err(error)
        }
    }
}

fn persist_candidate_validation_inner(
    connection: &DbConnection,
    workspace_id: &str,
    stored: &StoredCurationCandidate,
    to_status: &str,
    reviewed_at: &str,
    reviewed_by: &str,
    decision: &ValidationDecision,
) -> Result<String, DomainError> {
    let updated = connection
        .update_curation_candidate_review(
            workspace_id,
            &stored.id,
            CurationCandidateReviewUpdate {
                status: to_status,
                review_state: review_state_for_status_text(to_status),
                reviewed_at,
                reviewed_by,
                snoozed_until: None,
                merged_into_candidate_id: None,
                ttl_policy_id: None,
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to update curation candidate review: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    if !updated {
        return Err(DomainError::NotFound {
            resource: "curation candidate".to_owned(),
            id: stored.id.clone(),
            repair: Some("ee curate candidates --json".to_owned()),
        });
    }

    let audit_id = generate_audit_id();
    let details = serde_json::json!({
        "fromStatus": stored.status.as_str(),
        "toStatus": to_status,
        "fromReviewState": stored.review_state.as_str(),
        "toReviewState": review_state_for_status_text(to_status),
        "validationStatus": decision.validation.status.as_str(),
        "decision": decision.validation.decision.as_str(),
        "errorCodes": decision.validation.errors.iter().map(|issue| issue.code.as_str()).collect::<Vec<_>>(),
        "warningCodes": decision.validation.warnings.iter().map(|issue| issue.code.as_str()).collect::<Vec<_>>(),
    })
    .to_string();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some(reviewed_by.to_owned()),
                action: audit_actions::CURATION_CANDIDATE_VALIDATE.to_owned(),
                target_type: Some("curation_candidate".to_owned()),
                target_id: Some(stored.id.clone()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to write curation validation audit entry: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn persist_candidate_review(
    connection: &DbConnection,
    workspace_id: &str,
    stored: &StoredCurationCandidate,
    action: CurateReviewAction,
    decision: &ReviewDecision,
    reviewed_at: &str,
    reviewed_by: &str,
) -> Result<String, DomainError> {
    connection.begin().map_err(|error| DomainError::Storage {
        message: format!("Failed to begin curation review transaction: {error}"),
        repair: Some("ee doctor".to_owned()),
    })?;

    let result = persist_candidate_review_inner(
        connection,
        workspace_id,
        stored,
        action,
        decision,
        reviewed_at,
        reviewed_by,
    );

    match result {
        Ok(audit_id) => {
            connection.commit().map_err(|error| DomainError::Storage {
                message: format!("Failed to commit curation review: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?;
            Ok(audit_id)
        }
        Err(error) => {
            let _ = connection.rollback();
            Err(error)
        }
    }
}

fn persist_candidate_review_inner(
    connection: &DbConnection,
    workspace_id: &str,
    stored: &StoredCurationCandidate,
    action: CurateReviewAction,
    decision: &ReviewDecision,
    reviewed_at: &str,
    reviewed_by: &str,
) -> Result<String, DomainError> {
    let updated = connection
        .update_curation_candidate_review(
            workspace_id,
            &stored.id,
            CurationCandidateReviewUpdate {
                status: &decision.to_status,
                review_state: &decision.to_review_state,
                reviewed_at,
                reviewed_by,
                snoozed_until: decision.snoozed_until.as_deref(),
                merged_into_candidate_id: decision.merged_into_candidate_id.as_deref(),
                ttl_policy_id: None,
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to update curation candidate review state: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    if !updated {
        return Err(DomainError::NotFound {
            resource: "curation candidate".to_owned(),
            id: stored.id.clone(),
            repair: Some("ee curate candidates --json".to_owned()),
        });
    }

    let audit_id = generate_audit_id();
    let details = serde_json::json!({
        "candidateId": stored.id.as_str(),
        "action": action.as_str(),
        "fromStatus": stored.status.as_str(),
        "toStatus": decision.to_status.as_str(),
        "fromReviewState": stored.review_state.as_str(),
        "toReviewState": decision.to_review_state.as_str(),
        "snoozedUntil": decision.snoozed_until.as_deref(),
        "mergedIntoCandidateId": decision.merged_into_candidate_id.as_deref(),
        "decision": decision.review.decision.as_str(),
    })
    .to_string();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some(reviewed_by.to_owned()),
                action: action.audit_action().to_owned(),
                target_type: Some("curation_candidate".to_owned()),
                target_id: Some(stored.id.clone()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to write curation review audit entry: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn persist_candidate_application(
    connection: &DbConnection,
    workspace_id: &str,
    stored: &StoredCurationCandidate,
    decision: &ApplyDecision,
    applied_at: &str,
    applied_by: &str,
) -> Result<String, DomainError> {
    connection.begin().map_err(|error| DomainError::Storage {
        message: format!("Failed to begin curation apply transaction: {error}"),
        repair: Some("ee doctor".to_owned()),
    })?;

    let result = persist_candidate_application_inner(
        connection,
        workspace_id,
        stored,
        decision,
        applied_at,
        applied_by,
    );

    match result {
        Ok(audit_id) => {
            connection.commit().map_err(|error| DomainError::Storage {
                message: format!("Failed to commit curation apply: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?;
            Ok(audit_id)
        }
        Err(error) => {
            let _ = connection.rollback();
            Err(error)
        }
    }
}

fn persist_candidate_application_inner(
    connection: &DbConnection,
    workspace_id: &str,
    stored: &StoredCurationCandidate,
    decision: &ApplyDecision,
    applied_at: &str,
    applied_by: &str,
) -> Result<String, DomainError> {
    let memory_changed = if decision.tombstone_memory {
        connection
            .tombstone_memory(&stored.target_memory_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to tombstone target memory: {error}"),
                repair: Some("ee memory show <memory-id> --json".to_owned()),
            })?
    } else if let Some(update) = &decision.memory_update {
        connection
            .apply_memory_curation_update(&stored.target_memory_id, update)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to update target memory: {error}"),
                repair: Some("ee memory show <memory-id> --json".to_owned()),
            })?
    } else {
        false
    };
    if !memory_changed {
        return Err(DomainError::Storage {
            message: format!(
                "Curation candidate {} did not mutate target memory {}.",
                stored.id, stored.target_memory_id
            ),
            repair: Some("ee curate candidates --json".to_owned()),
        });
    }

    let marked_applied = connection
        .mark_curation_candidate_applied(workspace_id, &stored.id, applied_at)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to mark curation candidate applied: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    if !marked_applied {
        return Err(DomainError::Storage {
            message: format!(
                "Curation candidate {} was not approved at apply time.",
                stored.id
            ),
            repair: Some(format!("ee curate validate {}", stored.id)),
        });
    }

    let audit_id = generate_audit_id();
    let details = serde_json::json!({
        "candidateId": stored.id.as_str(),
        "candidateType": decision.application.candidate_type.as_str(),
        "fromStatus": stored.status.as_str(),
        "toStatus": decision.to_status.as_str(),
        "decision": decision.application.decision.as_str(),
        "changes": &decision.application.changes,
    })
    .to_string();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some(applied_by.to_owned()),
                action: audit_actions::CURATION_CANDIDATE_APPLY.to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(stored.target_memory_id.clone()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to write curation apply audit entry: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn normalized_review_state(stored: &StoredCurationCandidate) -> String {
    ReviewQueueState::from_str(&stored.review_state)
        .map(|state| state.as_str().to_owned())
        .unwrap_or_else(|_| review_state_for_status_text(&stored.status).to_owned())
}

fn review_state_for_status_text(status: &str) -> &'static str {
    CandidateStatus::from_str(status).map_or("new", |candidate_status| {
        ReviewQueueState::from_candidate_status(candidate_status).as_str()
    })
}

fn candidate_requires_validate(status: &str, review_state: &str) -> bool {
    status == CandidateStatus::Pending.as_str()
        && ReviewQueueState::from_str(review_state)
            .map(|state| state.requires_validation())
            .unwrap_or(true)
}

fn candidate_requires_apply(status: &str, review_state: &str) -> bool {
    status == CandidateStatus::Approved.as_str()
        || ReviewQueueState::from_str(review_state)
            .map(|state| state.requires_apply())
            .unwrap_or(false)
}

fn next_action_for_candidate_fields(
    candidate_id: &str,
    status: &str,
    review_state: &str,
    snoozed_until: Option<&str>,
) -> String {
    match review_state {
        "snoozed" => snoozed_until.map_or_else(
            || format!("ee curate candidates --all --json # {candidate_id} is snoozed"),
            |until| format!("no action until {until}"),
        ),
        "accepted" => format!("ee curate apply {candidate_id} --json"),
        "rejected" | "merged" | "superseded" | "expired" | "applied" => {
            "no action required".to_owned()
        }
        _ if status == CandidateStatus::Approved.as_str() => {
            format!("ee curate apply {candidate_id} --json")
        }
        _ if status == CandidateStatus::Rejected.as_str()
            || status == CandidateStatus::Expired.as_str()
            || status == CandidateStatus::Applied.as_str() =>
        {
            "no action required".to_owned()
        }
        _ => format!("ee curate validate {candidate_id} --json"),
    }
}

fn candidate_hidden_from_default_queue(
    candidate: &StoredCurationCandidate,
    now_rfc3339: &str,
) -> bool {
    if candidate.review_state != ReviewQueueState::Snoozed.as_str() {
        return false;
    }
    candidate.snoozed_until.as_deref().is_none_or(|until| {
        timestamp_has_expired(until, now_rfc3339).map_or(true, |expired| !expired)
    })
}

fn candidate_summary_from_stored(
    stored: StoredCurationCandidate,
    workspace_path: &Path,
) -> CurateCandidateSummary {
    let evidence = stored.source_id.as_ref().map_or_else(Vec::new, |id| {
        vec![CurateCandidateEvidence {
            evidence_type: stored.source_type.clone(),
            id: id.clone(),
        }]
    });
    let review_state = normalized_review_state(&stored);
    let requires_validate = candidate_requires_validate(&stored.status, &review_state);
    let requires_apply = candidate_requires_apply(&stored.status, &review_state);
    let next_action = next_action_for_candidate_fields(
        &stored.id,
        &stored.status,
        &review_state,
        stored.snoozed_until.as_deref(),
    );

    CurateCandidateSummary {
        id: stored.id,
        candidate_type: stored.candidate_type,
        target_memory_id: stored.target_memory_id,
        proposed_content: stored.proposed_content,
        proposed_confidence: stored.proposed_confidence,
        proposed_trust_class: stored.proposed_trust_class,
        confidence: stored.confidence,
        status: stored.status,
        review_state,
        reason: stored.reason,
        source: CurateCandidateSource {
            source_type: stored.source_type,
            source_id: stored.source_id,
        },
        evidence,
        validation: CurateCandidateValidation {
            status: "not_run".to_owned(),
            warnings: Vec::new(),
            next_action: "ee curate validate <CANDIDATE_ID>".to_owned(),
        },
        scope: "workspace".to_owned(),
        scope_key: workspace_path.display().to_string(),
        created_at: stored.created_at,
        reviewed_at: stored.reviewed_at,
        reviewed_by: stored.reviewed_by,
        applied_at: stored.applied_at,
        ttl_expires_at: stored.ttl_expires_at,
        snoozed_until: stored.snoozed_until,
        merged_into_candidate_id: stored.merged_into_candidate_id,
        state_entered_at: stored.state_entered_at,
        last_action_at: stored.last_action_at,
        ttl_policy_id: stored.ttl_policy_id,
        requires_validate,
        requires_apply,
        next_action,
    }
}

fn prepare_curate_read(
    workspace_path: &Path,
    database_path: Option<&Path>,
) -> Result<PreparedCurateRead, DomainError> {
    let workspace_path = resolve_workspace_path(workspace_path)?;
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    Ok(PreparedCurateRead {
        workspace_id: stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
    })
}

fn open_existing_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_owned()),
        });
    }
    DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
        message: format!("Failed to open database: {error}"),
        repair: Some("ee doctor".to_owned()),
    })
}

fn parse_optional_candidate_type(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            CandidateType::from_str(value)
                .map(|candidate_type| candidate_type.as_str().to_owned())
                .map_err(|error| {
                    curate_usage_error(error.to_string(), "ee curate candidates --help")
                })
        })
        .transpose()
}

fn parse_optional_status(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            CandidateStatus::from_str(value)
                .map(|status| status.as_str().to_owned())
                .map_err(|error| {
                    curate_usage_error(error.to_string(), "ee curate candidates --help")
                })
        })
        .transpose()
}

fn parse_optional_memory_id(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            MemoryId::from_str(value)
                .map(|id| id.to_string())
                .map_err(|error| {
                    curate_usage_error(
                        format!("invalid target memory ID: {error}"),
                        "ee curate candidates --help",
                    )
                })
        })
        .transpose()
}

fn parse_merge_target_candidate_id(
    options: &CurateReviewOptions<'_>,
) -> Result<Option<String>, DomainError> {
    match options.action {
        CurateReviewAction::Merge => {
            let raw = options.merge_into_candidate_id.ok_or_else(|| {
                curate_usage_error(
                    "curate merge requires a target candidate ID".to_owned(),
                    "ee curate merge <source-candidate-id> <target-candidate-id> --json",
                )
            })?;
            let target_id = validate_curate_candidate_id(raw)?;
            let source_id = options.candidate_id.trim();
            if target_id == source_id {
                return Err(curate_usage_error(
                    "curate merge target must differ from the source candidate".to_owned(),
                    "ee curate merge <source-candidate-id> <target-candidate-id> --json",
                ));
            }
            Ok(Some(target_id))
        }
        CurateReviewAction::Accept | CurateReviewAction::Reject | CurateReviewAction::Snooze => {
            Ok(None)
        }
    }
}

fn parse_snoozed_until(options: &CurateReviewOptions<'_>) -> Result<Option<String>, DomainError> {
    match options.action {
        CurateReviewAction::Snooze => {
            let raw = options.snoozed_until.ok_or_else(|| {
                curate_usage_error(
                    "curate snooze requires --until".to_owned(),
                    "ee curate snooze <candidate-id> --until <RFC3339> --json",
                )
            })?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(curate_usage_error(
                    "curate snooze --until must not be empty".to_owned(),
                    "ee curate snooze <candidate-id> --until <RFC3339> --json",
                ));
            }
            DateTime::parse_from_rfc3339(trimmed).map_err(|error| {
                curate_usage_error(
                    format!("invalid --until timestamp: {error}"),
                    "ee curate snooze <candidate-id> --until <RFC3339> --json",
                )
            })?;
            Ok(Some(trimmed.to_owned()))
        }
        CurateReviewAction::Accept | CurateReviewAction::Reject | CurateReviewAction::Merge => {
            Ok(None)
        }
    }
}

fn load_merge_target_candidate(
    connection: &DbConnection,
    workspace_id: &str,
    candidate_id: &str,
) -> Result<StoredCurationCandidate, DomainError> {
    connection
        .get_curation_candidate(workspace_id, candidate_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load merge target candidate: {error}"),
            repair: Some("ee curate candidates --all --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "merge target curation candidate".to_owned(),
            id: candidate_id.to_owned(),
            repair: Some("ee curate candidates --all --json".to_owned()),
        })
}

fn validate_curate_candidate_id(raw: &str) -> Result<String, DomainError> {
    let candidate_id = raw.trim();
    let valid = candidate_id.starts_with("curate_")
        && candidate_id.len() == 33
        && candidate_id
            .bytes()
            .skip("curate_".len())
            .all(|byte| byte.is_ascii_alphanumeric());
    if valid {
        Ok(candidate_id.to_owned())
    } else {
        Err(curate_usage_error(
            format!("invalid curation candidate ID: {raw}"),
            "ee curate candidates --json",
        ))
    }
}

fn validate_list_window(limit: u32) -> Result<(), DomainError> {
    if limit == 0 {
        return Err(curate_usage_error(
            "curate candidates --limit must be greater than zero".to_owned(),
            "ee curate candidates --help",
        ));
    }
    if limit > MAX_CANDIDATE_LIST_LIMIT {
        return Err(curate_usage_error(
            format!("curate candidates --limit must be <= {MAX_CANDIDATE_LIST_LIMIT}"),
            "ee curate candidates --help",
        ));
    }
    Ok(())
}

fn resolve_workspace_path(path: &Path) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    absolute
        .canonicalize()
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        })
}

pub(crate) fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes()) {
        *target = *source;
    }
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn curate_usage_error(message: String, repair: &str) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some(repair.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CurateCandidatesOptions, CurateReviewAction, CurateReviewOptions, apply_curation_candidate,
        candidate_summary_from_stored, list_curation_candidates, review_curation_candidate,
        stable_workspace_id, validate_curation_candidate,
    };
    use crate::db::{
        CreateCurationCandidateInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection,
        StoredCurationCandidate, audit_actions,
    };
    use crate::models::{CandidateId, MemoryId};

    type TestResult = Result<(), String>;

    #[test]
    fn candidate_summary_marks_pending_as_validate_before_apply() {
        let stored = StoredCurationCandidate {
            id: "curate_00000000000000000000000000".to_owned(),
            workspace_id: "wsp_00000000000000000000000000".to_owned(),
            candidate_type: "promote".to_owned(),
            target_memory_id: "mem_00000000000000000000000000".to_owned(),
            proposed_content: None,
            proposed_confidence: Some(0.82),
            proposed_trust_class: Some("agent_validated".to_owned()),
            source_type: "feedback_event".to_owned(),
            source_id: Some("outcome_1".to_owned()),
            reason: "Helpful feedback raised confidence.".to_owned(),
            confidence: 0.74,
            status: "pending".to_owned(),
            created_at: "2026-05-01T00:00:00Z".to_owned(),
            reviewed_at: None,
            reviewed_by: None,
            applied_at: None,
            ttl_expires_at: None,
            review_state: "new".to_owned(),
            snoozed_until: None,
            merged_into_candidate_id: None,
            state_entered_at: Some("2026-05-01T00:00:00Z".to_owned()),
            last_action_at: None,
            ttl_policy_id: None,
        };

        let summary = candidate_summary_from_stored(stored, std::path::Path::new("/repo"));
        assert!(summary.requires_validate);
        assert!(!summary.requires_apply);
        assert_eq!(
            summary.next_action,
            "ee curate validate curate_00000000000000000000000000 --json"
        );
        assert_eq!(summary.validation.status, "not_run");
        assert_eq!(summary.evidence.len(), 1);
    }

    #[test]
    fn list_curation_candidates_filters_pending_and_paginates() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string();

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("curate-test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo fmt --check before release.".to_owned(),
                    confidence: 0.7,
                    utility: 0.6,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let pending_id = curate_id(2);
        let approved_id = curate_id(3);
        connection
            .insert_curation_candidate(
                &pending_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id.clone(),
                    proposed_content: None,
                    proposed_confidence: Some(0.8),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "feedback_event".to_owned(),
                    source_id: Some("outcome_helpful".to_owned()),
                    reason: "Useful during release verification.".to_owned(),
                    confidence: 0.76,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:02Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_curation_candidate(
                &approved_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id,
                    proposed_content: None,
                    proposed_confidence: Some(0.85),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "human_request".to_owned(),
                    source_id: Some("reviewer".to_owned()),
                    reason: "Approved separately.".to_owned(),
                    confidence: 0.88,
                    status: Some("approved".to_owned()),
                    created_at: Some("2026-05-01T00:00:03Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = list_curation_candidates(&CurateCandidatesOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_type: Some("promote"),
            status: Some("pending"),
            target_memory_id: None,
            limit: 10,
            offset: 0,
            sort: "review_state",
            group_duplicates: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, super::CURATE_CANDIDATES_SCHEMA_V1);
        assert_eq!(report.total_count, 1);
        assert_eq!(report.returned_count, 1);
        assert_eq!(report.candidates[0].id, pending_id);
        assert!(!report.durable_mutation);
        assert_eq!(report.filter.status.as_deref(), Some("pending"));
        Ok(())
    }

    #[test]
    fn list_curation_candidates_supports_sorting_and_duplicate_grouping() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(21)).to_string();

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("curate-sort-group".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Review queue sort/group fixture.".to_owned(),
                    confidence: 0.7,
                    utility: 0.6,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let dup_older = curate_id(22);
        let dup_newer = curate_id(23);
        let other_group = curate_id(24);
        connection
            .insert_curation_candidate(
                &dup_older,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id.clone(),
                    proposed_content: Some("group-a".to_owned()),
                    proposed_confidence: Some(0.65),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "feedback_event".to_owned(),
                    source_id: Some("outcome_dup_older".to_owned()),
                    reason: "duplicate group older".to_owned(),
                    confidence: 0.65,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:01Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_curation_candidate(
                &dup_newer,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id.clone(),
                    proposed_content: Some("group-a".to_owned()),
                    proposed_confidence: Some(0.90),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "feedback_event".to_owned(),
                    source_id: Some("outcome_dup_newer".to_owned()),
                    reason: "duplicate group newer".to_owned(),
                    confidence: 0.90,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:03Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_curation_candidate(
                &other_group,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "supersede".to_owned(),
                    target_memory_id: memory_id,
                    proposed_content: Some("group-b".to_owned()),
                    proposed_confidence: Some(0.80),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "human_request".to_owned(),
                    source_id: Some("reviewer".to_owned()),
                    reason: "separate group".to_owned(),
                    confidence: 0.80,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:02Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = list_curation_candidates(&CurateCandidatesOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_type: None,
            status: Some("pending"),
            target_memory_id: None,
            limit: 10,
            offset: 0,
            sort: "created_at",
            group_duplicates: true,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.filter.sort, "created_at");
        assert!(report.filter.group_duplicates);
        assert_eq!(report.candidates.len(), 3);
        assert_eq!(report.candidates[0].id, dup_newer);
        assert_eq!(report.candidates[1].id, dup_older);
        assert_eq!(report.candidates[2].id, other_group);
        Ok(())
    }

    #[test]
    fn validate_curation_candidate_approves_pending_and_writes_audit() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(11)).to_string();
        let candidate_id = curate_id(12);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;

        let report = validate_curation_candidate(&super::CurateValidateOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &candidate_id,
            actor: Some("MistySalmon"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, super::CURATE_VALIDATE_SCHEMA_V1);
        assert_eq!(report.validation.status, "passed");
        assert_eq!(report.validation.decision, "approved");
        assert_eq!(report.mutation.from_status, "pending");
        assert_eq!(report.mutation.to_status, "approved");
        assert!(report.mutation.persisted);
        assert!(report.durable_mutation);
        let stored = connection
            .get_curation_candidate(&workspace_id, &candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate missing after validation".to_owned())?;
        assert_eq!(stored.status, "approved");
        assert_eq!(stored.reviewed_by.as_deref(), Some("MistySalmon"));
        let audit_id = report
            .mutation
            .audit_id
            .as_ref()
            .ok_or_else(|| "validation should write an audit id".to_owned())?;
        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "audit entry missing".to_owned())?;
        assert_eq!(audit.action, audit_actions::CURATION_CANDIDATE_VALIDATE);
        assert_eq!(audit.target_id.as_deref(), Some(candidate_id.as_str()));
        Ok(())
    }

    #[test]
    fn validate_curation_candidate_dry_run_rejects_without_mutation() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(13)).to_string();
        let candidate_id = curate_id(14);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "consolidate",
            Some("pending"),
            None,
        )?;

        let report = validate_curation_candidate(&super::CurateValidateOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &candidate_id,
            actor: Some("MistySalmon"),
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.validation.status, "failed");
        assert_eq!(report.validation.decision, "rejected");
        assert_eq!(report.mutation.to_status, "rejected");
        assert!(!report.mutation.persisted);
        assert!(report.dry_run);
        assert!(
            report
                .validation
                .errors
                .iter()
                .any(|issue| issue.code == "content_required_for_type")
        );
        let stored = connection
            .get_curation_candidate(&workspace_id, &candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate missing after dry run".to_owned())?;
        assert_eq!(stored.status, "pending");
        assert!(stored.reviewed_at.is_none());
        Ok(())
    }

    #[test]
    fn validate_curation_candidate_rejects_low_evidence_without_applying() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(27)).to_string();
        let candidate_id = curate_id(28);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;
        let missing_evidence_id = curate_id(29);
        connection
            .insert_curation_candidate(
                &missing_evidence_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id.clone(),
                    proposed_content: None,
                    proposed_confidence: Some(0.91),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "agent_inference".to_owned(),
                    source_id: None,
                    reason: "Candidate lacks explicit source evidence.".to_owned(),
                    confidence: 0.90,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:05Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = validate_curation_candidate(&super::CurateValidateOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &missing_evidence_id,
            actor: Some("MistySalmon"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.validation.status, "failed");
        assert_eq!(report.validation.decision, "rejected");
        assert!(
            report
                .validation
                .errors
                .iter()
                .any(|issue| issue.code == "candidate_missing_source_evidence")
        );
        assert_eq!(report.mutation.to_status, "rejected");
        assert!(report.mutation.persisted);
        assert!(report.mutation.audit_id.is_some());

        let stored = connection
            .get_curation_candidate(&workspace_id, &missing_evidence_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate missing after low-evidence validation".to_owned())?;
        assert_eq!(stored.status, "rejected");
        assert!(stored.applied_at.is_none());

        let memory = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after low-evidence validation".to_owned())?;
        assert!((memory.confidence - 0.7).abs() < 0.001);
        assert_eq!(memory.trust_class, "human_explicit");
        Ok(())
    }

    #[test]
    fn apply_curation_candidate_updates_memory_and_writes_audit() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(15)).to_string();
        let candidate_id = curate_id(16);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("approved"),
            None,
        )?;

        let report = apply_curation_candidate(&super::CurateApplyOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &candidate_id,
            actor: Some("MistySalmon"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, super::CURATE_APPLY_SCHEMA_V1);
        assert_eq!(report.application.status, "applied");
        assert_eq!(report.application.decision, "update_memory");
        assert_eq!(report.mutation.from_status, "approved");
        assert_eq!(report.mutation.to_status, "applied");
        assert!(report.mutation.persisted);
        assert!(report.durable_mutation);
        assert!(
            report
                .application
                .changes
                .iter()
                .any(|change| change.field == "confidence")
        );
        assert!(
            report
                .application
                .changes
                .iter()
                .any(|change| change.field == "trustClass")
        );

        let memory = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after apply".to_owned())?;
        assert!((memory.confidence - 0.82).abs() < 0.001);
        assert_eq!(memory.trust_class, "agent_validated");

        let stored = connection
            .get_curation_candidate(&workspace_id, &candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate missing after apply".to_owned())?;
        assert_eq!(stored.status, "applied");
        assert!(stored.applied_at.is_some());

        let audit_id = report
            .mutation
            .audit_id
            .as_ref()
            .ok_or_else(|| "apply should write an audit id".to_owned())?;
        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "audit entry missing".to_owned())?;
        assert_eq!(audit.action, audit_actions::CURATION_CANDIDATE_APPLY);
        assert_eq!(audit.target_id.as_deref(), Some(memory_id.as_str()));
        Ok(())
    }

    #[test]
    fn apply_curation_candidate_dry_run_leaves_memory_and_candidate_unchanged() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(17)).to_string();
        let candidate_id = curate_id(18);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("approved"),
            None,
        )?;

        let report = apply_curation_candidate(&super::CurateApplyOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &candidate_id,
            actor: Some("MistySalmon"),
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.application.status, "would_apply");
        assert_eq!(report.mutation.to_status, "applied");
        assert!(!report.mutation.persisted);
        assert!(report.dry_run);

        let memory = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after dry run".to_owned())?;
        assert!((memory.confidence - 0.7).abs() < 0.001);
        assert_eq!(memory.trust_class, "human_explicit");

        let stored = connection
            .get_curation_candidate(&workspace_id, &candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate missing after dry run".to_owned())?;
        assert_eq!(stored.status, "approved");
        assert!(stored.applied_at.is_none());
        Ok(())
    }

    #[test]
    fn review_curation_candidate_accepts_and_rejects_with_audit() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(19)).to_string();
        let accept_id = curate_id(20);
        let reject_id = curate_id(21);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &accept_id,
            "promote",
            Some("pending"),
            None,
        )?;
        connection
            .insert_curation_candidate(
                &reject_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id.clone(),
                    proposed_content: None,
                    proposed_confidence: Some(0.72),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "human_request".to_owned(),
                    source_id: Some("reviewer".to_owned()),
                    reason: "Reject duplicate candidate.".to_owned(),
                    confidence: 0.60,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:03Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let accept = review_curation_candidate(&CurateReviewOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &accept_id,
            action: CurateReviewAction::Accept,
            actor: Some("MistySalmon"),
            dry_run: false,
            snoozed_until: None,
            merge_into_candidate_id: None,
        })
        .map_err(|error| error.message())?;
        assert_eq!(accept.schema, super::CURATE_REVIEW_SCHEMA_V1);
        assert_eq!(accept.review.action, "accept");
        assert_eq!(accept.mutation.to_status, "approved");
        assert_eq!(accept.mutation.to_review_state, "accepted");
        assert!(accept.mutation.persisted);
        let accept_audit = accept
            .mutation
            .audit_id
            .as_ref()
            .ok_or_else(|| "accept should write an audit id".to_owned())?;
        let audit = connection
            .get_audit(accept_audit)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "accept audit entry missing".to_owned())?;
        assert_eq!(audit.action, audit_actions::CURATION_CANDIDATE_ACCEPT);

        let reject = review_curation_candidate(&CurateReviewOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &reject_id,
            action: CurateReviewAction::Reject,
            actor: Some("MistySalmon"),
            dry_run: false,
            snoozed_until: None,
            merge_into_candidate_id: None,
        })
        .map_err(|error| error.message())?;
        assert_eq!(reject.review.action, "reject");
        assert_eq!(reject.mutation.to_status, "rejected");
        assert_eq!(reject.mutation.to_review_state, "rejected");
        assert!(reject.durable_mutation);
        let stored = connection
            .get_curation_candidate(&workspace_id, &reject_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "rejected candidate missing".to_owned())?;
        assert_eq!(stored.status, "rejected");
        assert_eq!(stored.review_state, "rejected");
        let reject_audit = reject
            .mutation
            .audit_id
            .as_ref()
            .ok_or_else(|| "reject should write an audit id".to_owned())?;
        let audit_connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let audit = audit_connection
            .get_audit(reject_audit)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "reject audit entry missing".to_owned())?;
        assert_eq!(audit.action, audit_actions::CURATION_CANDIDATE_REJECT);
        Ok(())
    }

    #[test]
    fn review_curation_candidate_snoozes_and_merges_with_explicit_targets() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(22)).to_string();
        let source_id = curate_id(23);
        let target_id = curate_id(24);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &source_id,
            "promote",
            Some("pending"),
            None,
        )?;
        connection
            .insert_curation_candidate(
                &target_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id,
                    proposed_content: None,
                    proposed_confidence: Some(0.86),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "human_request".to_owned(),
                    source_id: Some("reviewer".to_owned()),
                    reason: "Primary candidate absorbs duplicate review work.".to_owned(),
                    confidence: 0.80,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:04Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let snooze = review_curation_candidate(&CurateReviewOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &source_id,
            action: CurateReviewAction::Snooze,
            actor: Some("MistySalmon"),
            dry_run: false,
            snoozed_until: Some("2030-01-01T00:00:00Z"),
            merge_into_candidate_id: None,
        })
        .map_err(|error| error.message())?;
        assert_eq!(snooze.mutation.to_status, "pending");
        assert_eq!(snooze.mutation.to_review_state, "snoozed");
        assert_eq!(
            snooze.mutation.snoozed_until.as_deref(),
            Some("2030-01-01T00:00:00Z")
        );
        let stored = connection
            .get_curation_candidate(&workspace_id, &source_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "snoozed candidate missing".to_owned())?;
        assert_eq!(stored.review_state, "snoozed");
        assert_eq!(
            stored.snoozed_until.as_deref(),
            Some("2030-01-01T00:00:00Z")
        );

        let merge = review_curation_candidate(&CurateReviewOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &source_id,
            action: CurateReviewAction::Merge,
            actor: Some("MistySalmon"),
            dry_run: false,
            snoozed_until: None,
            merge_into_candidate_id: Some(&target_id),
        })
        .map_err(|error| error.message())?;
        assert_eq!(merge.mutation.to_status, "rejected");
        assert_eq!(merge.mutation.to_review_state, "merged");
        assert_eq!(
            merge.mutation.merged_into_candidate_id.as_deref(),
            Some(target_id.as_str())
        );
        let stored = connection
            .get_curation_candidate(&workspace_id, &source_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "merged candidate missing".to_owned())?;
        assert_eq!(stored.status, "rejected");
        assert_eq!(stored.review_state, "merged");
        assert!(stored.snoozed_until.is_none());
        assert_eq!(
            stored.merged_into_candidate_id.as_deref(),
            Some(target_id.as_str())
        );
        let merge_audit = merge
            .mutation
            .audit_id
            .as_ref()
            .ok_or_else(|| "merge should write an audit id".to_owned())?;
        let audit = connection
            .get_audit(merge_audit)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "merge audit entry missing".to_owned())?;
        assert_eq!(audit.action, audit_actions::CURATION_CANDIDATE_MERGE);
        Ok(())
    }

    #[test]
    fn review_curation_candidate_dry_run_leaves_candidate_unchanged() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(25)).to_string();
        let candidate_id = curate_id(26);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;

        let report = review_curation_candidate(&CurateReviewOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &candidate_id,
            action: CurateReviewAction::Accept,
            actor: Some("MistySalmon"),
            dry_run: true,
            snoozed_until: None,
            merge_into_candidate_id: None,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.mutation.to_status, "approved");
        assert_eq!(report.mutation.to_review_state, "accepted");
        assert!(!report.mutation.persisted);
        assert!(report.dry_run);
        assert!(report.mutation.audit_id.is_none());
        let stored = connection
            .get_curation_candidate(&workspace_id, &candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate missing after dry run".to_owned())?;
        assert_eq!(stored.status, "pending");
        assert_eq!(stored.review_state, "new");
        assert!(stored.reviewed_at.is_none());
        Ok(())
    }

    fn seed_candidate_database(
        database_path: &std::path::Path,
        workspace_id: &str,
        memory_id: &str,
        candidate_id: &str,
        candidate_type: &str,
        status: Option<&str>,
        proposed_content: Option<&str>,
    ) -> Result<DbConnection, String> {
        let connection =
            DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: database_path
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .display()
                        .to_string(),
                    name: Some("curate-validate-test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo fmt --check before release.".to_owned(),
                    confidence: 0.7,
                    utility: 0.6,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_curation_candidate(
                candidate_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.to_owned(),
                    candidate_type: candidate_type.to_owned(),
                    target_memory_id: memory_id.to_owned(),
                    proposed_content: proposed_content.map(str::to_owned),
                    proposed_confidence: Some(0.82),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "feedback_event".to_owned(),
                    source_id: Some("outcome_helpful".to_owned()),
                    reason: "Useful during release verification.".to_owned(),
                    confidence: 0.76,
                    status: status.map(str::to_owned),
                    created_at: Some("2026-05-01T00:00:02Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    fn curate_id(seed: u128) -> String {
        let candidate = CandidateId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        format!("curate_{}", candidate.trim_start_matches("cand_"))
    }
}
