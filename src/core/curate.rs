//! Curation queue read services.
//!
//! `ee curate candidates` exposes the auditable proposal queue without
//! validating or applying candidates. Validation and durable mutation are
//! separate explicit commands.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use fnx_classes::Graph;
use fnx_runtime::CompatibilityMode;
use serde::Serialize;

use crate::config::{ConfigFile, GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY};
use crate::curate::{
    CandidateInput, CandidateSource, CandidateStatus, CandidateType, CandidateValidationError,
    ReviewQueueState, validate_candidate, validate_candidate_trust_evidence,
    validate_review_queue_transition,
};
use crate::db::{
    ApplyMemoryCurationInput, ApplyMemoryLevelTransitionInput, CreateAuditInput,
    CreateCurationCandidateInput, CreateProceduralRuleInput, CreateProcedureEventInput,
    CreateProcedureInput, CreateSearchIndexJobInput, CurationCandidateReviewUpdate, DbConnection,
    MemoryLevelTransitionAuditInput, SearchIndexJobType, StoredCurationCandidate,
    StoredCurationTtlPolicy, StoredEvidenceSpan, StoredMemory, StoredMemoryLink, StoredSession,
    audit_actions, default_curation_ttl_policy_id_for_review_state, generate_audit_id,
};
use crate::graph::decay::{
    StructuralDecayMultiplier, compute_structural_decay_adjustment,
    compute_structural_decay_connectivity,
};
use crate::models::degradation::GRAPH_CURATE_DISCONNECTED_GRAPH_CODE;
use crate::models::{
    CandidateId, DomainError, MemoryId, ProducerMetadata, REVIEW_SESSION_SCHEMA_V1, RuleId,
    WorkspaceId,
};
use crate::search::HashEmbedder;

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
/// Stable schema for curate retire reports.
pub const CURATE_RETIRE_SCHEMA_V1: &str = "ee.curate.retire.v1";
/// Stable schema for curate tombstone reports.
pub const CURATE_TOMBSTONE_SCHEMA_V1: &str = "ee.curate.tombstone.v1";
/// Stable schema for curate untombstone reports.
pub const CURATE_UNTOMBSTONE_SCHEMA_V1: &str = "ee.curate.untombstone.v1";
/// Stable schema for review workspace reports.
pub const REVIEW_WORKSPACE_SCHEMA_V1: &str = "ee.review.workspace.v1";
const MAX_CANDIDATE_LIST_LIMIT: u32 = 1000;
const MAX_REVIEW_SESSION_LIMIT: u32 = 100;
const DEFAULT_SNOOZE_SECONDS: u64 = 90 * 24 * 60 * 60;
const REVIEW_SESSION_CREATED_AT: &str = "1970-01-01T00:00:00Z";

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
    /// Whether graph structure can accelerate or protect age-based TTL disposition.
    pub structural_decay: bool,
    /// Optional frozen clock for tests and deterministic replay.
    pub now_rfc3339: Option<&'a str>,
}

/// Options for reviewing a CASS session and proposing curation candidates.
#[derive(Clone, Debug)]
pub struct ReviewSessionOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Internal ee session ID or upstream CASS session ID. Defaults to the last stable session.
    pub session_id: Option<&'a str>,
    /// Persist proposals into the curation queue.
    pub propose: bool,
    /// Preview without inserting curation candidates.
    pub dry_run: bool,
    /// Minimum confidence threshold for proposals.
    pub min_confidence: f32,
    /// Maximum candidates to return.
    pub limit: u32,
}

/// Options for retiring a curation candidate from the active review set.
#[derive(Clone, Debug)]
pub struct CurateRetireOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Curation candidate ID to retire.
    pub candidate_id: &'a str,
    /// Actor recorded in audit metadata.
    pub actor: Option<&'a str>,
    /// Preview without writing audit record.
    pub dry_run: bool,
    /// Retirement reason for audit trail.
    pub reason: Option<&'a str>,
}

/// Options for tombstoning a memory through the curation surface.
#[derive(Clone, Debug)]
pub struct CurateTombstoneOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Memory ID to tombstone.
    pub memory_id: &'a str,
    /// Actor recorded in audit metadata.
    pub actor: Option<&'a str>,
    /// Preview without writing tombstone record.
    pub dry_run: bool,
    /// Tombstone reason for audit trail.
    pub reason: Option<&'a str>,
}

/// Options for restoring a tombstoned memory through the curation surface.
#[derive(Clone, Debug)]
pub struct CurateUntombstoneOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Tombstoned memory ID to restore.
    pub memory_id: &'a str,
    /// Actor recorded in audit metadata.
    pub actor: Option<&'a str>,
    /// Preview without writing restore record.
    pub dry_run: bool,
    /// Restore reason for audit trail.
    pub reason: Option<&'a str>,
}

/// Options for reviewing workspace evidence and proposing curation candidates.
#[derive(Clone, Debug)]
pub struct ReviewWorkspaceOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Scope path for filtering evidence. Defaults to workspace root.
    pub scope: Option<&'a Path>,
    /// Include persisted CASS-derived evidence rows.
    pub include_cass: bool,
    /// Persist proposals into the curation queue.
    pub propose: bool,
    /// Preview without inserting curation candidates.
    pub dry_run: bool,
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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub structural_adjustments: Vec<CurateStructuralDecayAdjustment>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

/// Result of reviewing one CASS session for curation candidates.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub session_id: String,
    pub cass_session_id: String,
    pub propose_mode: bool,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub evidence_span_count: usize,
    pub topic_count: usize,
    pub candidate_count: usize,
    pub candidates: Vec<ReviewSessionCandidate>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

/// One proposed curation candidate distilled from session evidence.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionCandidate {
    pub candidate_id: String,
    pub candidate_type: String,
    pub candidate_kind: String,
    pub topic_key: String,
    pub target_memory_id: String,
    pub proposed_content: String,
    pub proposed_confidence: f32,
    pub source_type: String,
    pub source_ids: Vec<String>,
    pub reason: String,
    pub confidence: f32,
    pub content_hash: String,
    pub persisted: bool,
}

/// Result of retiring a curation candidate.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateRetireReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub candidate_id: String,
    pub from_status: String,
    pub to_status: String,
    pub reason: Option<String>,
    pub retired_at: String,
    pub retired_by: Option<String>,
    pub dry_run: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

impl CurateRetireReport {
    #[must_use]
    pub fn json_output(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"curate retire","error":"serialization_failed"}}"#,
                CURATE_RETIRE_SCHEMA_V1
            )
        })
    }

    #[must_use]
    pub fn human_output(&self) -> String {
        let mode = if self.dry_run { "DRY RUN" } else { "RETIRED" };
        let mut output = format!("{mode}: {}\n\n", self.candidate_id);
        output.push_str(&format!(
            "  transition: {} -> {}\n",
            self.from_status, self.to_status
        ));
        if let Some(reason) = &self.reason {
            output.push_str(&format!("  reason: {reason}\n"));
        }
        output.push_str(&format!("  retired_at: {}\n", self.retired_at));
        output.push_str(&format!("  persisted: {}\n", self.persisted));
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "CURATE_RETIRE|id={}|from={}|to={}|dry_run={}|persisted={}",
            self.candidate_id, self.from_status, self.to_status, self.dry_run, self.persisted
        )
    }
}

/// Result of tombstoning a memory through curation.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateTombstoneReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub memory_id: String,
    pub reason: Option<String>,
    pub tombstoned_at: String,
    pub tombstoned_by: Option<String>,
    pub dry_run: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

impl CurateTombstoneReport {
    #[must_use]
    pub fn json_output(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"curate tombstone","error":"serialization_failed"}}"#,
                CURATE_TOMBSTONE_SCHEMA_V1
            )
        })
    }

    #[must_use]
    pub fn human_output(&self) -> String {
        let mode = if self.dry_run {
            "DRY RUN"
        } else {
            "TOMBSTONED"
        };
        let mut output = format!("{mode}: {}\n\n", self.memory_id);
        if let Some(reason) = &self.reason {
            output.push_str(&format!("  reason: {reason}\n"));
        }
        output.push_str(&format!("  tombstoned_at: {}\n", self.tombstoned_at));
        output.push_str(&format!("  persisted: {}\n", self.persisted));
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "CURATE_TOMBSTONE|id={}|dry_run={}|persisted={}",
            self.memory_id, self.dry_run, self.persisted
        )
    }
}

/// Result of restoring a tombstoned memory through curation.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateUntombstoneReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub memory_id: String,
    pub reason: Option<String>,
    pub previous_tombstoned_at: Option<String>,
    pub restored_at: String,
    pub restored_by: Option<String>,
    pub dry_run: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

impl CurateUntombstoneReport {
    #[must_use]
    pub fn json_output(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"curate untombstone","error":"serialization_failed"}}"#,
                CURATE_UNTOMBSTONE_SCHEMA_V1
            )
        })
    }

    #[must_use]
    pub fn human_output(&self) -> String {
        let mode = if self.dry_run { "DRY RUN" } else { "RESTORED" };
        let mut output = format!("{mode}: {}\n\n", self.memory_id);
        if let Some(reason) = &self.reason {
            output.push_str(&format!("  reason: {reason}\n"));
        }
        if let Some(previous) = &self.previous_tombstoned_at {
            output.push_str(&format!("  previous_tombstoned_at: {previous}\n"));
        }
        output.push_str(&format!("  restored_at: {}\n", self.restored_at));
        output.push_str(&format!("  persisted: {}\n", self.persisted));
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "CURATE_UNTOMBSTONE|id={}|dry_run={}|persisted={}",
            self.memory_id, self.dry_run, self.persisted
        )
    }
}

/// Result of reviewing workspace evidence for curation candidates.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewWorkspaceReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub scope_path: String,
    pub include_cass: bool,
    pub propose_mode: bool,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub memory_count: usize,
    pub evidence_count: usize,
    pub candidate_count: usize,
    pub candidates: Vec<ReviewSessionCandidate>,
    pub degraded: Vec<CurateCandidatesDegradation>,
    pub next_action: String,
}

impl ReviewWorkspaceReport {
    #[must_use]
    pub fn json_output(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"review workspace","error":"serialization_failed"}}"#,
                REVIEW_WORKSPACE_SCHEMA_V1
            )
        })
    }

    #[must_use]
    pub fn human_output(&self) -> String {
        let mode = if self.dry_run {
            "DRY RUN"
        } else if self.propose_mode {
            "PROPOSED"
        } else {
            "REVIEWED"
        };
        let mut output = format!("{mode}: workspace evidence review\n\n");
        output.push_str(&format!("  scope: {}\n", self.scope_path));
        output.push_str(&format!("  memories: {}\n", self.memory_count));
        output.push_str(&format!("  evidence: {}\n", self.evidence_count));
        output.push_str(&format!("  candidates: {}\n", self.candidate_count));
        output.push_str(&format!("  persisted: {}\n", self.durable_mutation));
        if !self.candidates.is_empty() {
            output.push_str("\nCandidates:\n");
            for candidate in &self.candidates {
                output.push_str(&format!(
                    "  - {} ({}) -> {}\n",
                    candidate.candidate_id, candidate.candidate_type, candidate.target_memory_id
                ));
            }
        }
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "REVIEW_WORKSPACE|scope={}|memories={}|evidence={}|candidates={}|dry_run={}|persisted={}",
            self.scope_path,
            self.memory_count,
            self.evidence_count,
            self.candidate_count,
            self.dry_run,
            self.durable_mutation
        )
    }
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
    pub level: String,
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
pub struct CurateStructuralDecayAdjustment {
    pub candidate_id: String,
    pub memory_id: String,
    pub onion_layer: Option<usize>,
    pub max_layer: usize,
    pub is_articulation_point: bool,
    pub base_decay: f32,
    pub structural_multiplier: f32,
    pub adjusted_decay: f32,
    pub adjusted_ttl_threshold_seconds: u64,
    pub rationale: String,
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
    #[serde(rename = "candidateId")]
    pub candidate_id: String,
    pub id: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub candidate_type: String,
    pub target_memory_id: String,
    pub proposed_content: Option<String>,
    pub proposed_level: Option<String>,
    pub proposed_kind: Option<String>,
    pub proposed_tags: Vec<String>,
    pub proposed_confidence: Option<f32>,
    pub proposed_trust_class: Option<String>,
    pub trust_class: Option<String>,
    pub confidence: f32,
    pub status: String,
    pub review_state: String,
    pub reason: String,
    pub source: CurateCandidateSource,
    pub proposal_source: String,
    pub producer: ProducerMetadata,
    pub evidence: Vec<CurateCandidateEvidence>,
    pub evidence_summary: CurateCandidateEvidenceSummary,
    pub member_memory_ids: Vec<String>,
    pub tombstoned_member_count: usize,
    pub priority: String,
    pub close_reason: Option<String>,
    pub auto_rejected_reason: Option<String>,
    pub audit: CurateCandidateAudit,
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
pub struct CurateCandidateEvidenceSummary {
    pub member_memory_ids: Vec<String>,
    pub support_count: usize,
    pub contradiction_count: usize,
    pub cluster_coherence: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurateCandidateAudit {
    pub proposed_by: String,
    pub proposed_at: String,
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
        .map(|candidate| {
            candidate_summary_from_database(&connection, candidate, &prepared.workspace_path)
        })
        .collect::<Result<Vec<_>, _>>()?;
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

/// Review imported CASS evidence for a session and optionally persist proposals.
pub fn review_session_proposals(
    options: &ReviewSessionOptions<'_>,
) -> Result<ReviewSessionReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    validate_review_session_options(options)?;

    let connection = open_existing_database(&prepared.database_path)?;
    let session = resolve_review_session(
        &connection,
        &prepared.workspace_id,
        options
            .session_id
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    )?;
    let evidence_spans = connection
        .list_evidence_spans_for_session(&session.id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list session evidence spans: {error}"),
            repair: Some("ee import cass --workspace . --json".to_owned()),
        })?;

    let mut candidates = build_review_session_candidates(
        &prepared.workspace_id,
        &session,
        &evidence_spans,
        options.min_confidence,
        options.limit,
    );

    let mut durable_mutation = false;
    if options.propose && !options.dry_run {
        for candidate in &mut candidates {
            if connection
                .get_curation_candidate(&prepared.workspace_id, &candidate.candidate_id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to check existing curation candidate: {error}"),
                    repair: Some("ee curate candidates --json".to_owned()),
                })?
                .is_some()
            {
                continue;
            }
            connection
                .insert_curation_candidate(
                    &candidate.candidate_id,
                    &CreateCurationCandidateInput {
                        workspace_id: prepared.workspace_id.clone(),
                        candidate_type: candidate.candidate_type.clone(),
                        target_memory_id: candidate.target_memory_id.clone(),
                        proposed_content: Some(candidate.proposed_content.clone()),
                        proposed_confidence: Some(candidate.proposed_confidence),
                        proposed_trust_class: None,
                        source_type: candidate.source_type.clone(),
                        source_id: Some(candidate.source_ids.join(",")),
                        reason: candidate.reason.clone(),
                        confidence: candidate.confidence,
                        status: Some(CandidateStatus::Pending.as_str().to_owned()),
                        created_at: Some(REVIEW_SESSION_CREATED_AT.to_owned()),
                        ttl_expires_at: None,
                    },
                )
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to insert session review curation candidate: {error}"),
                    repair: Some("ee curate candidates --json".to_owned()),
                })?;
            candidate.persisted = true;
            durable_mutation = true;
        }
    }

    let topic_count = candidates
        .iter()
        .map(|candidate| candidate.topic_key.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let candidate_count = candidates.len();
    let next_action = if candidate_count == 0 {
        "no session-review candidates proposed".to_owned()
    } else if options.propose && !options.dry_run {
        "ee curate candidates --status pending --json".to_owned()
    } else {
        "ee review session <session-id> --propose --json".to_owned()
    };

    Ok(ReviewSessionReport {
        schema: REVIEW_SESSION_SCHEMA_V1,
        command: "review session",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        session_id: session.id,
        cass_session_id: session.cass_session_id,
        propose_mode: options.propose,
        dry_run: options.dry_run,
        durable_mutation,
        evidence_span_count: evidence_spans.len(),
        topic_count,
        candidate_count,
        candidates,
        degraded: Vec::new(),
        next_action,
    })
}

fn validate_review_session_options(options: &ReviewSessionOptions<'_>) -> Result<(), DomainError> {
    if !(0.0..=1.0).contains(&options.min_confidence) {
        return Err(curate_usage_error(
            format!(
                "review session --min-confidence must be between 0.0 and 1.0, got {}",
                options.min_confidence
            ),
            "ee review session --help",
        ));
    }
    if options.limit == 0 {
        return Err(curate_usage_error(
            "review session --limit must be greater than zero".to_owned(),
            "ee review session --help",
        ));
    }
    if options.limit > MAX_REVIEW_SESSION_LIMIT {
        return Err(curate_usage_error(
            format!("review session --limit must be <= {MAX_REVIEW_SESSION_LIMIT}"),
            "ee review session --help",
        ));
    }
    Ok(())
}

fn resolve_review_session(
    connection: &DbConnection,
    workspace_id: &str,
    requested: Option<&str>,
) -> Result<StoredSession, DomainError> {
    if let Some(session_id) = requested {
        if let Some(session) = connection
            .get_session(session_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to load session: {error}"),
                repair: Some("ee import cass --workspace . --json".to_owned()),
            })?
            .filter(|session| session.workspace_id == workspace_id)
        {
            return Ok(session);
        }
        return connection
            .get_session_by_cass_id(workspace_id, session_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to load CASS session: {error}"),
                repair: Some("ee import cass --workspace . --json".to_owned()),
            })?
            .ok_or_else(|| DomainError::NotFound {
                resource: "CASS session".to_owned(),
                id: session_id.to_owned(),
                repair: Some("ee import cass --workspace . --json".to_owned()),
            });
    }

    connection
        .list_sessions(workspace_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list sessions: {error}"),
            repair: Some("ee import cass --workspace . --json".to_owned()),
        })?
        .into_iter()
        .max_by(|left, right| {
            review_session_recency_key(left)
                .cmp(review_session_recency_key(right))
                .then_with(|| left.cass_session_id.cmp(&right.cass_session_id))
                .then_with(|| left.id.cmp(&right.id))
        })
        .ok_or_else(|| DomainError::NotFound {
            resource: "CASS session".to_owned(),
            id: "latest".to_owned(),
            repair: Some("ee import cass --workspace . --json".to_owned()),
        })
}

fn review_session_recency_key(session: &StoredSession) -> &str {
    session
        .ended_at
        .as_deref()
        .or(session.started_at.as_deref())
        .unwrap_or(session.imported_at.as_str())
}

fn build_review_session_candidates(
    workspace_id: &str,
    session: &StoredSession,
    evidence_spans: &[StoredEvidenceSpan],
    min_confidence: f32,
    limit: u32,
) -> Vec<ReviewSessionCandidate> {
    let mut grouped: BTreeMap<String, Vec<&StoredEvidenceSpan>> = BTreeMap::new();
    for span in evidence_spans {
        if span.memory_id.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        let topic_key = review_topic_key(&span.excerpt);
        if topic_key == "noise" {
            continue;
        }
        grouped.entry(topic_key).or_default().push(span);
    }

    let mut candidates = grouped
        .into_iter()
        .filter_map(|(topic_key, mut spans)| {
            spans.sort_by(|left, right| {
                left.start_line
                    .cmp(&right.start_line)
                    .then_with(|| left.end_line.cmp(&right.end_line))
                    .then_with(|| left.id.cmp(&right.id))
            });
            build_review_candidate(workspace_id, session, &topic_key, &spans)
        })
        .filter(|candidate| candidate.confidence >= min_confidence)
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| left.topic_key.cmp(&right.topic_key))
            .then_with(|| left.candidate_id.cmp(&right.candidate_id))
    });
    candidates.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    candidates
}

fn build_review_candidate(
    workspace_id: &str,
    session: &StoredSession,
    topic_key: &str,
    spans: &[&StoredEvidenceSpan],
) -> Option<ReviewSessionCandidate> {
    let evidence_ids = spans
        .iter()
        .map(|span| span.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if evidence_ids.len() < 2 {
        return None;
    }
    let target_memory_id = spans
        .iter()
        .filter_map(|span| span.memory_id.as_deref())
        .filter(|memory_id| !memory_id.trim().is_empty())
        .min()?
        .to_owned();
    let candidate_kind = review_candidate_kind(spans);
    let proposed_content = review_candidate_content(topic_key, &candidate_kind, spans);
    let confidence = review_candidate_confidence(spans.len());
    let content_hash = format!(
        "blake3:{}",
        blake3::hash(proposed_content.as_bytes()).to_hex()
    );
    let candidate_id = deterministic_curate_id(&[
        workspace_id,
        session.id.as_str(),
        session.cass_session_id.as_str(),
        topic_key,
        evidence_ids.join(",").as_str(),
        content_hash.as_str(),
    ]);
    let reason = format!(
        "Session review clustered {} evidence span(s) for topic `{topic_key}` from CASS session `{}`.",
        evidence_ids.len(),
        session.cass_session_id
    );

    Some(ReviewSessionCandidate {
        candidate_id,
        candidate_type: CandidateType::Rule.as_str().to_owned(),
        candidate_kind,
        topic_key: topic_key.to_owned(),
        target_memory_id,
        proposed_content,
        proposed_confidence: confidence,
        source_type: CandidateSource::AgentInference.as_str().to_owned(),
        source_ids: evidence_ids,
        reason,
        confidence,
        content_hash,
        persisted: false,
    })
}

fn review_topic_key(excerpt: &str) -> String {
    let tokens = normalized_review_tokens(excerpt);
    topic_from_keywords(&tokens).unwrap_or_else(|| {
        tokens
            .iter()
            .find(|token| token.len() >= 5)
            .cloned()
            .unwrap_or_else(|| "noise".to_owned())
    })
}

fn topic_from_keywords(tokens: &BTreeSet<String>) -> Option<String> {
    const TOPICS: &[(&str, &[&str])] = &[
        ("formatting", &["fmt", "format", "formatting", "rustfmt"]),
        (
            "linting",
            &["clippy", "lint", "lints", "warning", "warnings"],
        ),
        (
            "testing",
            &["e2e", "fixture", "fixtures", "golden", "test", "tests"],
        ),
        (
            "storage",
            &[
                "database",
                "db",
                "frankensqlite",
                "migration",
                "sqlite",
                "sqlmodel",
                "storage",
            ],
        ),
        (
            "retrieval",
            &[
                "bm25",
                "embedding",
                "frankensearch",
                "retrieval",
                "search",
                "semantic",
            ],
        ),
        (
            "runtime",
            &[
                "asupersync",
                "budget",
                "cancellation",
                "labruntime",
                "runtime",
            ],
        ),
        (
            "process",
            &[
                "agent",
                "beads",
                "br",
                "bv",
                "mail",
                "reservation",
                "worktree",
            ],
        ),
        ("cass", &["cass", "session", "span", "transcript"]),
    ];

    TOPICS.iter().find_map(|(topic, keywords)| {
        keywords
            .iter()
            .any(|keyword| tokens.contains(*keyword))
            .then(|| (*topic).to_owned())
    })
}

fn normalized_review_tokens(excerpt: &str) -> BTreeSet<String> {
    excerpt
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|token| token.len() >= 2)
        .map(str::to_ascii_lowercase)
        .filter(|token| !review_stopword(token))
        .collect()
}

fn review_stopword(token: &str) -> bool {
    matches!(
        token,
        "about"
            | "after"
            | "also"
            | "and"
            | "are"
            | "before"
            | "but"
            | "for"
            | "from"
            | "has"
            | "into"
            | "must"
            | "not"
            | "should"
            | "that"
            | "the"
            | "this"
            | "through"
            | "to"
            | "use"
            | "when"
            | "with"
    )
}

fn review_candidate_kind(spans: &[&StoredEvidenceSpan]) -> String {
    let joined = spans
        .iter()
        .map(|span| span.excerpt.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if ["failed", "failure", "panic", "regression"]
        .iter()
        .any(|term| joined.contains(term))
    {
        "failure".to_owned()
    } else if ["adr", "decided", "decision", "choose", "chose"]
        .iter()
        .any(|term| joined.contains(term))
    {
        "decision".to_owned()
    } else {
        "rule".to_owned()
    }
}

fn review_candidate_content(
    topic_key: &str,
    candidate_kind: &str,
    spans: &[&StoredEvidenceSpan],
) -> String {
    let excerpts = spans
        .iter()
        .take(2)
        .map(|span| compact_excerpt(&span.excerpt))
        .collect::<Vec<_>>()
        .join(" / ");
    match candidate_kind {
        "failure" => format!(
            "When `{topic_key}` work resembles this session, check the prior failure evidence before repeating it: {excerpts}"
        ),
        "decision" => format!(
            "For `{topic_key}` work, preserve the evidence-backed decision from this session: {excerpts}"
        ),
        _ => format!(
            "For `{topic_key}` work, follow the evidence-backed procedure shown in this session: {excerpts}"
        ),
    }
}

fn compact_excerpt(excerpt: &str) -> String {
    const MAX_CHARS: usize = 180;
    let compact = excerpt.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_CHARS {
        return compact;
    }
    let mut truncated = compact.chars().take(MAX_CHARS).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn review_candidate_confidence(span_count: usize) -> f32 {
    (0.45_f32 + (span_count.min(6) as f32 * 0.08)).min(0.85)
}

fn deterministic_curate_id(parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let hash = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    let candidate = CandidateId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string();
    format!("curate_{}", candidate.trim_start_matches("cand_"))
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
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("review_state")
        || trimmed.eq_ignore_ascii_case("review-state")
        || trimmed.eq_ignore_ascii_case("state")
        || trimmed.eq_ignore_ascii_case("queue")
    {
        Ok(CurateCandidateSortMode::ReviewState)
    } else if trimmed.eq_ignore_ascii_case("created_at")
        || trimmed.eq_ignore_ascii_case("created-at")
        || trimmed.eq_ignore_ascii_case("created")
        || trimmed.eq_ignore_ascii_case("time")
    {
        Ok(CurateCandidateSortMode::CreatedAt)
    } else if trimmed.eq_ignore_ascii_case("confidence") || trimmed.eq_ignore_ascii_case("score") {
        Ok(CurateCandidateSortMode::Confidence)
    } else {
        Err(curate_usage_error(
            format!(
                "Unknown curate candidates sort mode `{raw}`; expected review_state, created_at, or confidence"
            ),
            "ee curate candidates --help",
        ))
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
    let runtime_structural_decay_enabled = if options.structural_decay {
        structural_decay_feature_enabled(&prepared.workspace_path)?
    } else {
        false
    };
    if options.structural_decay && !runtime_structural_decay_enabled {
        push_structural_decay_feature_disabled_degradation(&mut degraded);
    }
    let structural_adjustments = if options.structural_decay && runtime_structural_decay_enabled {
        curate_structural_decay_adjustments(
            &connection,
            &candidates,
            &policy_map,
            &now,
            &mut degraded,
        )?
    } else {
        BTreeMap::new()
    };

    let mut decisions = Vec::new();
    let disposition_context = CurateDispositionContext {
        policies: &policy_map,
        now: &now,
        apply: options.apply,
        actor: &actor,
        connection: &connection,
    };
    for candidate in &candidates {
        let decision = evaluate_candidate_for_disposition(
            candidate,
            &disposition_context,
            structural_adjustments.get(&candidate.id),
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
        structural_adjustments: structural_adjustments.into_values().collect(),
        degraded,
        next_action,
    })
}

fn structural_decay_feature_enabled(workspace_path: &Path) -> Result<bool, DomainError> {
    let path = workspace_path.join(".ee").join("config.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(DomainError::Configuration {
                message: format!(
                    "Failed to read workspace curation config {}: {error}",
                    path.display()
                ),
                repair: Some("Fix or remove .ee/config.toml.".to_owned()),
            });
        }
    };
    let config = ConfigFile::parse(&contents).map_err(|error| DomainError::Configuration {
        message: format!(
            "Failed to parse workspace curation config {}: {error}",
            path.display()
        ),
        repair: Some("Fix [graph.feature.structural_decay] in .ee/config.toml.".to_owned()),
    })?;
    Ok(config
        .graph
        .feature
        .structural_decay_enabled
        .unwrap_or(false))
}

fn push_structural_decay_feature_disabled_degradation(
    degraded: &mut Vec<CurateCandidatesDegradation>,
) {
    degraded.push(CurateCandidatesDegradation {
        code: "graph_feature_disabled".to_owned(),
        severity: "medium".to_owned(),
        message: "Structural curation decay is disabled by runtime graph feature flag.".to_owned(),
        repair: format!("ee config set {GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY} true"),
    });
}

/// Retire a curation candidate from the active review set with an audited record.
pub fn run_curate_retire(
    options: &CurateRetireOptions<'_>,
) -> Result<CurateRetireReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let actor = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let reason = options
        .reason
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let retired_at = Utc::now().to_rfc3339();

    let next_action = "ee curate candidates --status=retired --json".to_owned();

    if options.dry_run {
        return Ok(CurateRetireReport {
            schema: CURATE_RETIRE_SCHEMA_V1,
            command: "curate retire",
            version: env!("CARGO_PKG_VERSION"),
            workspace_id: prepared.workspace_id,
            workspace_path: prepared.workspace_path.display().to_string(),
            database_path: prepared.database_path.display().to_string(),
            candidate_id: options.candidate_id.to_owned(),
            from_status: "pending".to_owned(),
            to_status: "retired".to_owned(),
            reason,
            retired_at,
            retired_by: actor,
            dry_run: true,
            persisted: false,
            audit_id: None,
            degraded: Vec::new(),
            next_action,
        });
    }

    let connection = open_existing_database(&prepared.database_path)?;
    let candidate = connection
        .get_curation_candidate(&prepared.workspace_id, options.candidate_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to fetch curation candidate: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "curation_candidate".to_owned(),
            id: options.candidate_id.to_owned(),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;

    let from_status = candidate.status.clone();
    let to_status = CandidateStatus::Rejected.as_str();

    let audit_id = generate_audit_id();
    let details = serde_json::json!({
        "from_status": from_status,
        "to_status": to_status,
        "reason": reason,
        "retired_at": retired_at,
    })
    .to_string();
    let audit_input = CreateAuditInput {
        workspace_id: Some(prepared.workspace_id.clone()),
        actor: actor.clone(),
        action: audit_actions::CURATION_CANDIDATE_RETIRE.to_string(),
        target_type: Some("curation_candidate".to_string()),
        target_id: Some(options.candidate_id.to_owned()),
        details: Some(details),
    };

    connection
        .insert_audit(&audit_id, &audit_input)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to create audit record: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let actor_str = actor.as_deref().unwrap_or("ee");
    let update = CurationCandidateReviewUpdate {
        status: to_status,
        review_state: ReviewQueueState::Rejected.as_str(),
        reviewed_at: &retired_at,
        reviewed_by: actor_str,
        snoozed_until: None,
        merged_into_candidate_id: None,
        ttl_policy_id: None,
    };
    connection
        .update_curation_candidate_review(&prepared.workspace_id, options.candidate_id, update)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to retire curation candidate: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    Ok(CurateRetireReport {
        schema: CURATE_RETIRE_SCHEMA_V1,
        command: "curate retire",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        candidate_id: options.candidate_id.to_owned(),
        from_status,
        to_status: to_status.to_owned(),
        reason,
        retired_at,
        retired_by: actor,
        dry_run: false,
        persisted: true,
        audit_id: Some(audit_id),
        degraded: Vec::new(),
        next_action,
    })
}

/// Write a tombstone audit record for a memory without deleting the row.
pub fn run_curate_tombstone(
    options: &CurateTombstoneOptions<'_>,
) -> Result<CurateTombstoneReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let actor = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let reason = options
        .reason
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let tombstoned_at = Utc::now().to_rfc3339();

    let next_action = "ee memory list --json".to_owned();

    if options.dry_run {
        return Ok(CurateTombstoneReport {
            schema: CURATE_TOMBSTONE_SCHEMA_V1,
            command: "curate tombstone",
            version: env!("CARGO_PKG_VERSION"),
            workspace_id: prepared.workspace_id,
            workspace_path: prepared.workspace_path.display().to_string(),
            database_path: prepared.database_path.display().to_string(),
            memory_id: options.memory_id.to_owned(),
            reason,
            tombstoned_at,
            tombstoned_by: actor,
            dry_run: true,
            persisted: false,
            audit_id: None,
            degraded: Vec::new(),
            next_action,
        });
    }

    let connection = open_existing_database(&prepared.database_path)?;
    let memory = connection
        .get_memory(options.memory_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to fetch memory: {error}"),
            repair: Some("ee memory list --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "memory".to_owned(),
            id: options.memory_id.to_owned(),
            repair: Some("ee memory list --json".to_owned()),
        })?;

    if memory.tombstoned_at.is_some() {
        return Err(DomainError::Usage {
            message: format!("Memory {} is already tombstoned.", options.memory_id),
            repair: Some("ee memory list --json".to_owned()),
        });
    }

    let audit_id = connection
        .tombstone_memory_audited(
            options.memory_id,
            &prepared.workspace_id,
            actor.as_deref(),
            reason.as_deref(),
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to tombstone memory: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
        .ok_or_else(|| DomainError::Storage {
            message: format!(
                "Failed to tombstone memory {}: no row updated.",
                options.memory_id
            ),
            repair: Some("ee memory list --json".to_owned()),
        })?;

    Ok(CurateTombstoneReport {
        schema: CURATE_TOMBSTONE_SCHEMA_V1,
        command: "curate tombstone",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        memory_id: options.memory_id.to_owned(),
        reason,
        tombstoned_at,
        tombstoned_by: actor,
        dry_run: false,
        persisted: true,
        audit_id: Some(audit_id),
        degraded: Vec::new(),
        next_action,
    })
}

/// Restore a tombstoned memory row and record an audit entry.
pub fn run_curate_untombstone(
    options: &CurateUntombstoneOptions<'_>,
) -> Result<CurateUntombstoneReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let actor = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let reason = options
        .reason
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let restored_at = Utc::now().to_rfc3339();

    let next_action = format!("ee memory show {} --json", options.memory_id);

    if options.dry_run {
        return Ok(CurateUntombstoneReport {
            schema: CURATE_UNTOMBSTONE_SCHEMA_V1,
            command: "curate untombstone",
            version: env!("CARGO_PKG_VERSION"),
            workspace_id: prepared.workspace_id,
            workspace_path: prepared.workspace_path.display().to_string(),
            database_path: prepared.database_path.display().to_string(),
            memory_id: options.memory_id.to_owned(),
            reason,
            previous_tombstoned_at: None,
            restored_at,
            restored_by: actor,
            dry_run: true,
            persisted: false,
            audit_id: None,
            degraded: Vec::new(),
            next_action,
        });
    }

    let connection = open_existing_database(&prepared.database_path)?;
    let memory = connection
        .get_memory(options.memory_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to fetch memory: {error}"),
            repair: Some("ee memory list --json".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "memory".to_owned(),
            id: options.memory_id.to_owned(),
            repair: Some("ee memory list --json".to_owned()),
        })?;

    if memory.workspace_id != prepared.workspace_id {
        return Err(DomainError::NotFound {
            resource: "memory".to_owned(),
            id: options.memory_id.to_owned(),
            repair: Some("ee memory list --json".to_owned()),
        });
    }

    let previous_tombstoned_at =
        memory
            .tombstoned_at
            .clone()
            .ok_or_else(|| DomainError::Usage {
                message: format!("Memory {} is not tombstoned.", options.memory_id),
                repair: Some("ee memory list --json".to_owned()),
            })?;

    let details = serde_json::json!({
        "previous_tombstoned_at": previous_tombstoned_at,
        "restored_at": restored_at,
        "reason": reason,
    })
    .to_string();

    let audit_id = connection
        .untombstone_memory_audited(
            options.memory_id,
            &prepared.workspace_id,
            actor.as_deref(),
            &restored_at,
            &details,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to restore memory: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
        .ok_or_else(|| DomainError::Storage {
            message: format!(
                "Failed to restore memory {}: no row updated.",
                options.memory_id
            ),
            repair: Some("ee memory list --json".to_owned()),
        })?;

    Ok(CurateUntombstoneReport {
        schema: CURATE_UNTOMBSTONE_SCHEMA_V1,
        command: "curate untombstone",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        memory_id: options.memory_id.to_owned(),
        reason,
        previous_tombstoned_at: Some(previous_tombstoned_at),
        restored_at,
        restored_by: actor,
        dry_run: false,
        persisted: true,
        audit_id: Some(audit_id),
        degraded: Vec::new(),
        next_action,
    })
}

/// Review workspace evidence and propose curation candidates.
pub fn run_review_workspace(
    options: &ReviewWorkspaceOptions<'_>,
) -> Result<ReviewWorkspaceReport, DomainError> {
    let prepared = prepare_curate_read(options.workspace_path, options.database_path)?;
    let scope_path = options
        .scope
        .map(Path::to_path_buf)
        .unwrap_or_else(|| prepared.workspace_path.clone());

    let next_action = if options.propose && !options.dry_run {
        "ee curate candidates --json".to_owned()
    } else {
        "ee review workspace --propose --json".to_owned()
    };

    if options.dry_run {
        return Ok(ReviewWorkspaceReport {
            schema: REVIEW_WORKSPACE_SCHEMA_V1,
            command: "review workspace",
            version: env!("CARGO_PKG_VERSION"),
            workspace_id: prepared.workspace_id,
            workspace_path: prepared.workspace_path.display().to_string(),
            database_path: prepared.database_path.display().to_string(),
            scope_path: scope_path.display().to_string(),
            include_cass: options.include_cass,
            propose_mode: options.propose,
            dry_run: true,
            durable_mutation: false,
            memory_count: 0,
            evidence_count: 0,
            candidate_count: 0,
            candidates: Vec::new(),
            degraded: Vec::new(),
            next_action,
        });
    }

    let connection = open_existing_database(&prepared.database_path)?;

    let memories = connection
        .list_memories(&prepared.workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories: {error}"),
            repair: Some("ee memory list --json".to_owned()),
        })?;

    let mut degraded = Vec::new();
    let evidence_count = if options.include_cass {
        degraded.push(CurateCandidatesDegradation {
            code: "cass_evidence_not_available".to_owned(),
            severity: "low".to_owned(),
            message: "CASS evidence span listing not yet implemented for workspace scope."
                .to_owned(),
            repair: "Use `ee review session` for session-specific evidence review.".to_owned(),
        });
        0
    } else {
        0
    };

    let memory_count = memories.len();

    let mut candidates = Vec::new();
    let mut durable_mutation = false;

    if options.propose {
        for memory in &memories {
            if memory.tombstoned_at.is_some() {
                continue;
            }
            let candidate_id = format!("curate_{}", generate_audit_id());
            let content_hash = blake3::hash(memory.content.as_bytes()).to_hex().to_string();

            let already_exists = connection
                .get_curation_candidate(&prepared.workspace_id, &candidate_id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to check existing curation candidate: {error}"),
                    repair: Some("ee curate candidates --json".to_owned()),
                })?
                .is_some();

            let persisted = if already_exists {
                false
            } else {
                connection
                    .insert_curation_candidate(
                        &candidate_id,
                        &CreateCurationCandidateInput {
                            workspace_id: prepared.workspace_id.clone(),
                            candidate_type: "review".to_owned(),
                            target_memory_id: memory.id.clone(),
                            proposed_content: Some(memory.content.clone()),
                            proposed_confidence: Some(memory.confidence),
                            proposed_trust_class: None,
                            source_type: "workspace_review".to_owned(),
                            source_id: Some(memory.id.clone()),
                            reason: "Workspace evidence review".to_owned(),
                            confidence: memory.confidence,
                            status: Some(CandidateStatus::Pending.as_str().to_owned()),
                            created_at: None,
                            ttl_expires_at: None,
                        },
                    )
                    .map_err(|error| DomainError::Storage {
                        message: format!("Failed to insert curation candidate: {error}"),
                        repair: Some("ee curate candidates --json".to_owned()),
                    })?;
                durable_mutation = true;
                true
            };

            let candidate = ReviewSessionCandidate {
                candidate_id: candidate_id.clone(),
                candidate_type: "review".to_owned(),
                candidate_kind: "workspace_memory".to_owned(),
                topic_key: memory.kind.clone(),
                target_memory_id: memory.id.clone(),
                proposed_content: memory.content.clone(),
                proposed_confidence: memory.confidence,
                source_type: "workspace_review".to_owned(),
                source_ids: vec![memory.id.clone()],
                reason: "Workspace evidence review".to_owned(),
                confidence: memory.confidence,
                content_hash,
                persisted,
            };
            candidates.push(candidate);
        }
    }

    Ok(ReviewWorkspaceReport {
        schema: REVIEW_WORKSPACE_SCHEMA_V1,
        command: "review workspace",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        scope_path: scope_path.display().to_string(),
        include_cass: options.include_cass,
        propose_mode: options.propose,
        dry_run: false,
        durable_mutation,
        memory_count,
        evidence_count,
        candidate_count: candidates.len(),
        candidates,
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
    rule_create: Option<ApplyRuleCurationInput>,
    procedure_create: Option<ApplyProcedureCurationInput>,
    tombstone_memory: bool,
    target_before: Option<CurateApplyMemoryState>,
    target_after: Option<CurateApplyMemoryState>,
    next_action: String,
}

#[derive(Clone, Debug)]
struct ApplyRuleCurationInput {
    rule_id: String,
    rule: CreateProceduralRuleInput,
    index_job_id: String,
    index_job: CreateSearchIndexJobInput,
}

#[derive(Clone, Debug)]
struct ApplyProcedureCurationInput {
    procedure_id: String,
    procedure: CreateProcedureInput,
    event_id: String,
    event: CreateProcedureEventInput,
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
                rule_create: None,
                procedure_create: None,
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

    if stored.proposed_trust_class.is_some() {
        let source_type = match CandidateSource::from_str(&stored.source_type) {
            Ok(source_type) => source_type,
            Err(error) => {
                errors.push(validation_issue(
                    "invalid_candidate_source",
                    error.to_string(),
                    "Regenerate the candidate with a supported source type.",
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
        match stored
            .source_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(source_id) => {
                if let Err(error) = validate_candidate_trust_evidence(
                    stored.proposed_trust_class.as_deref(),
                    source_type,
                    source_id,
                ) {
                    errors.push(validation_issue(
                        error.code(),
                        error.to_string(),
                        validation_repair(&error),
                    ));
                }
            }
            None => {
                let error = CandidateValidationError::MissingSourceEvidence;
                errors.push(validation_issue(
                    error.code(),
                    error.to_string(),
                    validation_repair(&error),
                ));
            }
        }
    }

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
    let mut rule_create = None;
    let mut procedure_create = None;
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
                    let redaction = crate::policy::redact_secret_like_content(content);
                    if redaction.redacted {
                        warnings.push(validation_issue(
                            "proposed_content_redacted",
                            format!(
                                "Proposed content for {candidate_type} contained secret-like values and was redacted before memory update."
                            ),
                            "Review the curation candidate and keep only durable, non-secret evidence.",
                        ));
                    }
                    push_apply_change(
                        &mut changes,
                        "content",
                        Some(target_memory.content.clone()),
                        Some(redaction.content.clone()),
                    );
                    target_after.content = redaction.content;
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
            if candidate_type == CandidateType::Promote && target_memory.level == "episodic" {
                push_apply_change(
                    &mut changes,
                    "level",
                    Some(target_after.level.clone()),
                    Some("semantic".to_owned()),
                );
                target_after.level = "semantic".to_owned();
            }
        }
        CandidateType::Rule | CandidateType::Procedure => {
            let proposed_content = stored.proposed_content.as_deref().map(str::trim);
            match proposed_content.filter(|value| !value.is_empty()) {
                Some(content) => {
                    let redaction = crate::policy::redact_secret_like_content(content);
                    if redaction.redacted {
                        warnings.push(validation_issue(
                            "proposed_content_redacted",
                            "Proposed rule content contained secret-like values and was redacted before rule creation.",
                            "Review the candidate and keep only durable, non-secret procedural guidance.",
                        ));
                    }
                    if candidate_type == CandidateType::Rule {
                        let source_memory_ids = source_memory_ids_for_rule_candidate(stored);
                        let rule_id = RuleId::now().to_string();
                        let index_job_id = generate_rule_search_index_job_id();
                        push_apply_change(&mut changes, "ruleId", None, Some(rule_id.clone()));
                        push_apply_change(
                            &mut changes,
                            "ruleContent",
                            None,
                            Some(redaction.content.clone()),
                        );
                        push_apply_change(
                            &mut changes,
                            "sourceMemoryCount",
                            None,
                            Some(source_memory_ids.len().to_string()),
                        );
                        push_apply_change(
                            &mut changes,
                            "ruleConfidence",
                            None,
                            Some(format_score(
                                stored.proposed_confidence.unwrap_or(stored.confidence),
                            )),
                        );
                        let rule_trust_class = stored
                            .proposed_trust_class
                            .clone()
                            .unwrap_or_else(|| "agent_assertion".to_owned());
                        push_apply_change(
                            &mut changes,
                            "ruleTrustClass",
                            None,
                            Some(rule_trust_class.clone()),
                        );
                        rule_create = Some(ApplyRuleCurationInput {
                            rule_id: rule_id.clone(),
                            rule: CreateProceduralRuleInput {
                                workspace_id: stored.workspace_id.clone(),
                                content: redaction.content,
                                confidence: stored.proposed_confidence.unwrap_or(stored.confidence),
                                utility: target_memory.utility,
                                importance: target_memory.importance,
                                trust_class: rule_trust_class,
                                scope: "workspace".to_owned(),
                                scope_pattern: None,
                                maturity: "candidate".to_owned(),
                                protected: false,
                                source_memory_ids,
                                tags: vec!["playbook".to_owned(), "extracted".to_owned()],
                            },
                            index_job_id,
                            index_job: CreateSearchIndexJobInput {
                                workspace_id: stored.workspace_id.clone(),
                                job_type: SearchIndexJobType::SingleDocument,
                                document_source: Some("rule".to_owned()),
                                document_id: Some(rule_id),
                                documents_total: 1,
                            },
                        });
                    } else {
                        let procedure_id = generate_procedure_id();
                        let event_id = generate_procedure_event_id(&procedure_id);
                        let evidence_uris = procedure_evidence_uris(stored, target_memory);
                        push_apply_change(
                            &mut changes,
                            "procedureId",
                            None,
                            Some(procedure_id.clone()),
                        );
                        push_apply_change(
                            &mut changes,
                            "procedureMaturity",
                            None,
                            Some("provisional".to_owned()),
                        );
                        push_apply_change(
                            &mut changes,
                            "procedureEvidenceCount",
                            None,
                            Some(evidence_uris.len().to_string()),
                        );
                        procedure_create = Some(ApplyProcedureCurationInput {
                            procedure_id: procedure_id.clone(),
                            procedure: CreateProcedureInput {
                                workspace_id: stored.workspace_id.clone(),
                                name: target_memory.kind.clone(),
                                body: redaction.content,
                                level: "procedural".to_owned(),
                                maturity: "provisional".to_owned(),
                                confidence: stored.proposed_confidence.unwrap_or(stored.confidence),
                                utility: target_memory.utility,
                                importance: target_memory.importance,
                                evidence_uris: evidence_uris.clone(),
                                created_at: None,
                            },
                            event_id,
                            event: CreateProcedureEventInput {
                                workspace_id: stored.workspace_id.clone(),
                                procedure_id,
                                event_type: "curation_apply".to_owned(),
                                from_maturity: None,
                                to_maturity: Some("provisional".to_owned()),
                                reason: Some(stored.reason.clone()),
                                evidence_uris,
                                actor: None,
                                created_at: None,
                            },
                        });
                    }
                }
                None => errors.push(validation_issue(
                    CandidateValidationError::ContentRequiredForType { candidate_type }.code(),
                    format!("proposed content is required for {candidate_type} candidates"),
                    "Validate or recreate the candidate with proposed procedural content.",
                )),
            }
        }
    }

    if candidate_type != CandidateType::Rule
        && candidate_type != CandidateType::Procedure
        && let Some(confidence) = stored.proposed_confidence
    {
        push_apply_change(
            &mut changes,
            "confidence",
            Some(format_score(target_memory.confidence)),
            Some(format_score(confidence)),
        );
        target_after.confidence = confidence;
    }
    if candidate_type != CandidateType::Rule
        && candidate_type != CandidateType::Procedure
        && let Some(trust_class) = &stored.proposed_trust_class
    {
        push_apply_change(
            &mut changes,
            "trustClass",
            Some(target_memory.trust_class.clone()),
            Some(trust_class.clone()),
        );
        target_after.trust_class = trust_class.clone();
    }
    if (rule_create.is_some() || procedure_create.is_some()) && target_after.level != "procedural" {
        push_apply_change(
            &mut changes,
            "level",
            Some(target_after.level.clone()),
            Some("procedural".to_owned()),
        );
        target_after.level = "procedural".to_owned();
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

    if !tombstone_memory && rule_create.is_none() && procedure_create.is_none() {
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
            decision: if rule_create.is_some() {
                "create_rule".to_owned()
            } else if procedure_create.is_some() {
                "create_procedure".to_owned()
            } else if tombstone_memory {
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
        rule_create,
        procedure_create,
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

fn curate_structural_decay_adjustments(
    connection: &DbConnection,
    candidates: &[StoredCurationCandidate],
    policies: &BTreeMap<&str, &StoredCurationTtlPolicy>,
    now: &DateTime<Utc>,
    degraded: &mut Vec<CurateCandidatesDegradation>,
) -> Result<BTreeMap<String, CurateStructuralDecayAdjustment>, DomainError> {
    let memory_ids = candidates
        .iter()
        .map(|candidate| candidate.target_memory_id.clone())
        .collect::<BTreeSet<_>>();
    let links = connection
        .list_all_memory_links(None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memory links for structural decay: {error}"),
            repair: Some("ee graph project --json".to_owned()),
        })?;
    let visible_links = links
        .into_iter()
        .filter(|link| {
            crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
        })
        .collect::<Vec<_>>();
    let graph = curate_structural_decay_graph(&memory_ids, &visible_links);
    push_structural_decay_connectivity_degradation(&graph, degraded);
    let mut adjustments = BTreeMap::new();

    for candidate in candidates {
        let review_state = normalized_review_state(candidate);
        let policy_id = candidate
            .ttl_policy_id
            .as_deref()
            .unwrap_or_else(|| default_curation_ttl_policy_id_for_review_state(&review_state));
        let Some(policy) = policies.get(policy_id).copied() else {
            continue;
        };
        let entered_raw = candidate
            .state_entered_at
            .as_deref()
            .or(candidate.reviewed_at.as_deref())
            .or(candidate.applied_at.as_deref())
            .unwrap_or(candidate.created_at.as_str());
        let state_entered = DateTime::parse_from_rfc3339(entered_raw)
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .unwrap_or(*now);
        let elapsed_seconds = now
            .signed_duration_since(state_entered)
            .num_seconds()
            .max(0);
        let base_decay = if policy.threshold_seconds == 0 {
            1.0
        } else {
            (elapsed_seconds as f64 / policy.threshold_seconds as f64).clamp(0.0, 1.0) as f32
        };
        let structural = compute_structural_decay_adjustment(&graph, &candidate.target_memory_id);
        let adjustment = curate_structural_decay_adjustment(
            &candidate.id,
            &candidate.target_memory_id,
            policy.threshold_seconds,
            base_decay,
            structural,
        );
        adjustments.insert(candidate.id.clone(), adjustment);
    }

    Ok(adjustments)
}

fn push_structural_decay_connectivity_degradation(
    graph: &Graph,
    degraded: &mut Vec<CurateCandidatesDegradation>,
) {
    let connectivity = compute_structural_decay_connectivity(graph);
    if connectivity.component_count <= 1 {
        return;
    }

    degraded.push(CurateCandidatesDegradation {
        code: GRAPH_CURATE_DISCONNECTED_GRAPH_CODE.to_owned(),
        severity: "warning".to_owned(),
        message: format!(
            "Structural curation graph has {} connected components; structural decay adjustments may be local to disconnected components.",
            connectivity.component_count
        ),
        repair: "Run `ee graph snapshot refresh --workspace .`, then `ee health --robot-insights --json`.".to_owned(),
    });
}

fn curate_structural_decay_graph(
    memory_ids: &BTreeSet<String>,
    links: &[StoredMemoryLink],
) -> Graph {
    let mut graph_memory_ids = memory_ids.clone();
    for link in links {
        if memory_ids.contains(&link.src_memory_id) || memory_ids.contains(&link.dst_memory_id) {
            graph_memory_ids.insert(link.src_memory_id.clone());
            graph_memory_ids.insert(link.dst_memory_id.clone());
        }
    }

    let mut graph = Graph::new(CompatibilityMode::Strict);
    for memory_id in &graph_memory_ids {
        graph.add_node(memory_id);
    }
    for link in links {
        if !graph_memory_ids.contains(&link.src_memory_id)
            || !graph_memory_ids.contains(&link.dst_memory_id)
        {
            continue;
        }
        graph.add_node(&link.src_memory_id);
        graph.add_node(&link.dst_memory_id);
        let _ = graph
            .extend_edges_unrecorded([(link.src_memory_id.as_str(), link.dst_memory_id.as_str())]);
    }
    graph
}

fn curate_structural_decay_adjustment(
    candidate_id: &str,
    memory_id: &str,
    base_threshold_seconds: u64,
    base_decay: f32,
    structural: StructuralDecayMultiplier,
) -> CurateStructuralDecayAdjustment {
    let structural_multiplier = (structural.structural_multiplier as f32).clamp(0.000_001, 1000.0);
    let adjusted_decay = (base_decay * structural_multiplier).clamp(0.0, 1.0);
    let adjusted_ttl_threshold_seconds = ((base_threshold_seconds as f64)
        / f64::from(structural_multiplier))
    .ceil()
    .clamp(1.0, u64::MAX as f64) as u64;
    CurateStructuralDecayAdjustment {
        candidate_id: candidate_id.to_owned(),
        memory_id: memory_id.to_owned(),
        onion_layer: structural.onion_layer,
        max_layer: structural.max_layer,
        is_articulation_point: structural.is_articulation_point,
        base_decay,
        structural_multiplier,
        adjusted_decay,
        adjusted_ttl_threshold_seconds,
        rationale: structural.rationale,
    }
}

struct CurateDispositionContext<'ctx, 'policy> {
    policies: &'ctx BTreeMap<&'policy str, &'policy StoredCurationTtlPolicy>,
    now: &'ctx DateTime<Utc>,
    apply: bool,
    actor: &'ctx str,
    connection: &'ctx DbConnection,
}

fn evaluate_candidate_for_disposition(
    stored: &StoredCurationCandidate,
    context: &CurateDispositionContext<'_, '_>,
    structural_adjustment: Option<&CurateStructuralDecayAdjustment>,
    degraded: &mut Vec<CurateCandidatesDegradation>,
) -> Result<CurateDispositionDecision, DomainError> {
    let policies = context.policies;
    let now = context.now;
    let apply = context.apply;
    let actor = context.actor;
    let connection = context.connection;
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

    let threshold_seconds = structural_adjustment.map_or(policy.threshold_seconds, |adjustment| {
        adjustment.adjusted_ttl_threshold_seconds
    });
    let threshold = duration_from_seconds(threshold_seconds, "threshold_seconds")?;
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
            ttl_threshold_seconds: threshold_seconds,
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
            ttl_threshold_seconds: threshold_seconds,
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
                    "Curation candidate {} requires harmful-feedback escalation review.",
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
        ttl_threshold_seconds: threshold_seconds,
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
        rule_create: None,
        procedure_create: None,
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
    chrono::Duration::try_seconds(seconds).ok_or_else(|| DomainError::Storage {
        message: format!("Curation TTL {field} exceeds supported duration range."),
        repair: Some("Repair the curation_ttl_policies table.".to_owned()),
    })
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
        CandidateValidationError::TrustPromotionEvidenceRejected { .. } => {
            "Attach evidence from the required durable ID namespace for this trust class."
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
        CandidateValidationError::InvalidTtlBaseTimestamp { .. } => {
            "Use an RFC 3339 timestamp as the TTL base time."
        }
        CandidateValidationError::TtlSecondsOutOfRange { .. }
        | CandidateValidationError::TtlExpiryOutOfRange { .. } => {
            "Use a TTL that fits within the supported timestamp range."
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
        level: memory.level.clone(),
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

fn source_memory_ids_for_rule_candidate(stored: &StoredCurationCandidate) -> Vec<String> {
    let mut ids = BTreeSet::new();
    if let Some(source_id) = stored.source_id.as_deref() {
        for raw in source_id
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            if MemoryId::from_str(raw).is_ok() {
                ids.insert(raw.to_owned());
            }
        }
    }
    if ids.is_empty() {
        ids.insert(stored.target_memory_id.clone());
    }
    ids.into_iter().collect()
}

fn generate_rule_search_index_job_id() -> String {
    let rule_id = RuleId::now().to_string();
    let payload = rule_id.trim_start_matches("rule_");
    format!("sidx_{payload}")
}

fn generate_procedure_id() -> String {
    let mut payload = uuid::Uuid::now_v7().simple().to_string();
    payload.truncate(26);
    format!("proc_{payload}")
}

fn generate_procedure_event_id(procedure_id: &str) -> String {
    let hash = blake3::hash(procedure_id.as_bytes()).to_hex().to_string();
    format!("pevt_{}", &hash[..26])
}

fn procedure_evidence_uris(
    stored: &StoredCurationCandidate,
    target_memory: &StoredMemory,
) -> Vec<String> {
    let mut uris = BTreeSet::new();
    uris.insert(format!("memory://{}", target_memory.id));
    if let Some(source_id) = stored.source_id.as_deref() {
        for raw in source_id
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            uris.insert(format!("curation-source://{raw}"));
        }
    }
    uris.into_iter().collect()
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
        let changed = connection
            .tombstone_memory(&stored.target_memory_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to tombstone target memory: {error}"),
                repair: Some("ee memory show <memory-id> --json".to_owned()),
            })?;
        if changed {
            let previous_level = decision
                .target_before
                .as_ref()
                .map(|state| state.level.clone())
                .unwrap_or_else(|| "unknown".to_owned());
            let _ = connection
                .insert_memory_level_transition_audit(&MemoryLevelTransitionAuditInput {
                    workspace_id: workspace_id.to_owned(),
                    actor: Some(applied_by.to_owned()),
                    memory_id: stored.target_memory_id.clone(),
                    previous_level,
                    new_level: "tombstoned".to_owned(),
                    reason: "manual_tombstone".to_owned(),
                    automatic: false,
                    event: "manual.tombstone".to_owned(),
                    evidence_refs: vec![stored.id.clone()],
                    source_action: Some(audit_actions::CURATION_CANDIDATE_APPLY.to_owned()),
                })
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to write memory level transition audit: {error}"),
                    repair: Some("ee memory history <memory-id> --json".to_owned()),
                })?;
        }
        changed
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
    let mut created_rule_id = None;
    let mut created_procedure_id = None;
    if let Some(rule_create) = &decision.rule_create {
        connection
            .insert_procedural_rule(&rule_create.rule_id, &rule_create.rule)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to create procedural rule from curation candidate: {error}"
                ),
                repair: Some("ee rule list --json".to_owned()),
            })?;
        connection
            .insert_search_index_job(&rule_create.index_job_id, &rule_create.index_job)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to queue procedural rule indexing: {error}"),
                repair: Some("ee index rebuild --workspace .".to_owned()),
            })?;
        created_rule_id = Some(rule_create.rule_id.clone());
    }
    if let Some(procedure_create) = &decision.procedure_create {
        connection
            .insert_procedure(&procedure_create.procedure_id, &procedure_create.procedure)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to create procedure from curation candidate: {error}"),
                repair: Some("ee procedure list --json".to_owned()),
            })?;
        connection
            .insert_procedure_event(&procedure_create.event_id, &procedure_create.event)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to record procedure curation history event: {error}"),
                repair: Some("ee procedure show <id> --json".to_owned()),
            })?;
        created_procedure_id = Some(procedure_create.procedure_id.clone());
    }
    if let Some((previous_level, new_level)) = applied_level_change(
        decision.target_before.as_ref(),
        decision.target_after.as_ref(),
    ) {
        let evidence_refs =
            level_transition_evidence_refs(stored, &created_rule_id, &created_procedure_id);
        let (reason, event, automatic) =
            curate_level_transition_metadata(&stored.candidate_type, &previous_level, &new_level);
        let _ = connection
            .apply_memory_level_transition_in_current_transaction(
                &stored.target_memory_id,
                &ApplyMemoryLevelTransitionInput {
                    workspace_id: workspace_id.to_owned(),
                    expected_level: Some(previous_level.clone()),
                    level: new_level,
                    updated_at: applied_at.to_owned(),
                    actor: Some(applied_by.to_owned()),
                    reason,
                    automatic,
                    event,
                    evidence_refs,
                    source_action: Some(audit_actions::CURATION_CANDIDATE_APPLY.to_owned()),
                },
            )
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to apply memory level transition from curation apply: {error}"
                ),
                repair: Some("ee memory history <memory-id> --json".to_owned()),
            })?;
    }
    if !memory_changed && created_rule_id.is_none() && created_procedure_id.is_none() {
        return Err(DomainError::Storage {
            message: format!(
                "Curation candidate {} did not mutate target memory {} or create a rule/procedure.",
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
        "createdRuleId": created_rule_id.as_deref(),
        "createdProcedureId": created_procedure_id.as_deref(),
        "changes": &decision.application.changes,
    })
    .to_string();
    let target_type = if created_rule_id.is_some() {
        "rule"
    } else if created_procedure_id.is_some() {
        "procedure"
    } else {
        "memory"
    };
    let target_id = created_rule_id
        .as_deref()
        .or(created_procedure_id.as_deref())
        .unwrap_or(stored.target_memory_id.as_str());
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some(applied_by.to_owned()),
                action: audit_actions::CURATION_CANDIDATE_APPLY.to_owned(),
                target_type: Some(target_type.to_owned()),
                target_id: Some(target_id.to_owned()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to write curation apply audit entry: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn applied_level_change(
    before: Option<&CurateApplyMemoryState>,
    after: Option<&CurateApplyMemoryState>,
) -> Option<(String, String)> {
    let before = before?;
    let after = after?;
    if before.tombstoned || after.tombstoned || before.level == after.level {
        return None;
    }
    Some((before.level.clone(), after.level.clone()))
}

fn level_transition_evidence_refs(
    stored: &StoredCurationCandidate,
    created_rule_id: &Option<String>,
    created_procedure_id: &Option<String>,
) -> Vec<String> {
    let mut evidence_refs = BTreeSet::new();
    evidence_refs.insert(stored.id.clone());
    evidence_refs.insert(stored.target_memory_id.clone());
    if let Some(source_id) = stored.source_id.as_deref() {
        evidence_refs.extend(
            source_id
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }
    if let Some(rule_id) = created_rule_id {
        evidence_refs.insert(rule_id.clone());
    }
    if let Some(procedure_id) = created_procedure_id {
        evidence_refs.insert(procedure_id.clone());
    }
    evidence_refs.into_iter().collect()
}

fn curate_level_transition_metadata(
    candidate_type: &str,
    previous_level: &str,
    new_level: &str,
) -> (String, String, bool) {
    match (candidate_type, previous_level, new_level) {
        ("promote", "episodic", "semantic") => (
            "clustered_repeated_observation".to_owned(),
            "repeated_observation".to_owned(),
            true,
        ),
        ("rule" | "procedure", _, "procedural") => (
            "procedural_rule_proposal".to_owned(),
            "curate.apply".to_owned(),
            true,
        ),
        _ => (
            "curation_apply".to_owned(),
            "curate.apply".to_owned(),
            false,
        ),
    }
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
    let evidence = candidate_evidence_from_source(&stored.source_type, stored.source_id.as_deref());
    let facts = CandidateEvidenceFacts::from_evidence(&evidence);
    candidate_summary_from_parts(stored, workspace_path, evidence, facts)
}

fn candidate_summary_from_database(
    connection: &DbConnection,
    stored: StoredCurationCandidate,
    workspace_path: &Path,
) -> Result<CurateCandidateSummary, DomainError> {
    let evidence = candidate_evidence_from_source(&stored.source_type, stored.source_id.as_deref());
    let facts = CandidateEvidenceFacts::from_database(connection, &evidence)?;
    Ok(candidate_summary_from_parts(
        stored,
        workspace_path,
        evidence,
        facts,
    ))
}

fn candidate_summary_from_parts(
    stored: StoredCurationCandidate,
    workspace_path: &Path,
    evidence: Vec<CurateCandidateEvidence>,
    facts: CandidateEvidenceFacts,
) -> CurateCandidateSummary {
    let review_state = normalized_review_state(&stored);
    let auto_rejected_reason = facts
        .all_member_memories_tombstoned()
        .then(|| "evidence_tombstoned".to_owned());
    let close_reason = auto_rejected_reason.clone();
    let summary_status = auto_rejected_reason
        .as_ref()
        .map_or_else(|| stored.status.clone(), |_| "auto_rejected".to_owned());
    let summary_review_state = auto_rejected_reason.as_ref().map_or(review_state, |_| {
        ReviewQueueState::Rejected.as_str().to_owned()
    });
    let requires_validate = auto_rejected_reason.is_none()
        && candidate_requires_validate(&summary_status, &summary_review_state);
    let requires_apply = auto_rejected_reason.is_none()
        && candidate_requires_apply(&summary_status, &summary_review_state);
    let next_action = if auto_rejected_reason.is_some() {
        "no action required".to_owned()
    } else {
        next_action_for_candidate_fields(
            &stored.id,
            &summary_status,
            &summary_review_state,
            stored.snoozed_until.as_deref(),
        )
    };

    let producer = ProducerMetadata::curation_candidate(
        &stored.source_type,
        stored.source_id.as_deref(),
        None,
        Some(&stored.created_at),
    );
    let proposal_source = proposal_source_for_candidate(&stored);
    let proposed_tags = proposed_tags_for_candidate(&stored, &facts.member_memory_ids);
    let priority = priority_for_candidate(
        stored.confidence,
        facts.support_count,
        facts.contradiction_count,
    );
    let candidate_id = stored.id.clone();
    let candidate_type = stored.candidate_type.clone();
    let source_type = stored.source_type.clone();
    let source_id = stored.source_id.clone();
    let created_at = stored.created_at.clone();
    let trust_class = effective_candidate_trust_class(&stored, &proposal_source);

    CurateCandidateSummary {
        candidate_id,
        id: stored.id,
        kind: kind_for_candidate_type(&candidate_type),
        candidate_type,
        target_memory_id: stored.target_memory_id,
        proposed_content: stored.proposed_content,
        proposed_level: proposed_level_for_candidate_type(&stored.candidate_type),
        proposed_kind: proposed_kind_for_candidate_type(&stored.candidate_type),
        proposed_tags,
        proposed_confidence: stored.proposed_confidence,
        proposed_trust_class: stored.proposed_trust_class,
        trust_class,
        confidence: stored.confidence,
        status: summary_status,
        review_state: summary_review_state,
        reason: stored.reason,
        source: CurateCandidateSource {
            source_type,
            source_id,
        },
        proposal_source: proposal_source.clone(),
        producer,
        evidence,
        evidence_summary: CurateCandidateEvidenceSummary {
            member_memory_ids: facts.member_memory_ids.clone(),
            support_count: facts.support_count,
            contradiction_count: facts.contradiction_count,
            cluster_coherence: facts.cluster_coherence,
        },
        member_memory_ids: facts.member_memory_ids,
        tombstoned_member_count: facts.tombstoned_member_count,
        priority,
        close_reason,
        auto_rejected_reason,
        audit: CurateCandidateAudit {
            proposed_by: proposed_by_for_candidate(&proposal_source),
            proposed_at: created_at.clone(),
        },
        validation: CurateCandidateValidation {
            status: "not_run".to_owned(),
            warnings: Vec::new(),
            next_action: "ee curate validate <CANDIDATE_ID>".to_owned(),
        },
        scope: "workspace".to_owned(),
        scope_key: workspace_path.display().to_string(),
        created_at,
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

fn candidate_evidence_from_source(
    source_type: &str,
    source_id: Option<&str>,
) -> Vec<CurateCandidateEvidence> {
    source_id.map_or_else(Vec::new, |id| {
        id.split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| CurateCandidateEvidence {
                evidence_type: source_type.to_owned(),
                id: part.to_owned(),
            })
            .collect()
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CandidateEvidenceFacts {
    member_memory_ids: Vec<String>,
    support_count: usize,
    contradiction_count: usize,
    cluster_coherence: Option<f32>,
    tombstoned_member_count: usize,
}

impl CandidateEvidenceFacts {
    fn from_evidence(evidence: &[CurateCandidateEvidence]) -> Self {
        let member_memory_ids = evidence
            .iter()
            .filter(|item| item.id.starts_with("mem_"))
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        Self {
            member_memory_ids,
            support_count: evidence.len(),
            contradiction_count: 0,
            cluster_coherence: candidate_cluster_coherence(evidence.len(), 0, None),
            tombstoned_member_count: 0,
        }
    }

    fn from_database(
        connection: &DbConnection,
        evidence: &[CurateCandidateEvidence],
    ) -> Result<Self, DomainError> {
        let mut member_memory_ids = BTreeSet::new();
        let mut contradiction_count = 0_usize;
        for item in evidence {
            if item.id.starts_with("mem_") {
                member_memory_ids.insert(item.id.clone());
            }
            if item.evidence_type == CandidateSource::FeedbackEvent.as_str()
                && let Some(event) = connection.get_feedback_event(&item.id).map_err(|error| {
                    DomainError::Storage {
                        message: format!(
                            "Failed to load curation candidate feedback evidence: {error}"
                        ),
                        repair: Some("ee learn summary --json".to_owned()),
                    }
                })?
            {
                if event.target_type == "memory" {
                    member_memory_ids.insert(event.target_id);
                }
                if feedback_signal_contradicts_candidate(&event.signal) {
                    contradiction_count = contradiction_count.saturating_add(1);
                }
            }
        }

        let member_memory_ids = member_memory_ids.into_iter().collect::<Vec<_>>();
        let mut tombstoned_member_count = 0_usize;
        let mut member_memories = Vec::new();
        for memory_id in &member_memory_ids {
            if let Some(memory) =
                connection
                    .get_memory(memory_id)
                    .map_err(|error| DomainError::Storage {
                        message: format!(
                            "Failed to load curation candidate member memory: {error}"
                        ),
                        repair: Some("ee memory show <memory-id> --json".to_owned()),
                    })?
            {
                if memory.tombstoned_at.is_some() {
                    tombstoned_member_count = tombstoned_member_count.saturating_add(1);
                }
                member_memories.push(memory);
            }
        }

        let support_count = evidence.len().saturating_sub(contradiction_count);
        let cluster_coherence =
            candidate_cluster_coherence_from_memories(connection, &member_memories)?
                .or_else(|| candidate_cluster_coherence(evidence.len(), contradiction_count, None));
        Ok(Self {
            member_memory_ids,
            support_count,
            contradiction_count,
            cluster_coherence,
            tombstoned_member_count,
        })
    }

    fn all_member_memories_tombstoned(&self) -> bool {
        !self.member_memory_ids.is_empty()
            && self.tombstoned_member_count == self.member_memory_ids.len()
    }
}

fn kind_for_candidate_type(candidate_type: &str) -> String {
    match candidate_type {
        "rule" => "procedural_rule_proposal".to_owned(),
        "procedure" => "procedure_proposal".to_owned(),
        other => format!("{other}_proposal"),
    }
}

fn proposed_level_for_candidate_type(candidate_type: &str) -> Option<String> {
    matches!(candidate_type, "rule" | "procedure").then(|| "procedural".to_owned())
}

fn proposed_kind_for_candidate_type(candidate_type: &str) -> Option<String> {
    matches!(candidate_type, "rule" | "procedure").then(|| candidate_type.to_owned())
}

fn proposal_source_for_candidate(stored: &StoredCurationCandidate) -> String {
    if stored.candidate_type == CandidateType::Rule.as_str()
        && stored.source_type == CandidateSource::FeedbackEvent.as_str()
    {
        "auto_propose_from_cluster".to_owned()
    } else if stored.source_type == CandidateSource::RuleEngine.as_str() {
        "playbook_rule_extraction".to_owned()
    } else if stored.source_type == CandidateSource::AgentInference.as_str()
        && stored
            .source_id
            .as_deref()
            .is_some_and(|id| id.contains(','))
    {
        "session_review_proposal".to_owned()
    } else {
        stored.source_type.clone()
    }
}

fn proposed_by_for_candidate(proposal_source: &str) -> String {
    match proposal_source {
        "auto_propose_from_cluster" => "auto_proposer:v1".to_owned(),
        "playbook_rule_extraction" => "rule_engine:v1".to_owned(),
        "session_review_proposal" => "review_session:v1".to_owned(),
        "human_request" => "human".to_owned(),
        other => format!("curation:{other}"),
    }
}

fn effective_candidate_trust_class(
    stored: &StoredCurationCandidate,
    proposal_source: &str,
) -> Option<String> {
    if proposal_source == "auto_propose_from_cluster" {
        Some("derived".to_owned())
    } else {
        stored.proposed_trust_class.clone()
    }
}

fn priority_for_candidate(
    confidence: f32,
    support_count: usize,
    contradiction_count: usize,
) -> String {
    if contradiction_count > 0 || confidence >= 0.85 || support_count >= 6 {
        "high".to_owned()
    } else if confidence >= 0.55 || support_count >= 2 {
        "medium".to_owned()
    } else {
        "low".to_owned()
    }
}

fn candidate_cluster_coherence(
    evidence_count: usize,
    contradiction_count: usize,
    fallback: Option<f32>,
) -> Option<f32> {
    if evidence_count == 0 {
        return fallback;
    }
    let support_count = evidence_count.saturating_sub(contradiction_count);
    let coherence = support_count as f32 / evidence_count as f32;
    Some((coherence * 1000.0).round() / 1000.0)
}

fn candidate_cluster_coherence_from_memories(
    connection: &DbConnection,
    memories: &[StoredMemory],
) -> Result<Option<f32>, DomainError> {
    if memories.len() < crate::curate::cluster_coherence::DEFAULT_MIN_CLUSTER_SIZE {
        return Ok(None);
    }
    let memory_ids = memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<&str>>();
    let memory_tags = connection
        .get_memory_tags_batch(&memory_ids)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load curation candidate memory tags: {error}"),
            repair: Some("ee memory tags <memory-id> --json".to_owned()),
        })?;
    let embedder = HashEmbedder::default_256();
    let inputs = memories
        .iter()
        .map(|memory| {
            let tags = memory_tags
                .get(&memory.id)
                .map_or(&[] as &[String], Vec::as_slice);
            ClusterCoherenceInput {
                memory_id: memory.id.clone(),
                embedding: embedder.embed_sync(&candidate_cluster_embedding_text(memory, tags)),
            }
        })
        .collect::<Vec<_>>();
    let report = silhouette_agglomerative_clusters(
        &inputs,
        crate::curate::cluster_coherence::DEFAULT_CLUSTER_COHERENCE_THRESHOLD as f32,
    );
    Ok(report
        .clusters
        .iter()
        .filter_map(|cluster| {
            cluster
                .silhouette_score
                .map(|score| (cluster.member_memory_ids.len(), score))
        })
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
        })
        .map(|(_, score)| score))
}

fn candidate_cluster_embedding_text(memory: &StoredMemory, tags: &[String]) -> String {
    format!(
        "level:{}\nkind:{}\ntags:{}\ncontent:{}",
        memory.level,
        memory.kind,
        tags.join(" "),
        memory.content
    )
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterCoherenceInput {
    pub memory_id: String,
    pub embedding: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterCoherenceCluster {
    pub cluster_id: String,
    pub member_memory_ids: Vec<String>,
    pub average_internal_similarity: Option<f32>,
    pub silhouette_score: Option<f32>,
    pub degradations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterCoherenceReport {
    pub threshold: f32,
    pub clusters: Vec<ClusterCoherenceCluster>,
    pub degradations: Vec<String>,
}

#[must_use]
pub fn silhouette_agglomerative_clusters(
    inputs: &[ClusterCoherenceInput],
    threshold: f32,
) -> ClusterCoherenceReport {
    let threshold = if threshold.is_finite() {
        threshold.clamp(0.0, 1.0)
    } else {
        crate::curate::cluster_coherence::DEFAULT_CLUSTER_COHERENCE_THRESHOLD as f32
    };
    let points = inputs
        .iter()
        .map(|input| {
            crate::curate::cluster_coherence::EmbeddingPoint::new(
                input.memory_id.clone(),
                input
                    .embedding
                    .iter()
                    .map(|value| f64::from(*value))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let config = crate::curate::cluster_coherence::ClusterCoherenceConfig {
        merge_threshold: f64::from(threshold),
        silhouette_cutoff: crate::curate::cluster_coherence::DEFAULT_CLUSTER_SILHOUETTE_CUTOFF,
        min_cluster_size: crate::curate::cluster_coherence::DEFAULT_MIN_CLUSTER_SIZE,
    };
    match crate::curate::cluster_coherence::agglomerate(&points, config) {
        Ok(report) => cluster_coherence_report_from_canonical(report),
        Err(_error) => ClusterCoherenceReport {
            threshold,
            clusters: Vec::new(),
            degradations: vec![format!(
                "degraded.{}",
                crate::curate::cluster_coherence::CLUSTERING_INSUFFICIENT_DATA_CODE
            )],
        },
    }
}

fn cluster_coherence_report_from_canonical(
    report: crate::curate::cluster_coherence::ClusterCoherenceReport,
) -> ClusterCoherenceReport {
    let cluster_count = report.clusters.len();
    let clusters = report
        .clusters
        .into_iter()
        .map(|cluster| cluster_coherence_cluster_from_canonical(cluster, cluster_count))
        .collect::<Vec<_>>();
    ClusterCoherenceReport {
        threshold: report.threshold_used as f32,
        clusters,
        degradations: report
            .degraded
            .into_iter()
            .map(|degradation| format!("degraded.{}", degradation.code))
            .collect(),
    }
}

fn cluster_coherence_cluster_from_canonical(
    cluster: crate::curate::cluster_coherence::CoherentCluster,
    cluster_count: usize,
) -> ClusterCoherenceCluster {
    let mut degradations = Vec::new();
    let silhouette_score = if cluster.member_count < 2 {
        degradations.push("degraded.clustering_silhouette_undefined_for_singleton".to_owned());
        None
    } else if cluster_count < 2 {
        degradations.push("degraded.clustering_silhouette_requires_two_clusters".to_owned());
        None
    } else {
        cluster.silhouette_score.map(|score| score as f32)
    };
    ClusterCoherenceCluster {
        cluster_id: cluster.cluster_id,
        member_memory_ids: cluster.member_memory_ids,
        average_internal_similarity: Some(cluster.average_internal_similarity as f32),
        silhouette_score,
        degradations,
    }
}

fn feedback_signal_contradicts_candidate(signal: &str) -> bool {
    matches!(
        signal,
        "negative" | "contradiction" | "harmful" | "stale" | "inaccurate" | "outdated"
    )
}

fn proposed_tags_for_candidate(
    stored: &StoredCurationCandidate,
    member_memory_ids: &[String],
) -> Vec<String> {
    let mut tags = BTreeSet::new();
    if stored.candidate_type == CandidateType::Rule.as_str() {
        tags.insert("procedural".to_owned());
        tags.insert("rule".to_owned());
    } else if stored.candidate_type == CandidateType::Procedure.as_str() {
        tags.insert("procedural".to_owned());
        tags.insert("procedure".to_owned());
    }
    if !member_memory_ids.is_empty() {
        tags.insert("cluster".to_owned());
    }
    let text = format!(
        "{} {}",
        stored.proposed_content.as_deref().unwrap_or_default(),
        stored.reason
    )
    .to_ascii_lowercase();
    for (needle, tag) in [
        ("cargo", "cargo"),
        ("release", "release"),
        ("fmt", "format"),
        ("format", "format"),
        ("clippy", "clippy"),
        ("test", "test"),
        ("build", "build"),
        ("search", "search"),
        ("curate", "curate"),
    ] {
        if text.contains(needle) {
            tags.insert(tag.to_owned());
        }
    }
    tags.into_iter().collect()
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
    let connection =
        DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    connection
        .migrate()
        .map_err(|error| DomainError::MigrationRequired {
            message: format!("Failed to migrate curation database: {error}"),
            repair: Some("ee db migrate --workspace .".to_owned()),
        })?;
    Ok(connection)
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
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use super::{
        CurateCandidatesOptions, CurateDispositionOptions, CurateReviewAction, CurateReviewOptions,
        ReviewSessionCandidate, ReviewSessionOptions, ReviewSessionReport,
        apply_curation_candidate, candidate_summary_from_stored, list_curation_candidates,
        review_curation_candidate, review_session_proposals, run_curation_disposition,
        stable_workspace_id, validate_curation_candidate,
    };
    use crate::db::{
        CreateCurationCandidateInput, CreateEvidenceSpanInput, CreateFeedbackEventInput,
        CreateMemoryInput, CreateMemoryLinkInput, CreateSessionInput, CreateWorkspaceInput,
        DbConnection, MemoryLinkRelation, MemoryLinkSource, StoredCurationCandidate, audit_actions,
    };
    use crate::models::degradation::GRAPH_CURATE_DISCONNECTED_GRAPH_CODE;
    use crate::models::{CandidateId, EvidenceId, MemoryId, SessionId};

    type TestResult = Result<(), String>;

    fn enable_structural_decay_feature(workspace_path: &Path) -> TestResult {
        let config_dir = workspace_path.join(".ee");
        fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        fs::write(
            config_dir.join("config.toml"),
            "[graph.feature.structural_decay]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())
    }

    fn test_workspace_id(workspace_path: &Path) -> String {
        let canonical = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        stable_workspace_id(&canonical)
    }

    #[test]
    fn duration_from_seconds_rejects_values_outside_chrono_range() -> TestResult {
        let error = match super::duration_from_seconds(u64::MAX, "threshold_seconds") {
            Ok(_) => return Err("out-of-range TTL should be rejected".to_owned()),
            Err(error) => error,
        };

        assert_eq!(
            error.message(),
            "Curation TTL threshold_seconds exceeds supported duration range."
        );
        Ok(())
    }

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
        assert!(summary.member_memory_ids.is_empty());
    }

    #[test]
    fn candidate_summary_splits_cluster_member_memory_ids() {
        let stored = StoredCurationCandidate {
            id: "curate_cluster0000000000000000".to_owned(),
            workspace_id: "wsp_00000000000000000000000000".to_owned(),
            candidate_type: "rule".to_owned(),
            target_memory_id: "mem_a".to_owned(),
            proposed_content: Some("Consolidate repeated cargo rules.".to_owned()),
            proposed_confidence: Some(0.82),
            proposed_trust_class: None,
            source_type: "agent_inference".to_owned(),
            source_id: Some("mem_alpha, mem_beta,mem_gamma".to_owned()),
            reason: "Remember-time proposal clustered repeated cargo rules.".to_owned(),
            confidence: 0.82,
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

        assert_eq!(
            summary.member_memory_ids,
            vec![
                "mem_alpha".to_owned(),
                "mem_beta".to_owned(),
                "mem_gamma".to_owned()
            ]
        );
        assert_eq!(summary.member_memory_ids.len(), summary.evidence.len());
    }

    #[test]
    fn candidate_summary_surfaces_g4_auto_proposal_metadata() {
        let stored = StoredCurationCandidate {
            id: "curate_cluster0000000000000001".to_owned(),
            workspace_id: "wsp_00000000000000000000000000".to_owned(),
            candidate_type: "rule".to_owned(),
            target_memory_id: "mem_alpha".to_owned(),
            proposed_content: Some(
                "Always run cargo fmt --check before cutting a release tag.".to_owned(),
            ),
            proposed_confidence: Some(0.67),
            proposed_trust_class: None,
            source_type: "feedback_event".to_owned(),
            source_id: Some("mem_alpha, mem_beta, mem_gamma".to_owned()),
            reason: "Auto-proposed from a repeated cargo release cluster.".to_owned(),
            confidence: 0.67,
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

        assert_eq!(summary.candidate_id, "curate_cluster0000000000000001");
        assert_eq!(summary.kind, "procedural_rule_proposal");
        assert_eq!(summary.proposal_source, "auto_propose_from_cluster");
        assert_eq!(summary.proposed_level.as_deref(), Some("procedural"));
        assert_eq!(summary.proposed_kind.as_deref(), Some("rule"));
        assert_eq!(summary.trust_class.as_deref(), Some("derived"));
        assert_eq!(summary.priority, "medium");
        assert_eq!(summary.audit.proposed_by, "auto_proposer:v1");
        assert_eq!(summary.evidence_summary.support_count, 3);
        assert_eq!(summary.evidence_summary.contradiction_count, 0);
        assert_eq!(summary.evidence_summary.cluster_coherence, Some(1.0));
        assert!(summary.proposed_tags.contains(&"cargo".to_owned()));
        assert!(summary.proposed_tags.contains(&"release".to_owned()));
        assert!(summary.proposed_tags.contains(&"rule".to_owned()));
    }

    #[test]
    fn review_session_proposes_two_topics_with_stable_ids() -> TestResult {
        let fixture = review_session_fixture()?;

        let first = review_session_proposals(&ReviewSessionOptions {
            workspace_path: fixture.workspace_path.as_path(),
            database_path: Some(fixture.database_path.as_path()),
            session_id: Some("cass-review-session-a"),
            propose: true,
            dry_run: true,
            min_confidence: 0.50,
            limit: 10,
        })
        .map_err(|error| error.message())?;
        let second = review_session_proposals(&ReviewSessionOptions {
            workspace_path: fixture.workspace_path.as_path(),
            database_path: Some(fixture.database_path.as_path()),
            session_id: Some("cass-review-session-a"),
            propose: true,
            dry_run: true,
            min_confidence: 0.50,
            limit: 10,
        })
        .map_err(|error| error.message())?;

        assert_eq!(first.candidate_count, 2);
        assert_eq!(first.topic_count, 2);
        assert!(!first.durable_mutation);
        assert_eq!(first.candidates, second.candidates);
        for candidate in &first.candidates {
            assert!(candidate.source_ids.len() >= 2);
            assert!(candidate.candidate_id.starts_with("curate_"));
            assert_eq!(candidate.candidate_type, "rule");
            assert!(candidate.content_hash.starts_with("blake3:"));
        }
        let topics = first
            .candidates
            .iter()
            .map(|candidate| candidate.topic_key.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(topics, BTreeSet::from(["storage", "testing"]));
        Ok(())
    }

    #[test]
    fn review_session_persists_candidates_idempotently() -> TestResult {
        let fixture = review_session_fixture()?;

        let first = review_session_proposals(&ReviewSessionOptions {
            workspace_path: fixture.workspace_path.as_path(),
            database_path: Some(fixture.database_path.as_path()),
            session_id: Some(fixture.session_id.as_str()),
            propose: true,
            dry_run: false,
            min_confidence: 0.50,
            limit: 10,
        })
        .map_err(|error| error.message())?;
        let second = review_session_proposals(&ReviewSessionOptions {
            workspace_path: fixture.workspace_path.as_path(),
            database_path: Some(fixture.database_path.as_path()),
            session_id: Some(fixture.session_id.as_str()),
            propose: true,
            dry_run: false,
            min_confidence: 0.50,
            limit: 10,
        })
        .map_err(|error| error.message())?;

        assert!(first.durable_mutation);
        assert_eq!(
            first
                .candidates
                .iter()
                .filter(|candidate| candidate.persisted)
                .count(),
            2
        );
        assert!(!second.durable_mutation);
        assert_eq!(
            second
                .candidates
                .iter()
                .filter(|candidate| candidate.persisted)
                .count(),
            0
        );

        let report = list_curation_candidates(&CurateCandidatesOptions {
            workspace_path: fixture.workspace_path.as_path(),
            database_path: Some(fixture.database_path.as_path()),
            candidate_type: Some("rule"),
            status: Some("pending"),
            target_memory_id: None,
            limit: 10,
            offset: 0,
            sort: "created",
            group_duplicates: false,
        })
        .map_err(|error| error.message())?;
        assert_eq!(report.total_count, 2);
        assert!(
            report
                .candidates
                .iter()
                .all(|candidate| candidate.evidence.len() >= 2)
        );
        Ok(())
    }

    #[test]
    fn review_session_empty_and_noisy_sessions_propose_nothing() -> TestResult {
        let fixture = review_session_fixture()?;
        let connection =
            DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
        let empty_session_id = SessionId::from_uuid(uuid::Uuid::from_u128(404)).to_string();
        let noise_session_id = SessionId::from_uuid(uuid::Uuid::from_u128(405)).to_string();
        connection
            .insert_session(
                &empty_session_id,
                &session_input(&fixture.workspace_id, "cass-empty-review"),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_session(
                &noise_session_id,
                &session_input(&fixture.workspace_id, "cass-noise-review"),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_evidence_span(
                &evidence_id(500),
                &evidence_span_input(
                    &fixture.workspace_id,
                    &noise_session_id,
                    None,
                    "noise-a",
                    1,
                    "ok yes and the but use this",
                ),
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        for session_id in ["cass-empty-review", "cass-noise-review"] {
            let report = review_session_proposals(&ReviewSessionOptions {
                workspace_path: fixture.workspace_path.as_path(),
                database_path: Some(fixture.database_path.as_path()),
                session_id: Some(session_id),
                propose: true,
                dry_run: true,
                min_confidence: 0.50,
                limit: 10,
            })
            .map_err(|error| error.message())?;
            assert_eq!(report.candidate_count, 0, "{session_id}");
            assert_eq!(report.next_action, "no session-review candidates proposed");
        }
        Ok(())
    }

    #[test]
    fn review_session_rejects_invalid_confidence_and_limit() -> TestResult {
        let fixture = review_session_fixture()?;
        let invalid_confidence = review_session_proposals(&ReviewSessionOptions {
            workspace_path: fixture.workspace_path.as_path(),
            database_path: Some(fixture.database_path.as_path()),
            session_id: Some("cass-review-session-a"),
            propose: true,
            dry_run: true,
            min_confidence: 1.1,
            limit: 10,
        });
        assert!(invalid_confidence.is_err());

        let invalid_limit = review_session_proposals(&ReviewSessionOptions {
            workspace_path: fixture.workspace_path.as_path(),
            database_path: Some(fixture.database_path.as_path()),
            session_id: Some("cass-review-session-a"),
            propose: true,
            dry_run: true,
            min_confidence: 0.5,
            limit: 0,
        });
        assert!(invalid_limit.is_err());
        Ok(())
    }

    #[test]
    fn review_session_report_json_matches_golden() -> TestResult {
        let report = ReviewSessionReport {
            schema: "ee.review.session.v1",
            command: "review session",
            version: "0.0.0",
            workspace_id: "wsp_review_golden".to_owned(),
            workspace_path: "/workspace/example".to_owned(),
            database_path: "/workspace/example/.ee/ee.db".to_owned(),
            session_id: "sess_review_golden".to_owned(),
            cass_session_id: "cass-review-golden".to_owned(),
            propose_mode: true,
            dry_run: true,
            durable_mutation: false,
            evidence_span_count: 2,
            topic_count: 1,
            candidate_count: 1,
            candidates: vec![ReviewSessionCandidate {
                candidate_id: "curate_review_golden".to_owned(),
                candidate_type: "rule".to_owned(),
                candidate_kind: "rule".to_owned(),
                topic_key: "testing".to_owned(),
                target_memory_id: "mem_review_golden".to_owned(),
                proposed_content:
                    "For `testing` work, follow the evidence-backed procedure shown in this session: Run golden tests / Keep JSON stable"
                        .to_owned(),
                proposed_confidence: 0.61,
                source_type: "agent_inference".to_owned(),
                source_ids: vec!["ev_review_a".to_owned(), "ev_review_b".to_owned()],
                reason:
                    "Session review clustered 2 evidence span(s) for topic `testing` from CASS session `cass-review-golden`."
                        .to_owned(),
                confidence: 0.61,
                content_hash: "blake3:review-golden-hash".to_owned(),
                persisted: false,
            }],
            degraded: Vec::new(),
            next_action: "ee review session <session-id> --propose --json".to_owned(),
        };

        let actual = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
        let expected =
            include_str!("../../tests/fixtures/golden/review/session_propose.golden").trim_end();
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn list_curation_candidates_filters_pending_and_paginates() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
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
                    workflow_id: None,
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
                    source_id: Some("fb_01234567890123456789012345".to_owned()),
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
        assert!(report.candidates[0].member_memory_ids.is_empty());
        assert!(!report.durable_mutation);
        assert_eq!(report.filter.status.as_deref(), Some("pending"));
        Ok(())
    }

    #[test]
    fn list_curation_candidates_resolves_feedback_cluster_members_and_tombstones() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let memory_one = MemoryId::from_uuid(uuid::Uuid::from_u128(0x7001)).to_string();
        let memory_two = MemoryId::from_uuid(uuid::Uuid::from_u128(0x7002)).to_string();
        let candidate_id = curate_id(0x7003);
        let feedback_one = feedback_id(1);
        let feedback_two = feedback_id(2);

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("curate-g4-cluster".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        for (memory_id, content) in [
            (&memory_one, "Run cargo fmt --check before release."),
            (
                &memory_two,
                "Keep cargo release tags behind fmt verification.",
            ),
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
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
        }
        for (feedback_id, target_id) in [(&feedback_one, &memory_one), (&feedback_two, &memory_two)]
        {
            connection
                .insert_feedback_event(
                    feedback_id,
                    &CreateFeedbackEventInput {
                        workspace_id: workspace_id.clone(),
                        target_type: "memory".to_owned(),
                        target_id: target_id.clone(),
                        signal: "helpful".to_owned(),
                        weight: 1.0,
                        source_type: "agent_inference".to_owned(),
                        source_id: Some("cluster-fixture".to_owned()),
                        reason: Some("Cluster member supports the proposal.".to_owned()),
                        evidence_json: None,
                        session_id: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        connection
            .insert_curation_candidate(
                &candidate_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "rule".to_owned(),
                    target_memory_id: memory_one.clone(),
                    proposed_content: Some(
                        "Always run cargo fmt --check before cutting a release tag.".to_owned(),
                    ),
                    proposed_confidence: Some(0.67),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "feedback_event".to_owned(),
                    source_id: Some(format!("{feedback_one},{feedback_two}")),
                    reason: "Learning cluster proposed a cargo release rule.".to_owned(),
                    confidence: 0.67,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:02Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .tombstone_memory(&memory_one)
            .map_err(|error| error.to_string())?;
        connection
            .tombstone_memory(&memory_two)
            .map_err(|error| error.to_string())?;

        let report = list_curation_candidates(&CurateCandidatesOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_type: Some("rule"),
            status: Some("pending"),
            target_memory_id: None,
            limit: 10,
            offset: 0,
            sort: "review_state",
            group_duplicates: false,
        })
        .map_err(|error| error.message())?;

        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .ok_or_else(|| "G4 candidate missing from queue".to_owned())?;
        assert_eq!(candidate.proposal_source, "auto_propose_from_cluster");
        assert_eq!(
            candidate.member_memory_ids,
            vec![memory_one.clone(), memory_two.clone()]
        );
        assert_eq!(
            candidate.evidence_summary.member_memory_ids,
            candidate.member_memory_ids
        );
        assert_eq!(candidate.evidence_summary.support_count, 2);
        assert_eq!(candidate.tombstoned_member_count, 2);
        assert_eq!(candidate.status, "auto_rejected");
        assert_eq!(candidate.review_state, "rejected");
        assert_eq!(
            candidate.close_reason.as_deref(),
            Some("evidence_tombstoned")
        );
        assert_eq!(
            candidate.auto_rejected_reason.as_deref(),
            Some("evidence_tombstoned")
        );
        assert!(!candidate.requires_validate);
        assert!(!candidate.requires_apply);
        assert_eq!(candidate.next_action, "no action required");
        Ok(())
    }

    #[test]
    fn list_curation_candidates_scores_cluster_coherence_from_member_memories() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let memory_ids = [
            MemoryId::from_uuid(uuid::Uuid::from_u128(0x7101)).to_string(),
            MemoryId::from_uuid(uuid::Uuid::from_u128(0x7102)).to_string(),
            MemoryId::from_uuid(uuid::Uuid::from_u128(0x7103)).to_string(),
            MemoryId::from_uuid(uuid::Uuid::from_u128(0x7104)).to_string(),
        ];
        let candidate_id = curate_id(0x7105);
        let feedback_id = feedback_id(0x7106);

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("curate-g5-coherence".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        for (memory_id, content, tags) in [
            (
                &memory_ids[0],
                "cargo release format verification cargo release format",
                vec!["cargo".to_owned(), "release".to_owned()],
            ),
            (
                &memory_ids[1],
                "cargo release format verification cargo release format",
                vec!["cargo".to_owned(), "release".to_owned()],
            ),
            (
                &memory_ids[2],
                "sqlmodel frankensqlite storage migration sqlmodel storage",
                vec!["sqlmodel".to_owned(), "storage".to_owned()],
            ),
            (
                &memory_ids[3],
                "sqlmodel frankensqlite storage migration sqlmodel storage",
                vec!["sqlmodel".to_owned(), "storage".to_owned()],
            ),
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.7,
                        utility: 0.6,
                        importance: 0.5,
                        provenance_uri: None,
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags,
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        connection
            .insert_feedback_event(
                &feedback_id,
                &CreateFeedbackEventInput {
                    workspace_id: workspace_id.clone(),
                    target_type: "memory".to_owned(),
                    target_id: memory_ids[0].clone(),
                    signal: "stale".to_owned(),
                    weight: 1.0,
                    source_type: "agent_inference".to_owned(),
                    source_id: Some("cluster-coherence-fixture".to_owned()),
                    reason: Some(
                        "Contradictory evidence should affect only fallback scoring.".to_owned(),
                    ),
                    evidence_json: None,
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_curation_candidate(
                &candidate_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "rule".to_owned(),
                    target_memory_id: memory_ids[0].clone(),
                    proposed_content: Some(
                        "Separate repeated cargo and storage rules before promotion.".to_owned(),
                    ),
                    proposed_confidence: Some(0.67),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "feedback_event".to_owned(),
                    source_id: Some(format!(
                        "{},{},{},{},{}",
                        memory_ids[0], memory_ids[1], memory_ids[2], memory_ids[3], feedback_id
                    )),
                    reason: "Learning cluster proposed a mixed evidence candidate.".to_owned(),
                    confidence: 0.67,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:02Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = list_curation_candidates(&CurateCandidatesOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_type: Some("rule"),
            status: Some("pending"),
            target_memory_id: None,
            limit: 10,
            offset: 0,
            sort: "review_state",
            group_duplicates: false,
        })
        .map_err(|error| error.message())?;

        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .ok_or_else(|| "G5 coherence candidate missing from queue".to_owned())?;
        let coherence = candidate
            .evidence_summary
            .cluster_coherence
            .ok_or_else(|| "candidate should surface a cluster coherence score".to_owned())?;
        assert!(
            (-1.0..=1.0).contains(&coherence),
            "cluster coherence must be a silhouette score, got {coherence}"
        );
        assert!(
            (coherence - 0.8).abs() > f32::EPSILON,
            "database-backed candidate should not fall back to support ratio coherence"
        );
        assert_eq!(candidate.evidence_summary.contradiction_count, 1);
        Ok(())
    }

    #[test]
    fn list_curation_candidates_supports_sorting_and_duplicate_grouping() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
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
                    workflow_id: None,
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
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
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
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
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
        let workspace_id = test_workspace_id(workspace_path);
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
        let workspace_id = test_workspace_id(workspace_path);
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
    fn apply_curation_candidate_blocks_spoofed_trust_evidence() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(30)).to_string();
        let seed_id = curate_id(31);
        let spoof_id = curate_id(32);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &seed_id,
            "promote",
            Some("approved"),
            None,
        )?;
        connection
            .insert_curation_candidate(
                &spoof_id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_owned(),
                    target_memory_id: memory_id.clone(),
                    proposed_content: None,
                    proposed_confidence: Some(0.95),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "human_request".to_owned(),
                    source_id: Some("reviewer".to_owned()),
                    reason: "Spoofed reviewer string must not promote trust.".to_owned(),
                    confidence: 0.91,
                    status: Some("approved".to_owned()),
                    created_at: Some("2026-05-01T00:00:06Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = apply_curation_candidate(&super::CurateApplyOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &spoof_id,
            actor: Some("MistySalmon"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.application.status, "blocked");
        assert!(
            report
                .application
                .errors
                .iter()
                .any(|issue| issue.code == "trust_promotion_evidence_rejected")
        );
        assert!(!report.mutation.persisted);

        let memory = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after blocked spoof apply".to_owned())?;
        assert!((memory.confidence - 0.7).abs() < 0.001);
        assert_eq!(memory.trust_class, "human_explicit");
        Ok(())
    }

    #[test]
    fn apply_curation_candidate_redacts_secret_like_content_before_memory_persist() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(31)).to_string();
        let candidate_id = curate_id(32);
        let raw_value = concat!("ghp", "_", "curate", "_", "apply");
        let proposed_content =
            format!("Run `cargo test` before editing src/core/curate.rs with token: {raw_value}.");
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "consolidate",
            Some("approved"),
            Some(&proposed_content),
        )?;

        let report = apply_curation_candidate(&super::CurateApplyOptions {
            workspace_path,
            database_path: Some(&database_path),
            candidate_id: &candidate_id,
            actor: Some("MistySalmon"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.application.status, "applied");
        assert!(
            report
                .application
                .warnings
                .iter()
                .any(|issue| issue.code == "proposed_content_redacted")
        );
        let content_change = report
            .application
            .changes
            .iter()
            .find(|change| change.field == "content")
            .ok_or_else(|| "content change missing".to_owned())?;
        let after = content_change
            .after
            .as_ref()
            .ok_or_else(|| "content change after missing".to_owned())?;
        assert!(after.contains("[REDACTED:"));
        assert!(!after.contains(raw_value));

        let memory = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after redacted apply".to_owned())?;
        assert!(memory.content.contains("[REDACTED:"));
        assert!(!memory.content.contains(raw_value));
        Ok(())
    }

    #[test]
    fn apply_curation_candidate_dry_run_leaves_memory_and_candidate_unchanged() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
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
        let workspace_id = test_workspace_id(workspace_path);
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
        let workspace_id = test_workspace_id(workspace_path);
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
        let workspace_id = test_workspace_id(workspace_path);
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

    #[test]
    fn run_curate_untombstone_restores_tombstoned_memory_and_audits() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
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
        connection
            .tombstone_memory(&memory_id)
            .map_err(|error| error.to_string())?;
        let previous_tombstoned_at = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .and_then(|memory| memory.tombstoned_at)
            .ok_or_else(|| "memory should be tombstoned before restore".to_owned())?;

        let report = super::run_curate_untombstone(&super::CurateUntombstoneOptions {
            workspace_path,
            database_path: Some(&database_path),
            memory_id: &memory_id,
            actor: Some("MistySalmon"),
            dry_run: false,
            reason: Some("restore reversible decay tombstone"),
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, super::CURATE_UNTOMBSTONE_SCHEMA_V1);
        assert_eq!(report.memory_id, memory_id);
        assert_eq!(
            report.previous_tombstoned_at.as_deref(),
            Some(previous_tombstoned_at.as_str())
        );
        assert_eq!(report.restored_by.as_deref(), Some("MistySalmon"));
        assert!(report.persisted);
        let audit_id = report
            .audit_id
            .as_ref()
            .ok_or_else(|| "restore should return an audit id".to_owned())?;

        let restored = connection
            .get_memory(&report.memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after restore".to_owned())?;
        assert!(restored.tombstoned_at.is_none());
        assert_eq!(restored.updated_at, report.restored_at);

        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "audit entry missing".to_owned())?;
        assert_eq!(audit.action, audit_actions::MEMORY_UNTOMBSTONE);
        assert_eq!(audit.target_id.as_deref(), Some(report.memory_id.as_str()));
        assert_eq!(audit.actor.as_deref(), Some("MistySalmon"));
        assert!(
            audit
                .details
                .as_ref()
                .is_some_and(|details| details.contains("restore reversible decay tombstone"))
        );
        Ok(())
    }

    #[test]
    fn curation_disposition_structural_decay_protects_bridge_candidate() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let bridge_id = MemoryId::from_uuid(uuid::Uuid::from_u128(41)).to_string();
        let core_b_id = MemoryId::from_uuid(uuid::Uuid::from_u128(42)).to_string();
        let core_c_id = MemoryId::from_uuid(uuid::Uuid::from_u128(43)).to_string();
        let leaf_id = MemoryId::from_uuid(uuid::Uuid::from_u128(44)).to_string();
        let bridge_candidate_id = curate_id(45);
        let leaf_candidate_id = curate_id(50);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &bridge_id,
            &bridge_candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;
        insert_test_memory(&connection, &workspace_id, &core_b_id, "Core B")?;
        insert_test_memory(&connection, &workspace_id, &core_c_id, "Core C")?;
        insert_test_memory(&connection, &workspace_id, &leaf_id, "Leaf")?;
        insert_test_candidate(
            &connection,
            TestCandidateInput {
                workspace_id: &workspace_id,
                memory_id: &leaf_id,
                candidate_id: &leaf_candidate_id,
                source_id: "fb_11234567890123456789012345",
                candidate_type: "promote",
                status: Some("pending"),
                proposed_content: None,
            },
        )?;
        insert_test_link(
            &connection,
            "link_00000000000000000000000041",
            &bridge_id,
            &core_b_id,
        )?;
        insert_test_link(
            &connection,
            "link_00000000000000000000000042",
            &core_b_id,
            &core_c_id,
        )?;
        insert_test_link(
            &connection,
            "link_00000000000000000000000043",
            &bridge_id,
            &core_c_id,
        )?;
        insert_test_link(
            &connection,
            "link_00000000000000000000000044",
            &bridge_id,
            &leaf_id,
        )?;
        enable_structural_decay_feature(workspace_path)?;

        let legacy = run_curation_disposition(&CurateDispositionOptions {
            workspace_path,
            database_path: Some(&database_path),
            actor: Some("MistySalmon"),
            apply: false,
            structural_decay: false,
            now_rfc3339: Some("2026-05-20T00:00:02Z"),
        })
        .map_err(|error| error.message())?;
        let structural = run_curation_disposition(&CurateDispositionOptions {
            workspace_path,
            database_path: Some(&database_path),
            actor: Some("MistySalmon"),
            apply: false,
            structural_decay: true,
            now_rfc3339: Some("2026-05-20T00:00:02Z"),
        })
        .map_err(|error| error.message())?;

        assert!(legacy.structural_adjustments.is_empty());
        let legacy_bridge_decision = legacy
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == bridge_candidate_id)
            .ok_or_else(|| "legacy bridge decision missing".to_owned())?;
        let legacy_leaf_decision = legacy
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == leaf_candidate_id)
            .ok_or_else(|| "legacy leaf decision missing".to_owned())?;
        let structural_bridge_decision = structural
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == bridge_candidate_id)
            .ok_or_else(|| "structural bridge decision missing".to_owned())?;
        let structural_leaf_decision = structural
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == leaf_candidate_id)
            .ok_or_else(|| "structural leaf decision missing".to_owned())?;
        assert_eq!(legacy_bridge_decision.decision, "planned");
        assert_eq!(legacy_leaf_decision.decision, "planned");
        assert_eq!(structural_bridge_decision.decision, "not_due");
        assert_eq!(structural_leaf_decision.decision, "planned");

        let bridge_adjustment = structural
            .structural_adjustments
            .iter()
            .find(|adjustment| adjustment.memory_id == bridge_id)
            .ok_or_else(|| "bridge adjustment missing".to_owned())?;
        assert!(bridge_adjustment.is_articulation_point);
        assert!(bridge_adjustment.structural_multiplier < 1.0);
        assert!(
            bridge_adjustment.adjusted_ttl_threshold_seconds
                > legacy_bridge_decision.ttl_threshold_seconds
        );
        assert!(bridge_adjustment.adjusted_decay < bridge_adjustment.base_decay);

        let leaf_adjustment = structural
            .structural_adjustments
            .iter()
            .find(|adjustment| adjustment.memory_id == leaf_id)
            .ok_or_else(|| "leaf adjustment missing".to_owned())?;
        assert!(!leaf_adjustment.is_articulation_point);
        assert!(leaf_adjustment.structural_multiplier > 1.0);
        assert!(
            leaf_adjustment.adjusted_ttl_threshold_seconds
                < legacy_leaf_decision.ttl_threshold_seconds
        );

        let snapshot = serde_json::json!({
            "structuralAdjustments": structural.structural_adjustments,
        });
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path("../../tests/snapshots");
        settings.set_prepend_module_to_snapshot(false);
        settings.bind(|| {
            insta::assert_json_snapshot!("curation_structural_adjustments_block", snapshot);
        });
        Ok(())
    }

    #[test]
    fn curation_disposition_enabled_feature_emits_structural_adjustments() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(46)).to_string();
        let candidate_id = curate_id(47);
        seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;
        enable_structural_decay_feature(workspace_path)?;

        let report = run_curation_disposition(&CurateDispositionOptions {
            workspace_path,
            database_path: Some(&database_path),
            actor: None,
            apply: false,
            structural_decay: true,
            now_rfc3339: Some("2026-05-02T00:00:02Z"),
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.structural_adjustments.len(), 1);
        assert_eq!(report.structural_adjustments[0].memory_id, memory_id);
        assert_eq!(report.structural_adjustments[0].structural_multiplier, 1.0);
        Ok(())
    }

    #[test]
    fn curation_disposition_disabled_feature_suppresses_structural_adjustments() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(54)).to_string();
        let candidate_id = curate_id(55);
        seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;

        let report = run_curation_disposition(&CurateDispositionOptions {
            workspace_path,
            database_path: Some(&database_path),
            actor: None,
            apply: false,
            structural_decay: true,
            now_rfc3339: Some("2026-05-02T00:00:02Z"),
        })
        .map_err(|error| error.message())?;
        let data = report.data_json();

        assert!(report.structural_adjustments.is_empty());
        assert!(!data.contains("structuralAdjustments"));
        let degraded = report
            .degraded
            .iter()
            .find(|entry| entry.code == "graph_feature_disabled")
            .ok_or_else(|| "expected graph_feature_disabled degradation".to_owned())?;
        assert_eq!(degraded.severity, "medium");
        assert!(
            degraded
                .repair
                .contains("graph.feature.structural_decay.enabled")
        );
        Ok(())
    }

    #[test]
    fn curation_disposition_structural_decay_reports_disconnected_graph() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let first_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(50)).to_string();
        let second_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(51)).to_string();
        let first_candidate_id = curate_id(52);
        let second_candidate_id = curate_id(53);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &first_memory_id,
            &first_candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;
        insert_test_memory(
            &connection,
            &workspace_id,
            &second_memory_id,
            "Review isolated memories before structural decay.",
        )?;
        insert_test_candidate(
            &connection,
            TestCandidateInput {
                workspace_id: &workspace_id,
                memory_id: &second_memory_id,
                candidate_id: &second_candidate_id,
                source_id: "fb_22222222222222222222222222",
                candidate_type: "promote",
                status: Some("pending"),
                proposed_content: None,
            },
        )?;
        enable_structural_decay_feature(workspace_path)?;

        let report = run_curation_disposition(&CurateDispositionOptions {
            workspace_path,
            database_path: Some(&database_path),
            actor: None,
            apply: false,
            structural_decay: true,
            now_rfc3339: Some("2026-05-02T00:00:02Z"),
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.structural_adjustments.len(), 2);
        let degraded = report
            .degraded
            .iter()
            .find(|entry| entry.code == GRAPH_CURATE_DISCONNECTED_GRAPH_CODE)
            .ok_or_else(|| "expected disconnected-graph degradation".to_owned())?;
        assert_eq!(degraded.severity, "warning");
        assert!(
            degraded.message.contains("connected components"),
            "degradation should explain disconnected components: {}",
            degraded.message
        );
        Ok(())
    }

    #[test]
    fn curation_disposition_structural_decay_ignores_denied_mesh_links() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let first_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x5201)).to_string();
        let second_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x5202)).to_string();
        let first_candidate_id = curate_id(0x5203);
        let second_candidate_id = curate_id(0x5204);
        let connection = seed_candidate_database(
            &database_path,
            &workspace_id,
            &first_memory_id,
            &first_candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;
        insert_test_memory(
            &connection,
            &workspace_id,
            &second_memory_id,
            "Review isolated mesh-derived evidence separately.",
        )?;
        insert_test_candidate(
            &connection,
            TestCandidateInput {
                workspace_id: &workspace_id,
                memory_id: &second_memory_id,
                candidate_id: &second_candidate_id,
                source_id: "fb_52045204520452045204520452",
                candidate_type: "promote",
                status: Some("pending"),
                proposed_content: None,
            },
        )?;
        insert_test_link_with_metadata(
            &connection,
            "link_00000000000000000000005201",
            &first_memory_id,
            &second_memory_id,
            Some(denied_mesh_link_metadata()),
        )?;
        enable_structural_decay_feature(workspace_path)?;

        let report = run_curation_disposition(&CurateDispositionOptions {
            workspace_path,
            database_path: Some(&database_path),
            actor: None,
            apply: false,
            structural_decay: true,
            now_rfc3339: Some("2026-05-02T00:00:02Z"),
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.structural_adjustments.len(), 2);
        let degraded = report
            .degraded
            .iter()
            .find(|entry| entry.code == GRAPH_CURATE_DISCONNECTED_GRAPH_CODE)
            .ok_or_else(|| "denied mesh link must not connect curation graph".to_owned())?;
        assert_eq!(degraded.severity, "warning");
        Ok(())
    }

    #[test]
    fn curation_disposition_no_structural_decay_keeps_legacy_report_shape() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(workspace_path);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(48)).to_string();
        let candidate_id = curate_id(49);
        seed_candidate_database(
            &database_path,
            &workspace_id,
            &memory_id,
            &candidate_id,
            "promote",
            Some("pending"),
            None,
        )?;

        let report = run_curation_disposition(&CurateDispositionOptions {
            workspace_path,
            database_path: Some(&database_path),
            actor: None,
            apply: false,
            structural_decay: false,
            now_rfc3339: Some("2026-05-02T00:00:02Z"),
        })
        .map_err(|error| error.message())?;
        let data = report.data_json();

        assert!(report.structural_adjustments.is_empty());
        assert!(!data.contains("structuralAdjustments"));
        Ok(())
    }

    fn insert_test_memory(
        connection: &DbConnection,
        workspace_id: &str,
        memory_id: &str,
        content: &str,
    ) -> Result<(), String> {
        connection
            .insert_memory(
                memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
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
            .map_err(|error| error.to_string())
    }

    fn insert_test_link(
        connection: &DbConnection,
        link_id: &str,
        src_memory_id: &str,
        dst_memory_id: &str,
    ) -> Result<(), String> {
        insert_test_link_with_metadata(connection, link_id, src_memory_id, dst_memory_id, None)
    }

    fn insert_test_link_with_metadata(
        connection: &DbConnection,
        link_id: &str,
        src_memory_id: &str,
        dst_memory_id: &str,
        metadata_json: Option<String>,
    ) -> Result<(), String> {
        connection
            .insert_memory_link(
                link_id,
                &CreateMemoryLinkInput {
                    src_memory_id: src_memory_id.to_owned(),
                    dst_memory_id: dst_memory_id.to_owned(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("curate-structural-test".to_owned()),
                    metadata_json,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn denied_mesh_link_metadata() -> String {
        serde_json::json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_link_denied_5201",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_decision_denied_5201",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }

    struct TestCandidateInput<'a> {
        workspace_id: &'a str,
        memory_id: &'a str,
        candidate_id: &'a str,
        source_id: &'a str,
        candidate_type: &'a str,
        status: Option<&'a str>,
        proposed_content: Option<&'a str>,
    }

    fn insert_test_candidate(
        connection: &DbConnection,
        input: TestCandidateInput<'_>,
    ) -> Result<(), String> {
        connection
            .insert_curation_candidate(
                input.candidate_id,
                &CreateCurationCandidateInput {
                    workspace_id: input.workspace_id.to_owned(),
                    candidate_type: input.candidate_type.to_owned(),
                    target_memory_id: input.memory_id.to_owned(),
                    proposed_content: input.proposed_content.map(str::to_owned),
                    proposed_confidence: Some(0.82),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: "feedback_event".to_owned(),
                    source_id: Some(input.source_id.to_owned()),
                    reason: "Useful during release verification.".to_owned(),
                    confidence: 0.76,
                    status: input.status.map(str::to_owned),
                    created_at: Some("2026-05-01T00:00:02Z".to_owned()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())
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
                    workflow_id: None,
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
        insert_test_candidate(
            &connection,
            TestCandidateInput {
                workspace_id,
                memory_id,
                candidate_id,
                source_id: "fb_01234567890123456789012345",
                candidate_type,
                status,
                proposed_content,
            },
        )?;
        Ok(connection)
    }

    struct ReviewFixture {
        _tempdir: tempfile::TempDir,
        workspace_path: std::path::PathBuf,
        database_path: std::path::PathBuf,
        workspace_id: String,
        session_id: String,
    }

    fn review_session_fixture() -> Result<ReviewFixture, String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path().to_path_buf();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = test_workspace_id(&workspace_path);
        let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(303)).to_string();
        let storage_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(101)).to_string();
        let testing_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(202)).to_string();

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("review-session-test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        insert_review_memory(
            &connection,
            &workspace_id,
            &storage_memory_id,
            "Use SQLModel with FrankenSQLite for durable curation storage.",
        )?;
        insert_review_memory(
            &connection,
            &workspace_id,
            &testing_memory_id,
            "Golden tests must cover review session proposal output.",
        )?;
        connection
            .insert_session(
                &session_id,
                &session_input(&workspace_id, "cass-review-session-a"),
            )
            .map_err(|error| error.to_string())?;

        let storage_excerpts = [
            "Storage review decided SQLModel and FrankenSQLite remain the source of truth.",
            "Database migration evidence says curation candidates must persist in SQLite.",
            "FrankenSQLite storage spans preserve provenance for review proposals.",
            "SQLModel storage rows need deterministic curation candidate identifiers.",
            "The storage layer must retain CASS evidence links for later validation.",
        ];
        let testing_excerpts = [
            "Golden tests should cover review session proposal JSON output.",
            "The test fixture needs two topics and deterministic candidate IDs.",
            "E2E tests verify review proposals route into the curation queue.",
            "Malformed review input should return a usage error in tests.",
            "Empty review sessions must produce no curation candidates.",
        ];
        for (index, excerpt) in storage_excerpts.iter().enumerate() {
            connection
                .insert_evidence_span(
                    &evidence_id(u128::try_from(index + 1).map_err(|error| error.to_string())?),
                    &evidence_span_input(
                        &workspace_id,
                        &session_id,
                        Some(&storage_memory_id),
                        &format!("storage-{index}"),
                        u32::try_from(index + 1).map_err(|error| error.to_string())?,
                        excerpt,
                    ),
                )
                .map_err(|error| error.to_string())?;
        }
        for (index, excerpt) in testing_excerpts.iter().enumerate() {
            connection
                .insert_evidence_span(
                    &evidence_id(u128::try_from(index + 20).map_err(|error| error.to_string())?),
                    &evidence_span_input(
                        &workspace_id,
                        &session_id,
                        Some(&testing_memory_id),
                        &format!("testing-{index}"),
                        u32::try_from(index + 20).map_err(|error| error.to_string())?,
                        excerpt,
                    ),
                )
                .map_err(|error| error.to_string())?;
        }
        connection.close().map_err(|error| error.to_string())?;

        Ok(ReviewFixture {
            _tempdir: tempdir,
            workspace_path,
            database_path,
            workspace_id,
            session_id,
        })
    }

    fn insert_review_memory(
        connection: &DbConnection,
        workspace_id: &str,
        memory_id: &str,
        content: &str,
    ) -> Result<(), String> {
        connection
            .insert_memory(
                memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "episodic".to_owned(),
                    kind: "cass_import".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 0.55,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("cass-session://cass-review-session-a#L1-L2".to_owned()),
                    trust_class: "cass_evidence".to_owned(),
                    trust_subclass: Some("session-span".to_owned()),
                    tags: vec!["cass".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn session_input(workspace_id: &str, cass_session_id: &str) -> CreateSessionInput {
        CreateSessionInput {
            workspace_id: workspace_id.to_owned(),
            cass_session_id: cass_session_id.to_owned(),
            source_path: Some("/tmp/cass/session.jsonl".to_owned()),
            agent_name: Some("codex".to_owned()),
            model: Some("gpt-5".to_owned()),
            started_at: Some("2026-05-06T00:00:00Z".to_owned()),
            ended_at: Some("2026-05-06T00:10:00Z".to_owned()),
            message_count: 10,
            token_count: Some(1000),
            content_hash: format!(
                "blake3:{}",
                blake3::hash(cass_session_id.as_bytes()).to_hex()
            ),
            metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.to_owned()),
        }
    }

    fn evidence_span_input(
        workspace_id: &str,
        session_id: &str,
        memory_id: Option<&str>,
        cass_span_id: &str,
        start_line: u32,
        excerpt: &str,
    ) -> CreateEvidenceSpanInput {
        CreateEvidenceSpanInput {
            workspace_id: workspace_id.to_owned(),
            session_id: session_id.to_owned(),
            memory_id: memory_id.map(str::to_owned),
            cass_span_id: cass_span_id.to_owned(),
            span_kind: "message".to_owned(),
            start_line,
            end_line: start_line + 1,
            start_byte: Some(start_line.saturating_mul(100)),
            end_byte: Some(start_line.saturating_mul(100).saturating_add(80)),
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
            metadata_json: Some(r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.to_owned()),
        }
    }

    fn evidence_id(seed: u128) -> String {
        EvidenceId::from_uuid(uuid::Uuid::from_u128(seed)).to_string()
    }

    fn curate_id(seed: u128) -> String {
        let candidate = CandidateId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        format!("curate_{}", candidate.trim_start_matches("cand_"))
    }

    fn feedback_id(seed: u128) -> String {
        format!("fb_{seed:026}")
    }
}
