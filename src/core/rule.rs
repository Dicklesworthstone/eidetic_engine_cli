//! Procedural rule creation service (EE-086).
//!
//! `ee rule add` writes to the dedicated procedural rule tables added in
//! EE-084. It keeps direct rule management separate from generic memory
//! capture while preserving the same workspace, audit, dry-run, and index
//! queue conventions used by `ee remember`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::curate::{CandidateSource, CandidateType, specificity_score};
use crate::db::{
    CreateAuditInput, CreateCurationCandidateInput, CreateProceduralRuleInput,
    CreateSearchIndexJobInput, CreateWorkspaceInput, DbConnection, SearchIndexJobType,
    StoredMemory, StoredProceduralRule, UpdateProceduralRuleInput,
    UpdateProceduralRuleLifecycleInput, audit_actions, generate_audit_id,
};
use crate::models::{
    CandidateId, DomainError, MemoryContent, MemoryId, RuleId, RuleLifecycleEvidence,
    RuleLifecycleTransition, RuleLifecycleTrigger, RuleMaturity, RuleScope, Tag, TrustClass,
    UnitScore, WorkspaceId,
};

/// Stable schema for `ee rule add` response data.
pub const RULE_ADD_SCHEMA_V1: &str = "ee.rule.add.v1";
/// Stable schema for `ee rule list` response data.
pub const RULE_LIST_SCHEMA_V1: &str = "ee.rule.list.v1";
/// Stable schema for `ee rule show` response data.
pub const RULE_SHOW_SCHEMA_V1: &str = "ee.rule.show.v1";
/// Stable schema for `ee rule mark` response data.
pub const RULE_MARK_SCHEMA_V1: &str = "ee.rule.mark.v1";
/// Stable schema for `ee rule protect` response data.
pub const RULE_PROTECT_SCHEMA_V1: &str = "ee.rule.protect.v1";
/// Stable schema for `ee rule update` response data.
pub const RULE_UPDATE_SCHEMA_V1: &str = "ee.rule.update.v1";
/// Stable schema for `ee playbook extract` response data.
pub const PLAYBOOK_EXTRACT_SCHEMA_V1: &str = "ee.playbook.extract.v1";
/// Stable schema for `ee playbook list` response data.
pub const PLAYBOOK_LIST_SCHEMA_V1: &str = "ee.playbook.list.v1";
/// Stable schema for `ee playbook export` response data.
pub const PLAYBOOK_EXPORT_SCHEMA_V1: &str = "ee.playbook.export.v1";
/// Stable schema for `ee playbook import` response data.
pub const PLAYBOOK_IMPORT_SCHEMA_V1: &str = "ee.playbook.import.v1";
/// Stable portable playbook document schema.
pub const PLAYBOOK_PORTABLE_SCHEMA_V1: &str = "ee.playbook.portable.v1";

const MAX_RULE_CONTENT_BYTES: usize = 8192;
const MAX_RULE_LIST_LIMIT: u32 = 1000;
const MAX_PLAYBOOK_EXTRACT_LIMIT: u32 = 1000;
const PLAYBOOK_MIN_EVIDENCE: usize = 3;

/// Options for creating a procedural rule through `ee rule add`.
#[derive(Clone, Debug)]
pub struct RuleAddOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Rule body.
    pub content: &'a str,
    /// Rule scope.
    pub scope: &'a str,
    /// Optional scope pattern for directory/file-pattern rules.
    pub scope_pattern: Option<&'a str>,
    /// Initial maturity.
    pub maturity: &'a str,
    /// Optional explicit confidence score.
    pub confidence: Option<f32>,
    /// Initial utility score.
    pub utility: f32,
    /// Initial importance score.
    pub importance: f32,
    /// Trust class.
    pub trust_class: &'a str,
    /// Protect the rule against automatic harmful-feedback inversion.
    pub protected: bool,
    /// Tags, allowing repeated flags and comma-separated values.
    pub tags: &'a [String],
    /// Source memory IDs used as explicit evidence.
    pub source_memory_ids: &'a [String],
    /// Validate and render the write without mutating storage.
    pub dry_run: bool,
    /// Optional audit actor.
    pub actor: Option<&'a str>,
}

/// Options for listing procedural rules through `ee rule list`.
#[derive(Clone, Debug)]
pub struct RuleListOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Optional maturity filter.
    pub maturity: Option<&'a str>,
    /// Optional scope filter.
    pub scope: Option<&'a str>,
    /// Optional tag filter.
    pub tag: Option<&'a str>,
    /// Include tombstoned rules.
    pub include_tombstoned: bool,
    /// Maximum number of rules to return.
    pub limit: u32,
    /// Number of filtered rules to skip.
    pub offset: u32,
}

/// Options for showing one procedural rule through `ee rule show`.
#[derive(Clone, Debug)]
pub struct RuleShowOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Rule ID to retrieve.
    pub rule_id: &'a str,
    /// Include tombstoned rules.
    pub include_tombstoned: bool,
}

/// Lifecycle evidence for `ee rule mark`.
#[derive(Clone, Debug, Default)]
pub struct RuleMarkEvidenceOptions<'a> {
    pub helpful_outcomes: u32,
    pub harmful_outcomes: u32,
    pub distinct_harmful_sources: u32,
    pub manual_curation_approved: bool,
    pub intervening_helpful_from_harmful_sources: bool,
    pub validation_passes: u32,
    pub validation_contradictions: u32,
    pub review_approved: bool,
    pub superseding_rule_id: Option<&'a str>,
}

/// Options for recording lifecycle evidence with `ee rule mark`.
#[derive(Clone, Debug)]
pub struct RuleMarkOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Rule ID to mark.
    pub rule_id: &'a str,
    /// Lifecycle trigger.
    pub trigger: &'a str,
    /// Evidence counters and flags used by the lifecycle evaluator.
    pub evidence: RuleMarkEvidenceOptions<'a>,
    /// Validate and render the lifecycle plan without mutating storage.
    pub dry_run: bool,
    /// Optional audit actor.
    pub actor: Option<&'a str>,
}

/// Options for toggling a procedural rule's protected marker.
#[derive(Clone, Debug)]
pub struct RuleProtectOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Rule ID to protect or unprotect.
    pub rule_id: &'a str,
    /// Desired protected state. `false` implements `--unprotect`.
    pub protected: bool,
    /// Validate and render the write without mutating storage.
    pub dry_run: bool,
    /// Optional audit actor.
    pub actor: Option<&'a str>,
}

/// Options for updating mutable procedural rule metadata.
#[derive(Clone, Debug)]
pub struct RuleUpdateOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Rule ID to update.
    pub rule_id: &'a str,
    /// Replacement content, if supplied.
    pub content: Option<&'a str>,
    /// Replacement scope, if supplied.
    pub scope: Option<&'a str>,
    /// Replacement scope pattern, if supplied.
    pub scope_pattern: Option<&'a str>,
    /// Clear the existing scope pattern.
    pub clear_scope_pattern: bool,
    /// Replacement trust class, if supplied.
    pub trust_class: Option<&'a str>,
    /// Replacement confidence, if supplied.
    pub confidence: Option<f32>,
    /// Replacement utility, if supplied.
    pub utility: Option<f32>,
    /// Replacement importance, if supplied.
    pub importance: Option<f32>,
    /// Desired protected state, if supplied.
    pub protected: Option<bool>,
    /// Replacement tag set, if supplied.
    pub tags: Option<&'a [String]>,
    /// Clear the tag set.
    pub clear_tags: bool,
    /// Replacement source-memory evidence set, if supplied.
    pub source_memory_ids: Option<&'a [String]>,
    /// Clear the source-memory evidence set.
    pub clear_source_memory_ids: bool,
    /// Validate and render the update without mutating storage.
    pub dry_run: bool,
    /// Optional audit actor.
    pub actor: Option<&'a str>,
}

/// Options for extracting rule candidates from repeated semantic memories.
#[derive(Clone, Debug)]
pub struct PlaybookExtractOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Optional lower bound for memory created_at timestamps.
    pub since: Option<&'a str>,
    /// Maximum number of semantic memories to scan.
    pub limit: u32,
    /// Preview candidate creation without mutating storage.
    pub dry_run: bool,
    /// Optional audit actor.
    pub actor: Option<&'a str>,
}

/// Options for listing portable playbook rules through `ee playbook list`.
#[derive(Clone, Debug)]
pub struct PlaybookListOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Include tombstoned rules.
    pub include_tombstoned: bool,
    /// Maximum number of rules to return.
    pub limit: u32,
    /// Number of filtered rules to skip.
    pub offset: u32,
}

/// Options for exporting portable playbook rules through `ee playbook export`.
#[derive(Clone, Debug)]
pub struct PlaybookExportOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Destination playbook JSON file.
    pub output_path: &'a Path,
    /// Include tombstoned rules.
    pub include_tombstoned: bool,
    /// Maximum number of rules to export.
    pub limit: u32,
    /// Preview the export without writing the side-path artifact.
    pub dry_run: bool,
}

/// Options for importing portable playbook rules through `ee playbook import`.
#[derive(Clone, Debug)]
pub struct PlaybookImportOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Source playbook JSON file.
    pub source_path: &'a Path,
    /// Preview the import without writing rules, audit rows, or index jobs.
    pub dry_run: bool,
    /// Optional audit actor.
    pub actor: Option<&'a str>,
}

/// Result of creating or previewing a procedural rule.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub rule_id: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub content: String,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub maturity: String,
    pub lifecycle: RuleAddLifecycle,
    pub trust_class: String,
    pub protected: bool,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source_memory_ids: Vec<String>,
    pub evidence: RuleAddEvidence,
    pub dry_run: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
    pub index_status: String,
    pub redaction_status: String,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleAddReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule add","status":"serialization_failed"}}"#,
                RULE_ADD_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        if self.dry_run {
            format!(
                "DRY RUN: Would add procedural rule ({})\n  Content: {}\n  Evidence: {}\n",
                self.maturity, self.content, self.evidence.status
            )
        } else {
            format!(
                "Added procedural rule: {}\n  ID: {}\n  Audit: {}\n  Index job: {}\n",
                self.content,
                self.rule_id,
                self.audit_id.as_deref().unwrap_or("none"),
                self.index_job_id.as_deref().unwrap_or("none")
            )
        }
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "RULE_ADD|status={}|id={}|maturity={}|evidence={}|persisted={}",
            self.status, self.rule_id, self.maturity, self.evidence.status, self.persisted
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddLifecycle {
    pub initial_maturity: String,
    pub is_active: bool,
    pub is_terminal: bool,
    pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddEvidence {
    pub status: String,
    pub source_memory_count: usize,
    pub verified: bool,
    pub requirement: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAddDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

/// Result of listing procedural rules.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleListReport {
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
    pub filter: RuleListFilter,
    pub rules: Vec<RuleSummary>,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleListReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule list","status":"serialization_failed"}}"#,
                RULE_LIST_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Procedural rules ({} total", self.total_count);
        if self.truncated {
            output.push_str(", showing batch");
        }
        output.push_str(")\n\n");
        if self.rules.is_empty() {
            output.push_str("  No procedural rules found.\n");
            return output;
        }
        for rule in &self.rules {
            output.push_str(&format!(
                "  {} [{}] confidence={:.2}\n",
                rule.id, rule.maturity, rule.confidence
            ));
            output.push_str(&format!("    {}\n", rule.content));
            output.push_str(&format!(
                "    scope={}, tags={}, evidence={}\n\n",
                rule.scope,
                rule.tags.len(),
                rule.evidence.source_memory_count
            ));
        }
        output.push_str("Next:\n  ee rule show <RULE_ID>\n");
        output
    }
}

/// Result of showing one procedural rule.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleShowReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub found: bool,
    pub rule: RuleDetails,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleShowReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule show","status":"serialization_failed"}}"#,
                RULE_SHOW_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let rule = &self.rule;
        let mut output = format!("Procedural rule: {}\n\n", rule.id);
        output.push_str(&format!("  Maturity: {}\n", rule.maturity));
        output.push_str(&format!("  Scope: {}", rule.scope));
        if let Some(pattern) = &rule.scope_pattern {
            output.push_str(&format!(" ({pattern})"));
        }
        output.push('\n');
        output.push_str(&format!("  Content:\n    {}\n", rule.content));
        output.push_str(&format!(
            "  Scores: confidence={:.2}, utility={:.2}, importance={:.2}\n",
            rule.confidence, rule.utility, rule.importance
        ));
        output.push_str(&format!("  Trust: {}\n", rule.trust_class));
        output.push_str(&format!("  Protected: {}\n", rule.protected));
        output.push_str(&format!(
            "  Feedback: +{} / -{}\n",
            rule.positive_feedback_count, rule.negative_feedback_count
        ));
        if !rule.tags.is_empty() {
            output.push_str(&format!("  Tags: {}\n", rule.tags.join(", ")));
        }
        if !rule.source_memory_ids.is_empty() {
            output.push_str(&format!(
                "  Source memories: {}\n",
                rule.source_memory_ids.join(", ")
            ));
        }
        output.push_str(&format!("  Created: {}\n", rule.created_at));
        output.push_str(&format!("  Updated: {}\n", rule.updated_at));
        if let Some(ts) = &rule.tombstoned_at {
            output.push_str(&format!("  Tombstoned: {ts}\n"));
        }
        output
    }
}

/// Result of recording lifecycle evidence for one procedural rule.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMarkReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub rule_id: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub dry_run: bool,
    pub persisted: bool,
    pub changed: bool,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
    pub index_status: String,
    pub transition: RuleMarkTransition,
    pub evidence: RuleMarkEvidenceReport,
    pub previous_rule: RuleDetails,
    pub rule: RuleDetails,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleMarkReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule mark","status":"serialization_failed"}}"#,
                RULE_MARK_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let prefix = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{prefix}rule mark {status}\n  ID: {id}\n  Trigger: {trigger}\n  Maturity: {from} -> {to}\n  Changed: {changed}\n  Audit: {audit}\n",
            status = self.status,
            id = self.rule_id,
            trigger = self.transition.trigger,
            from = self.transition.prior_maturity,
            to = self.transition.next_maturity,
            changed = self.changed,
            audit = self.audit_id.as_deref().unwrap_or("none"),
        )
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "RULE_MARK|status={}|id={}|trigger={}|action={}|from={}|to={}|changed={}",
            self.status,
            self.rule_id,
            self.transition.trigger,
            self.transition.action,
            self.transition.prior_maturity,
            self.transition.next_maturity,
            self.changed
        )
    }
}

/// Stable lifecycle transition data emitted by `ee rule mark`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMarkTransition {
    pub trigger: String,
    pub action: String,
    pub prior_maturity: String,
    pub next_maturity: String,
    pub allowed: bool,
    pub requires_curation: bool,
    pub audit_required: bool,
    pub confidence_delta: f64,
    pub utility_delta: f64,
    pub reason: String,
}

/// Stable lifecycle evidence data emitted by `ee rule mark`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMarkEvidenceReport {
    pub helpful_outcomes: u32,
    pub harmful_outcomes: u32,
    pub distinct_harmful_sources: u32,
    pub protected_rule: bool,
    pub manual_curation_approved: bool,
    pub intervening_helpful_from_harmful_sources: bool,
    pub validation_passes: u32,
    pub validation_contradictions: u32,
    pub review_approved: bool,
    pub superseding_rule_id: Option<String>,
}

/// Result of protecting or unprotecting one procedural rule.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleProtectReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub rule_id: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub protected: bool,
    pub previous_protected: bool,
    pub changed: bool,
    pub dry_run: bool,
    pub audit_id: Option<String>,
    pub degraded: Vec<RuleAddDegradation>,
}

/// Result of updating one procedural rule.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleUpdateReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub rule_id: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub dry_run: bool,
    pub persisted: bool,
    pub changed: bool,
    pub changed_fields: Vec<String>,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
    pub index_status: String,
    pub previous_rule: RuleDetails,
    pub rule: RuleDetails,
    pub degraded: Vec<RuleAddDegradation>,
}

impl RuleUpdateReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule update","status":"serialization_failed"}}"#,
                RULE_UPDATE_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let prefix = if self.dry_run { "DRY RUN: " } else { "" };
        let fields = if self.changed_fields.is_empty() {
            "none".to_owned()
        } else {
            self.changed_fields.join(", ")
        };
        format!(
            "{prefix}rule update {status}\n  ID: {id}\n  Changed: {changed}\n  Fields: {fields}\n  Audit: {audit}\n",
            status = self.status,
            id = self.rule_id,
            changed = self.changed,
            audit = self.audit_id.as_deref().unwrap_or("none"),
        )
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "RULE_UPDATE|status={}|id={}|changed={}|fields={}",
            self.status,
            self.rule_id,
            self.changed,
            self.changed_fields.join(",")
        )
    }
}

/// Result of extracting playbook rule candidates.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookExtractReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub since: Option<String>,
    pub scanned_memory_count: usize,
    pub candidate_count: usize,
    pub persisted_count: usize,
    pub duplicate_count: usize,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub candidates: Vec<PlaybookRuleCandidate>,
    pub degraded: Vec<RuleAddDegradation>,
    pub next_action: String,
}

impl PlaybookExtractReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"playbook extract","status":"serialization_failed"}}"#,
                PLAYBOOK_EXTRACT_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run { "DRY RUN" } else { "EXTRACTED" };
        let mut output = format!(
            "{mode}: playbook rule candidates ({} created, {} duplicates, {} memories scanned)\n",
            self.persisted_count, self.duplicate_count, self.scanned_memory_count
        );
        for candidate in &self.candidates {
            output.push_str(&format!(
                "  {} confidence={:.2} evidence={}\n",
                candidate.candidate_id.as_deref().unwrap_or("<dry-run>"),
                candidate.confidence,
                candidate.source_memory_ids.len()
            ));
            output.push_str(&format!("    {}\n", candidate.proposed_content));
        }
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookRuleCandidate {
    pub candidate_id: Option<String>,
    pub candidate_type: String,
    pub target_memory_id: String,
    pub proposed_content: String,
    pub command_pattern: String,
    pub specificity_score: f32,
    pub confidence: f32,
    pub reason: String,
    pub source_memory_ids: Vec<String>,
    pub persisted: bool,
    pub duplicate: bool,
    pub audit_id: Option<String>,
}

/// Portable rule row used by playbook list/export/import.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookPortableRule {
    pub source_rule_id: Option<String>,
    pub content: String,
    pub maturity: String,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub trust_class: String,
    pub protected: bool,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source_memory_ids: Vec<String>,
    pub source_memory_count: usize,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Portable playbook artifact written by `ee playbook export`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookPortableDocument {
    pub schema: String,
    pub exported_at: String,
    pub ee_version: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub rule_count: usize,
    pub rules: Vec<PlaybookPortableRule>,
}

/// Result of listing playbook-portable rules.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookListReport {
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
    pub rules: Vec<PlaybookPortableRule>,
    pub degraded: Vec<RuleAddDegradation>,
}

impl PlaybookListReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"playbook list","status":"serialization_failed"}}"#,
                PLAYBOOK_LIST_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Playbook rules ({} total)\n", self.total_count);
        for rule in &self.rules {
            output.push_str(&format!(
                "  {} [{}] tags={}\n",
                rule.source_rule_id.as_deref().unwrap_or("<portable>"),
                rule.maturity,
                rule.tags.len()
            ));
            output.push_str(&format!("    {}\n", truncate_rule_content(&rule.content).0));
        }
        output
    }
}

/// Result of exporting playbook-portable rules.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookExportReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub dry_run: bool,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub output_path: String,
    pub artifact_hash: String,
    pub total_count: usize,
    pub exported_count: usize,
    pub truncated: bool,
    pub no_overwrite: bool,
    pub redaction_status: String,
    pub document: PlaybookPortableDocument,
    pub degraded: Vec<RuleAddDegradation>,
}

impl PlaybookExportReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"playbook export","status":"serialization_failed"}}"#,
                PLAYBOOK_EXPORT_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let prefix = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{prefix}playbook export {status}: {count} rules\n  path: {path}\n  hash: {hash}\n",
            status = self.status,
            count = self.exported_count,
            path = self.output_path,
            hash = self.artifact_hash,
        )
    }
}

/// Per-rule import decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookImportDecision {
    pub source_rule_id: Option<String>,
    pub content_hash: String,
    pub status: String,
    pub imported_rule_id: Option<String>,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
    pub issue_codes: Vec<String>,
}

/// Result of importing playbook-portable rules.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookImportReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub dry_run: bool,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub source_path: String,
    pub source_hash: String,
    pub source_schema: String,
    pub source_rule_count: usize,
    pub imported_count: usize,
    pub duplicate_count: usize,
    pub skipped_count: usize,
    pub downgraded_count: usize,
    pub durable_mutation: bool,
    pub decisions: Vec<PlaybookImportDecision>,
    pub degraded: Vec<RuleAddDegradation>,
}

impl PlaybookImportReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"playbook import","status":"serialization_failed"}}"#,
                PLAYBOOK_IMPORT_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let prefix = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{prefix}playbook import {}: imported={}, duplicates={}, skipped={}\n  source: {}\n",
            self.status,
            self.imported_count,
            self.duplicate_count,
            self.skipped_count,
            self.source_path
        )
    }
}

impl RuleProtectReport {
    /// Serialize response data without the outer response envelope.
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"rule protect","status":"serialization_failed"}}"#,
                RULE_PROTECT_SCHEMA_V1
            )
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let verb = if self.protected {
            "Protected"
        } else {
            "Unprotected"
        };
        if self.dry_run {
            format!(
                "DRY RUN: Would set procedural rule protection\n  ID: {}\n  Protected: {}\n",
                self.rule_id, self.protected
            )
        } else {
            format!(
                "{verb} procedural rule\n  ID: {}\n  Changed: {}\n  Audit: {}\n",
                self.rule_id,
                self.changed,
                self.audit_id.as_deref().unwrap_or("none")
            )
        }
    }

    /// Compact TOON-like summary.
    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "RULE_PROTECT|status={}|id={}|protected={}|changed={}",
            self.status, self.rule_id, self.protected, self.changed
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleListFilter {
    pub maturity: Option<String>,
    pub scope: Option<String>,
    pub tag: Option<String>,
    pub include_tombstoned: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSummary {
    pub id: String,
    /// Rule body text. May be truncated for list views — when truncated,
    /// `content_truncated` is `true` and the value ends with "...".
    pub content: String,
    /// True if `content` was truncated for the list view.
    pub content_truncated: bool,
    pub maturity: String,
    pub lifecycle: RuleLifecycle,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub trust_class: String,
    pub protected: bool,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub evidence: RuleEvidence,
    pub tags: Vec<String>,
    pub is_tombstoned: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDetails {
    pub id: String,
    pub workspace_id: String,
    pub content: String,
    pub confidence: f32,
    pub utility: f32,
    pub importance: f32,
    pub trust_class: String,
    pub scope: String,
    pub scope_pattern: Option<String>,
    pub maturity: String,
    pub protected: bool,
    pub lifecycle: RuleLifecycle,
    pub positive_feedback_count: u32,
    pub negative_feedback_count: u32,
    pub last_applied_at: Option<String>,
    pub last_validated_at: Option<String>,
    pub superseded_by: Option<String>,
    pub source_memory_ids: Vec<String>,
    pub tags: Vec<String>,
    pub evidence: RuleEvidence,
    pub created_at: String,
    pub updated_at: String,
    pub tombstoned_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleLifecycle {
    pub maturity: String,
    pub is_active: bool,
    pub is_terminal: bool,
    pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleEvidence {
    pub status: String,
    pub source_memory_count: usize,
    pub verified: bool,
    pub requirement: String,
}

#[derive(Clone, Debug)]
struct PreparedRuleAdd {
    rule_id: RuleId,
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
    content: String,
    scope: RuleScope,
    scope_pattern: Option<String>,
    maturity: RuleMaturity,
    trust_class: TrustClass,
    confidence: f32,
    utility: f32,
    importance: f32,
    tags: Vec<String>,
    source_memory_ids: Vec<String>,
    protected: bool,
    actor: Option<String>,
}

#[derive(Clone, Debug)]
struct PreparedRuleRead {
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
}

#[derive(Clone, Debug)]
struct PreparedRuleUpdate {
    input: UpdateProceduralRuleInput,
    changed_fields: Vec<String>,
    next_detail: RuleDetails,
}

/// Add a procedural rule or preview the write.
pub fn add_rule(options: &RuleAddOptions<'_>) -> Result<RuleAddReport, DomainError> {
    let prepared = prepare_rule_add(options)?;
    if options.dry_run {
        return Ok(rule_add_report(
            &prepared, "dry_run", false, None, None, false,
        ));
    }

    ensure_database_parent_exists(&prepared.database_path)?;
    let connection =
        DbConnection::open_file(&prepared.database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;
    ensure_workspace(
        &connection,
        &prepared.workspace_id,
        &prepared.workspace_path,
    )?;
    verify_source_memories(
        &connection,
        &prepared.workspace_id,
        &prepared.source_memory_ids,
    )?;

    let rule_id = prepared.rule_id.to_string();
    let audit_id = generate_audit_id();
    let index_job_id = generate_search_index_job_id();
    let input = CreateProceduralRuleInput {
        workspace_id: prepared.workspace_id.clone(),
        content: prepared.content.clone(),
        confidence: prepared.confidence,
        utility: prepared.utility,
        importance: prepared.importance,
        trust_class: prepared.trust_class.as_str().to_owned(),
        scope: prepared.scope.as_str().to_owned(),
        scope_pattern: prepared.scope_pattern.clone(),
        maturity: prepared.maturity.as_str().to_owned(),
        protected: prepared.protected,
        source_memory_ids: prepared.source_memory_ids.clone(),
        tags: prepared.tags.clone(),
    };
    let audit_details = rule_add_audit_details(&rule_id, &input);
    let index_input = CreateSearchIndexJobInput {
        workspace_id: prepared.workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("rule".to_owned()),
        document_id: Some(rule_id.clone()),
        documents_total: 1,
    };

    connection
        .with_transaction(|| {
            connection.insert_procedural_rule(&rule_id, &input)?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(input.workspace_id.clone()),
                    actor: prepared
                        .actor
                        .clone()
                        .or_else(|| Some("ee rule add".to_owned())),
                    action: audit_actions::RULE_CREATE.to_owned(),
                    target_type: Some("rule".to_owned()),
                    target_id: Some(rule_id.clone()),
                    details: Some(audit_details.clone()),
                },
            )?;
            connection.insert_search_index_job(&index_job_id, &index_input)
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to store procedural rule: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    Ok(rule_add_report(
        &prepared,
        "stored",
        true,
        Some(audit_id),
        Some(index_job_id),
        true,
    ))
}

/// List procedural rules for the selected workspace.
pub fn list_rules(options: &RuleListOptions<'_>) -> Result<RuleListReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee rule list --help"),
    )?;
    let maturity = parse_optional_maturity(options.maturity)?;
    let scope = parse_optional_scope(options.scope)?;
    let tag = parse_optional_tag(options.tag)?;
    validate_list_window(options.limit)?;

    let connection = open_existing_database(&prepared.database_path)?;
    let stored = connection
        .list_procedural_rules(
            &prepared.workspace_id,
            maturity.as_deref(),
            scope.as_deref(),
            options.include_tombstoned,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list procedural rules: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;

    let mut details = Vec::with_capacity(stored.len());
    for rule in stored {
        let detail = load_rule_details(&connection, rule)?;
        if tag
            .as_ref()
            .is_none_or(|required| detail.tags.iter().any(|value| value == required))
        {
            details.push(detail);
        }
    }

    let total_count = details.len();
    let offset = usize::try_from(options.offset).map_err(|_| {
        rule_read_usage_error(
            "rule list offset is too large".to_owned(),
            "ee rule list --help",
        )
    })?;
    let limit = usize::try_from(options.limit).map_err(|_| {
        rule_read_usage_error(
            "rule list limit is too large".to_owned(),
            "ee rule list --help",
        )
    })?;
    let rules = details
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(rule_summary_from_details)
        .collect::<Vec<_>>();
    let returned_count = rules.len();
    let truncated = offset.saturating_add(returned_count) < total_count;

    Ok(RuleListReport {
        schema: RULE_LIST_SCHEMA_V1,
        command: "rule list",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        total_count,
        returned_count,
        limit: options.limit,
        offset: options.offset,
        truncated,
        filter: RuleListFilter {
            maturity,
            scope,
            tag,
            include_tombstoned: options.include_tombstoned,
        },
        rules,
        degraded: Vec::new(),
    })
}

/// Show one procedural rule in the selected workspace.
pub fn show_rule(options: &RuleShowOptions<'_>) -> Result<RuleShowReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee rule show <RULE_ID> --json"),
    )?;
    let rule_id = RuleId::from_str(options.rule_id)
        .map_err(|error| {
            rule_read_usage_error(format!("invalid rule ID: {error}"), "ee rule show --help")
        })?
        .to_string();
    let connection = open_existing_database(&prepared.database_path)?;
    let Some(rule) =
        connection
            .get_procedural_rule(&rule_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query procedural rule: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?
    else {
        return Err(rule_not_found(&rule_id));
    };
    if rule.workspace_id != prepared.workspace_id {
        return Err(rule_not_found(&rule_id));
    }
    if rule.tombstoned_at.is_some() && !options.include_tombstoned {
        return Err(rule_not_found(&rule_id));
    }
    let detail = load_rule_details(&connection, rule)?;

    Ok(RuleShowReport {
        schema: RULE_SHOW_SCHEMA_V1,
        command: "rule show",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        found: true,
        rule: detail,
        degraded: Vec::new(),
    })
}

/// Protect or unprotect one procedural rule.
pub fn protect_rule(options: &RuleProtectOptions<'_>) -> Result<RuleProtectReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee rule protect <RULE_ID> --json"),
    )?;
    let rule_id = RuleId::from_str(options.rule_id)
        .map_err(|error| {
            rule_read_usage_error(
                format!("invalid rule ID: {error}"),
                "ee rule protect --help",
            )
        })?
        .to_string();
    let connection = open_existing_database(&prepared.database_path)?;
    let Some(rule) =
        connection
            .get_procedural_rule(&rule_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query procedural rule: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?
    else {
        return Err(rule_not_found(&rule_id));
    };
    if rule.workspace_id != prepared.workspace_id || rule.tombstoned_at.is_some() {
        return Err(rule_not_found(&rule_id));
    }

    let previous_protected = rule.protected;
    let changed = previous_protected != options.protected;
    if options.dry_run {
        return Ok(rule_protect_report(
            &prepared,
            &rule_id,
            options.protected,
            previous_protected,
            changed,
            true,
            None,
        ));
    }

    let audit_id = generate_audit_id();
    let details = rule_protect_audit_details(&rule_id, previous_protected, options.protected);
    connection
        .with_transaction(|| {
            if changed {
                connection.update_procedural_rule_protected(
                    &rule_id,
                    &prepared.workspace_id,
                    options.protected,
                )?;
            }
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(prepared.workspace_id.clone()),
                    actor: options
                        .actor
                        .map(str::to_owned)
                        .or_else(|| Some("ee rule protect".to_owned())),
                    action: audit_actions::RULE_PROTECT.to_owned(),
                    target_type: Some("rule".to_owned()),
                    target_id: Some(rule_id.clone()),
                    details: Some(details.clone()),
                },
            )
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to update procedural rule protection: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;

    Ok(rule_protect_report(
        &prepared,
        &rule_id,
        options.protected,
        previous_protected,
        changed,
        false,
        Some(audit_id),
    ))
}

/// Record lifecycle evidence for one procedural rule.
pub fn mark_rule(options: &RuleMarkOptions<'_>) -> Result<RuleMarkReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee rule mark <RULE_ID> --trigger <TRIGGER> --json"),
    )?;
    let rule_id = parse_rule_id_for_command(options.rule_id, "ee rule mark --help")?;
    let trigger = RuleLifecycleTrigger::from_str(options.trigger)
        .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule mark --help"))?;
    let connection = open_existing_database(&prepared.database_path)?;
    let stored = load_active_rule(&connection, &prepared.workspace_id, &rule_id)?;
    let previous_detail = load_rule_details(&connection, stored.clone())?;
    let maturity = RuleMaturity::from_str(&stored.maturity).map_err(|error| {
        DomainError::Storage {
            message: format!("Stored procedural rule has invalid maturity: {error}"),
            repair: Some("ee rule update <RULE_ID> --maturity is intentionally unavailable; repair the database or recreate the rule".to_owned()),
        }
    })?;
    let evidence =
        build_rule_lifecycle_evidence(&connection, &prepared.workspace_id, &stored, options)?;
    let transition = maturity.evaluate_lifecycle_transition(trigger, &evidence);
    if !transition.allowed {
        return Err(DomainError::PolicyDenied {
            message: format!("Rule lifecycle transition rejected: {}", transition.reason),
            repair: Some(
                "Use `ee rule show <RULE_ID> --json` and supply the required lifecycle evidence."
                    .to_owned(),
            ),
        });
    }

    let marked_at = Utc::now().to_rfc3339();
    let next_confidence = apply_score_delta(stored.confidence, transition.confidence_delta);
    let next_utility = apply_score_delta(stored.utility, transition.utility_delta);
    let next_maturity = transition.next_maturity.as_str().to_owned();
    let next_superseded_by = evidence
        .superseding_rule_id
        .clone()
        .or_else(|| stored.superseded_by.clone());
    let positive_feedback_delta = positive_feedback_delta(trigger, &evidence);
    let negative_feedback_delta = negative_feedback_delta(trigger, &evidence);
    let last_validated_at = last_validated_marker(trigger, &marked_at);
    let changed = stored.maturity != next_maturity
        || score_changed(stored.confidence, next_confidence)
        || score_changed(stored.utility, next_utility)
        || stored.superseded_by != next_superseded_by
        || positive_feedback_delta > 0
        || negative_feedback_delta > 0
        || last_validated_at.is_some();

    let dry_rule = apply_mark_to_detail(
        previous_detail.clone(),
        ApplyMarkToDetail {
            transition: &transition,
            confidence: next_confidence,
            utility: next_utility,
            superseded_by: next_superseded_by.clone(),
            positive_feedback_delta,
            negative_feedback_delta,
            last_validated_at: last_validated_at.clone(),
            updated_at: &marked_at,
        },
    );
    if options.dry_run {
        return Ok(rule_mark_report(RuleMarkReportInput {
            prepared: &prepared,
            rule_id: &rule_id,
            dry_run: true,
            persisted: false,
            changed,
            audit_id: None,
            index_job_id: None,
            transition: &transition,
            evidence: &evidence,
            previous_rule: previous_detail,
            rule: dry_rule,
        }));
    }

    let audit_id = generate_audit_id();
    let index_job_id = if changed {
        Some(generate_search_index_job_id())
    } else {
        None
    };
    let lifecycle_input = UpdateProceduralRuleLifecycleInput {
        workspace_id: prepared.workspace_id.clone(),
        maturity: next_maturity,
        confidence: next_confidence,
        utility: next_utility,
        positive_feedback_delta,
        negative_feedback_delta,
        last_validated_at: last_validated_at.clone(),
        superseded_by: next_superseded_by,
        updated_at: marked_at,
    };
    let audit_details =
        rule_mark_audit_details(&rule_id, &transition, &evidence, changed, &previous_detail);

    connection
        .with_transaction(|| {
            if changed {
                connection.update_procedural_rule_lifecycle(&rule_id, &lifecycle_input)?;
            }
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(prepared.workspace_id.clone()),
                    actor: options
                        .actor
                        .map(str::to_owned)
                        .or_else(|| Some("ee rule mark".to_owned())),
                    action: audit_actions::RULE_MARK.to_owned(),
                    target_type: Some("rule".to_owned()),
                    target_id: Some(rule_id.clone()),
                    details: Some(audit_details.clone()),
                },
            )?;
            if let Some(index_job_id) = &index_job_id {
                connection.insert_search_index_job(
                    index_job_id,
                    &CreateSearchIndexJobInput {
                        workspace_id: prepared.workspace_id.clone(),
                        job_type: SearchIndexJobType::SingleDocument,
                        document_source: Some("rule".to_owned()),
                        document_id: Some(rule_id.clone()),
                        documents_total: 1,
                    },
                )?;
            }
            Ok(())
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to mark procedural rule: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;

    let updated = load_active_rule(&connection, &prepared.workspace_id, &rule_id)?;
    let updated_detail = load_rule_details(&connection, updated)?;
    Ok(rule_mark_report(RuleMarkReportInput {
        prepared: &prepared,
        rule_id: &rule_id,
        dry_run: false,
        persisted: true,
        changed,
        audit_id: Some(audit_id),
        index_job_id,
        transition: &transition,
        evidence: &evidence,
        previous_rule: previous_detail,
        rule: updated_detail,
    }))
}

/// Update mutable metadata for one procedural rule.
pub fn update_rule(options: &RuleUpdateOptions<'_>) -> Result<RuleUpdateReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee rule update <RULE_ID> --json"),
    )?;
    validate_rule_update_request(options)?;
    let rule_id = parse_rule_id_for_command(options.rule_id, "ee rule update --help")?;
    let connection = open_existing_database(&prepared.database_path)?;
    let stored = load_active_rule(&connection, &prepared.workspace_id, &rule_id)?;
    let previous_detail = load_rule_details(&connection, stored)?;
    let prepared_update = prepare_rule_update(&connection, &prepared, &previous_detail, options)?;
    let changed = !prepared_update.changed_fields.is_empty();

    if options.dry_run {
        return Ok(rule_update_report(RuleUpdateReportInput {
            prepared: &prepared,
            rule_id: &rule_id,
            dry_run: true,
            persisted: false,
            changed,
            changed_fields: prepared_update.changed_fields,
            audit_id: None,
            index_job_id: None,
            previous_rule: previous_detail,
            rule: prepared_update.next_detail,
        }));
    }

    if !changed {
        return Ok(rule_update_report(RuleUpdateReportInput {
            prepared: &prepared,
            rule_id: &rule_id,
            dry_run: false,
            persisted: false,
            changed: false,
            changed_fields: Vec::new(),
            audit_id: None,
            index_job_id: None,
            previous_rule: previous_detail.clone(),
            rule: previous_detail,
        }));
    }

    let audit_id = generate_audit_id();
    let index_job_id = generate_search_index_job_id();
    let audit_details = rule_update_audit_details(
        &rule_id,
        &prepared_update.changed_fields,
        &previous_detail,
        &prepared_update.next_detail,
    );
    connection
        .with_transaction(|| {
            connection.update_procedural_rule_metadata(&rule_id, &prepared_update.input)?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(prepared.workspace_id.clone()),
                    actor: options
                        .actor
                        .map(str::to_owned)
                        .or_else(|| Some("ee rule update".to_owned())),
                    action: audit_actions::RULE_UPDATE.to_owned(),
                    target_type: Some("rule".to_owned()),
                    target_id: Some(rule_id.clone()),
                    details: Some(audit_details.clone()),
                },
            )?;
            connection.insert_search_index_job(
                &index_job_id,
                &CreateSearchIndexJobInput {
                    workspace_id: prepared.workspace_id.clone(),
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("rule".to_owned()),
                    document_id: Some(rule_id.clone()),
                    documents_total: 1,
                },
            )
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to update procedural rule: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;

    let updated = load_active_rule(&connection, &prepared.workspace_id, &rule_id)?;
    let updated_detail = load_rule_details(&connection, updated)?;
    Ok(rule_update_report(RuleUpdateReportInput {
        prepared: &prepared,
        rule_id: &rule_id,
        dry_run: false,
        persisted: true,
        changed: true,
        changed_fields: prepared_update.changed_fields,
        audit_id: Some(audit_id),
        index_job_id: Some(index_job_id),
        previous_rule: previous_detail,
        rule: updated_detail,
    }))
}

/// Extract procedural-rule curation candidates from repeated semantic memories.
pub fn extract_playbook_candidates(
    options: &PlaybookExtractOptions<'_>,
) -> Result<PlaybookExtractReport, DomainError> {
    validate_playbook_limit(options.limit)?;
    let since = parse_playbook_since(options.since)?;
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee playbook extract --workspace . --json"),
    )?;

    let connection = open_existing_database(&prepared.database_path)?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("ee doctor".to_owned()),
    })?;
    ensure_workspace(
        &connection,
        &prepared.workspace_id,
        &prepared.workspace_path,
    )?;

    let mut memories = connection
        .list_memories(&prepared.workspace_id, Some("semantic"), false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to scan semantic memories: {error}"),
            repair: Some("ee memory list --level semantic --json".to_owned()),
        })?;
    memories.retain(|memory| memory_is_after_since(memory, since.as_ref()));
    memories.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let limit = usize::try_from(options.limit).map_err(|_| {
        rule_read_usage_error(
            "playbook extract --limit is too large".to_owned(),
            "ee playbook extract --help",
        )
    })?;
    memories.truncate(limit);

    let scanned_memory_count = memories.len();
    let groups = group_playbook_memories(&connection, memories)?;
    let existing_contents = existing_rule_candidate_contents(&connection, &prepared.workspace_id)?;

    let mut candidates = Vec::new();
    let mut persisted_count = 0_usize;
    let mut duplicate_count = 0_usize;
    let actor = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ee playbook extract");

    for group in groups
        .into_values()
        .filter(|group| group.memories.len() >= PLAYBOOK_MIN_EVIDENCE)
    {
        let source_memory_ids = group
            .memories
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<Vec<_>>();
        let target_memory_id = source_memory_ids.first().cloned().ok_or_else(|| {
            rule_read_usage_error(
                "playbook group had no evidence".to_owned(),
                "ee playbook extract --help",
            )
        })?;
        let proposed_content = proposed_playbook_rule(&group);
        let normalized = normalize_rule_text(&proposed_content);
        let duplicate = existing_contents.contains(&normalized);
        let specificity = specificity_score(&proposed_content).score;
        let confidence = playbook_candidate_confidence(source_memory_ids.len(), specificity);
        let reason = format!(
            "playbook extract observed {} semantic memories repeating `{}`.",
            source_memory_ids.len(),
            group.command_pattern
        );
        let mut candidate_id = None;
        let mut audit_id = None;
        let mut persisted = false;

        if duplicate {
            duplicate_count += 1;
        } else if !options.dry_run {
            let new_candidate_id = generate_curation_candidate_id();
            let new_audit_id = generate_audit_id();
            let source_id = source_memory_ids.join(",");
            let input = CreateCurationCandidateInput {
                workspace_id: prepared.workspace_id.clone(),
                candidate_type: CandidateType::Rule.as_str().to_owned(),
                target_memory_id: target_memory_id.clone(),
                proposed_content: Some(proposed_content.clone()),
                proposed_confidence: Some(confidence),
                proposed_trust_class: None,
                source_type: CandidateSource::RuleEngine.as_str().to_owned(),
                source_id: Some(source_id),
                reason: reason.clone(),
                confidence,
                status: Some("pending".to_owned()),
                created_at: None,
                ttl_expires_at: None,
            };
            let details = playbook_candidate_audit_details(
                &new_candidate_id,
                &group.command_pattern,
                &source_memory_ids,
                &proposed_content,
                confidence,
            );
            connection
                .with_transaction(|| {
                    connection.insert_curation_candidate(&new_candidate_id, &input)?;
                    connection.insert_audit(
                        &new_audit_id,
                        &CreateAuditInput {
                            workspace_id: Some(prepared.workspace_id.clone()),
                            actor: Some(actor.to_owned()),
                            action: audit_actions::CURATION_CANDIDATE_CREATE.to_owned(),
                            target_type: Some("curation_candidate".to_owned()),
                            target_id: Some(new_candidate_id.clone()),
                            details: Some(details.clone()),
                        },
                    )
                })
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to persist playbook candidate: {error}"),
                    repair: Some("ee curate candidates --type rule --json".to_owned()),
                })?;
            candidate_id = Some(new_candidate_id);
            audit_id = Some(new_audit_id);
            persisted = true;
            persisted_count += 1;
        }

        candidates.push(PlaybookRuleCandidate {
            candidate_id,
            candidate_type: CandidateType::Rule.as_str().to_owned(),
            target_memory_id,
            proposed_content,
            command_pattern: group.command_pattern,
            specificity_score: specificity,
            confidence,
            reason,
            source_memory_ids,
            persisted,
            duplicate,
            audit_id,
        });
    }

    candidates.sort_by(|left, right| {
        left.duplicate
            .cmp(&right.duplicate)
            .then_with(|| {
                right
                    .source_memory_ids
                    .len()
                    .cmp(&left.source_memory_ids.len())
            })
            .then_with(|| left.command_pattern.cmp(&right.command_pattern))
            .then_with(|| left.proposed_content.cmp(&right.proposed_content))
    });

    let next_action = if persisted_count > 0 {
        "ee curate candidates --type rule --json".to_owned()
    } else if options.dry_run && !candidates.is_empty() {
        "ee playbook extract --workspace . --json".to_owned()
    } else {
        "no action required".to_owned()
    };

    Ok(PlaybookExtractReport {
        schema: PLAYBOOK_EXTRACT_SCHEMA_V1,
        command: "playbook extract",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        since: since.map(|timestamp| timestamp.to_rfc3339()),
        scanned_memory_count,
        candidate_count: candidates.len(),
        persisted_count,
        duplicate_count,
        dry_run: options.dry_run,
        durable_mutation: persisted_count > 0,
        candidates,
        degraded: Vec::new(),
        next_action,
    })
}

/// List procedural rules in the portable playbook shape.
pub fn list_playbook_rules(
    options: &PlaybookListOptions<'_>,
) -> Result<PlaybookListReport, DomainError> {
    validate_list_window(options.limit)?;
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee playbook list --help"),
    )?;
    let snapshot = load_playbook_snapshot(
        &prepared,
        options.include_tombstoned,
        options.limit,
        options.offset,
    )?;

    Ok(PlaybookListReport {
        schema: PLAYBOOK_LIST_SCHEMA_V1,
        command: "playbook list",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        total_count: snapshot.total_count,
        returned_count: snapshot.rules.len(),
        limit: options.limit,
        offset: options.offset,
        truncated: snapshot.truncated,
        rules: snapshot.rules,
        degraded: Vec::new(),
    })
}

/// Export procedural rules as a portable, local playbook artifact.
pub fn export_playbook(
    options: &PlaybookExportOptions<'_>,
) -> Result<PlaybookExportReport, DomainError> {
    validate_list_window(options.limit)?;
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee playbook export --help"),
    )?;
    let snapshot = load_playbook_snapshot(&prepared, options.include_tombstoned, options.limit, 0)?;
    let exported_at = Utc::now().to_rfc3339();
    let document = PlaybookPortableDocument {
        schema: PLAYBOOK_PORTABLE_SCHEMA_V1.to_owned(),
        exported_at,
        ee_version: env!("CARGO_PKG_VERSION").to_owned(),
        workspace_id: prepared.workspace_id.clone(),
        workspace_path: prepared.workspace_path.display().to_string(),
        rule_count: snapshot.rules.len(),
        rules: snapshot.rules,
    };
    let bytes = serde_json::to_vec_pretty(&document).map_err(|error| DomainError::Storage {
        message: format!("failed to serialize playbook export: {error}"),
        repair: Some("inspect rule content and retry".to_owned()),
    })?;
    let artifact_hash = hash_bytes(&bytes);
    if !options.dry_run {
        write_side_path_no_overwrite(options.output_path, &bytes)?;
    }

    Ok(PlaybookExportReport {
        schema: PLAYBOOK_EXPORT_SCHEMA_V1,
        command: "playbook export",
        version: env!("CARGO_PKG_VERSION"),
        status: if options.dry_run {
            "dry_run".to_owned()
        } else {
            "exported".to_owned()
        },
        dry_run: options.dry_run,
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        output_path: options.output_path.display().to_string(),
        artifact_hash,
        total_count: snapshot.total_count,
        exported_count: document.rule_count,
        truncated: snapshot.truncated,
        no_overwrite: true,
        redaction_status: "metadata_and_rule_text_only_no_memory_content".to_owned(),
        document,
        degraded: Vec::new(),
    })
}

/// Import a portable playbook artifact into procedural rules.
pub fn import_playbook(
    options: &PlaybookImportOptions<'_>,
) -> Result<PlaybookImportReport, DomainError> {
    let prepared = prepare_rule_read(
        options.workspace_path,
        options.database_path,
        Some("ee playbook import --help"),
    )?;
    let bytes = read_side_path_no_symlinks(options.source_path)?;
    let source_hash = hash_bytes(&bytes);
    let document: PlaybookPortableDocument =
        serde_json::from_slice(&bytes).map_err(|error| DomainError::Import {
            message: format!("malformed playbook JSON: {error}"),
            repair: Some(
                "use `ee playbook export --out <path> --json` to create a supported file"
                    .to_owned(),
            ),
        })?;
    validate_playbook_document(&document)?;

    let existing = load_playbook_snapshot(&prepared, true, MAX_RULE_LIST_LIMIT, 0)?;
    let mut existing_contents = existing
        .rules
        .iter()
        .map(|rule| normalize_rule_text(&rule.content))
        .collect::<BTreeSet<_>>();
    let actor = options
        .actor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ee playbook import");
    if !options.dry_run {
        preflight_playbook_import_candidates(&document, &prepared, &existing_contents, actor)?;
    }

    let mut imported_count = 0_usize;
    let mut duplicate_count = 0_usize;
    let mut skipped_count = 0_usize;
    let mut downgraded_count = 0_usize;
    let mut decisions = Vec::new();

    for rule in &document.rules {
        let content_hash = hash_str(&rule.content);
        let mut issue_codes = Vec::new();
        let normalized = normalize_rule_text(&rule.content);
        if existing_contents.contains(&normalized) {
            duplicate_count += 1;
            decisions.push(PlaybookImportDecision {
                source_rule_id: rule.source_rule_id.clone(),
                content_hash,
                status: "duplicate".to_owned(),
                imported_rule_id: None,
                audit_id: None,
                index_job_id: None,
                issue_codes,
            });
            continue;
        }

        let import_maturity = portable_import_maturity(rule, &mut issue_codes);
        if import_maturity.is_none() {
            skipped_count += 1;
            decisions.push(PlaybookImportDecision {
                source_rule_id: rule.source_rule_id.clone(),
                content_hash,
                status: "skipped".to_owned(),
                imported_rule_id: None,
                audit_id: None,
                index_job_id: None,
                issue_codes,
            });
            continue;
        }
        if issue_codes
            .iter()
            .any(|code| code == "validated_rule_imported_as_candidate")
        {
            downgraded_count += 1;
        }
        let maturity = import_maturity.unwrap_or(RuleMaturity::Candidate.as_str());

        if options.dry_run {
            existing_contents.insert(normalized);
            decisions.push(PlaybookImportDecision {
                source_rule_id: rule.source_rule_id.clone(),
                content_hash,
                status: "would_import".to_owned(),
                imported_rule_id: None,
                audit_id: None,
                index_job_id: None,
                issue_codes,
            });
            continue;
        }

        let add_options = RuleAddOptions {
            workspace_path: &prepared.workspace_path,
            database_path: Some(&prepared.database_path),
            content: &rule.content,
            scope: &rule.scope,
            scope_pattern: rule.scope_pattern.as_deref(),
            maturity,
            confidence: Some(rule.confidence),
            utility: rule.utility,
            importance: rule.importance,
            trust_class: &rule.trust_class,
            protected: rule.protected,
            tags: &rule.tags,
            source_memory_ids: &[],
            dry_run: false,
            actor: Some(actor),
        };
        let add_report = add_rule(&add_options)?;
        existing_contents.insert(normalized);
        imported_count += 1;
        decisions.push(PlaybookImportDecision {
            source_rule_id: rule.source_rule_id.clone(),
            content_hash,
            status: "imported".to_owned(),
            imported_rule_id: Some(add_report.rule_id),
            audit_id: add_report.audit_id,
            index_job_id: add_report.index_job_id,
            issue_codes,
        });
    }

    let would_import_count = decisions
        .iter()
        .filter(|decision| decision.status == "would_import")
        .count();
    let status = if options.dry_run {
        "dry_run"
    } else if imported_count > 0 {
        "imported"
    } else if duplicate_count > 0 && skipped_count == 0 {
        "duplicates_only"
    } else {
        "no_changes"
    };

    Ok(PlaybookImportReport {
        schema: PLAYBOOK_IMPORT_SCHEMA_V1,
        command: "playbook import",
        version: env!("CARGO_PKG_VERSION"),
        status: status.to_owned(),
        dry_run: options.dry_run,
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        source_path: options.source_path.display().to_string(),
        source_hash,
        source_schema: document.schema,
        source_rule_count: document.rules.len(),
        imported_count: if options.dry_run {
            would_import_count
        } else {
            imported_count
        },
        duplicate_count,
        skipped_count,
        downgraded_count,
        durable_mutation: !options.dry_run && imported_count > 0,
        decisions,
        degraded: Vec::new(),
    })
}

#[derive(Clone, Debug)]
struct PlaybookSnapshot {
    total_count: usize,
    truncated: bool,
    rules: Vec<PlaybookPortableRule>,
}

fn load_playbook_snapshot(
    prepared: &PreparedRuleRead,
    include_tombstoned: bool,
    limit: u32,
    offset: u32,
) -> Result<PlaybookSnapshot, DomainError> {
    validate_list_window(limit)?;
    let connection = open_existing_database(&prepared.database_path)?;
    let stored = connection
        .list_procedural_rules(&prepared.workspace_id, None, None, include_tombstoned)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list playbook rules: {error}"),
            repair: Some("ee rule list --json".to_owned()),
        })?;
    let mut rules = Vec::with_capacity(stored.len());
    for rule in stored {
        rules.push(playbook_rule_from_details(load_rule_details(
            &connection,
            rule,
        )?));
    }
    rules.sort_by(|left, right| {
        left.maturity
            .cmp(&right.maturity)
            .then_with(|| left.content.cmp(&right.content))
            .then_with(|| left.source_rule_id.cmp(&right.source_rule_id))
    });

    let total_count = rules.len();
    let offset = usize::try_from(offset).map_err(|_| {
        rule_read_usage_error(
            "playbook list offset is too large".to_owned(),
            "ee playbook list --help",
        )
    })?;
    let limit = usize::try_from(limit).map_err(|_| {
        rule_read_usage_error(
            "playbook list limit is too large".to_owned(),
            "ee playbook list --help",
        )
    })?;
    let rules = rules
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let truncated = offset.saturating_add(rules.len()) < total_count;
    Ok(PlaybookSnapshot {
        total_count,
        truncated,
        rules,
    })
}

fn playbook_rule_from_details(rule: RuleDetails) -> PlaybookPortableRule {
    PlaybookPortableRule {
        source_rule_id: Some(rule.id),
        content: rule.content,
        maturity: rule.maturity,
        scope: rule.scope,
        scope_pattern: rule.scope_pattern,
        trust_class: rule.trust_class,
        protected: rule.protected,
        confidence: rule.confidence,
        utility: rule.utility,
        importance: rule.importance,
        tags: rule.tags,
        source_memory_count: rule.source_memory_ids.len(),
        source_memory_ids: rule.source_memory_ids,
        created_at: Some(rule.created_at),
        updated_at: Some(rule.updated_at),
    }
}

fn portable_import_maturity(
    rule: &PlaybookPortableRule,
    issue_codes: &mut Vec<String>,
) -> Option<&'static str> {
    let parsed = RuleMaturity::from_str(&rule.maturity).ok()?;
    if parsed.is_terminal() {
        issue_codes.push("terminal_maturity_not_imported".to_owned());
        return None;
    }
    if parsed == RuleMaturity::Validated {
        issue_codes.push("validated_rule_imported_as_candidate".to_owned());
        return Some(RuleMaturity::Candidate.as_str());
    }
    Some(parsed.as_str())
}

fn validate_playbook_document(document: &PlaybookPortableDocument) -> Result<(), DomainError> {
    if document.schema != PLAYBOOK_PORTABLE_SCHEMA_V1 {
        return Err(DomainError::Import {
            message: format!("unsupported playbook schema `{}`", document.schema),
            repair: Some(format!("expected schema `{PLAYBOOK_PORTABLE_SCHEMA_V1}`")),
        });
    }
    if document.rule_count != document.rules.len() {
        return Err(DomainError::Import {
            message: format!(
                "playbook rule_count {} does not match {} rule row(s)",
                document.rule_count,
                document.rules.len()
            ),
            repair: Some("recreate the playbook with `ee playbook export --out <path>`".to_owned()),
        });
    }
    Ok(())
}

fn preflight_playbook_import_candidates(
    document: &PlaybookPortableDocument,
    prepared: &PreparedRuleRead,
    existing_contents: &BTreeSet<String>,
    actor: &str,
) -> Result<(), DomainError> {
    let mut seen_contents = existing_contents.clone();
    for rule in &document.rules {
        let normalized = normalize_rule_text(&rule.content);
        if seen_contents.contains(&normalized) {
            continue;
        }

        let mut issue_codes = Vec::new();
        let Some(maturity) = portable_import_maturity(rule, &mut issue_codes) else {
            continue;
        };
        prepare_rule_add(&RuleAddOptions {
            workspace_path: &prepared.workspace_path,
            database_path: Some(&prepared.database_path),
            content: &rule.content,
            scope: &rule.scope,
            scope_pattern: rule.scope_pattern.as_deref(),
            maturity,
            confidence: Some(rule.confidence),
            utility: rule.utility,
            importance: rule.importance,
            trust_class: &rule.trust_class,
            protected: rule.protected,
            tags: &rule.tags,
            source_memory_ids: &[],
            dry_run: true,
            actor: Some(actor),
        })
        .map_err(|error| DomainError::Import {
            message: format!(
                "playbook rule {} is invalid: {}",
                rule.source_rule_id.as_deref().unwrap_or("<portable>"),
                error.message()
            ),
            repair: Some(
                "fix the playbook source or recreate it with `ee playbook export --out <path>`"
                    .to_owned(),
            ),
        })?;
        seen_contents.insert(normalized);
    }
    Ok(())
}

fn write_side_path_no_overwrite(path: &Path, bytes: &[u8]) -> Result<(), DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "playbook export path '{}' traverses symbolic link '{}'",
                path.display(),
                symlink_path.display()
            ),
            repair: Some("choose a real, non-symlink --out path".to_owned()),
        });
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create playbook export directory '{}': {error}",
                parent.display()
            ),
            repair: Some("choose a writable --out path".to_owned()),
        })?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create playbook export '{}': {error}",
                path.display()
            ),
            repair: Some(
                "choose a new --out path; ee never overwrites existing playbook exports".to_owned(),
            ),
        })?;
    file.write_all(bytes)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "failed to write playbook export '{}': {error}",
                path.display()
            ),
            repair: Some("inspect the partial side-path artifact before retrying".to_owned()),
        })?;
    file.write_all(b"\n")
        .map_err(|error| DomainError::Storage {
            message: format!(
                "failed to finish playbook export '{}': {error}",
                path.display()
            ),
            repair: Some("inspect the partial side-path artifact before retrying".to_owned()),
        })?;
    file.sync_all().map_err(|error| DomainError::Storage {
        message: format!(
            "failed to sync playbook export '{}': {error}",
            path.display()
        ),
        repair: Some("inspect disk health and retry".to_owned()),
    })
}

fn read_side_path_no_symlinks(path: &Path) -> Result<Vec<u8>, DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "playbook import source path '{}' traverses symbolic link '{}'",
                path.display(),
                symlink_path.display()
            ),
            repair: Some("choose a real, non-symlink playbook source path".to_owned()),
        });
    }

    fs::read(path).map_err(|error| DomainError::Import {
        message: format!(
            "failed to read playbook source '{}': {error}",
            path.display()
        ),
        repair: Some("choose a readable playbook JSON file".to_owned()),
    })
}

fn first_existing_symlink_component(path: &Path) -> Result<Option<PathBuf>, DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "failed to inspect playbook path component '{}': {error}",
                        current.display()
                    ),
                    repair: Some(
                        "inspect filesystem permissions or choose another playbook path".to_owned(),
                    ),
                });
            }
        }
    }
    Ok(None)
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn hash_str(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

#[derive(Clone, Debug)]
struct PlaybookMemoryGroup {
    command_pattern: String,
    memories: Vec<StoredMemory>,
    tags: BTreeSet<String>,
    release_signal: bool,
}

fn group_playbook_memories(
    connection: &DbConnection,
    memories: Vec<StoredMemory>,
) -> Result<BTreeMap<String, PlaybookMemoryGroup>, DomainError> {
    let mut groups = BTreeMap::new();
    for memory in memories {
        let Some(command_pattern) = extract_command_pattern(&memory.content) else {
            continue;
        };
        let tags =
            connection
                .get_memory_tags(&memory.id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to read tags for memory {}: {error}", memory.id),
                    repair: Some("ee memory show <memory-id> --json".to_owned()),
                })?;
        let release_signal = playbook_release_signal(&memory.content, &tags);
        let entry = groups
            .entry(command_pattern.clone())
            .or_insert_with(|| PlaybookMemoryGroup {
                command_pattern,
                memories: Vec::new(),
                tags: BTreeSet::new(),
                release_signal: false,
            });
        entry.release_signal |= release_signal;
        entry.tags.extend(tags);
        entry.memories.push(memory);
    }
    Ok(groups)
}

fn extract_command_pattern(content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    const KNOWN_COMMAND_PATTERNS: &[(&str, &str)] = &[
        (
            "cargo clippy --all-targets -- -d warnings",
            "cargo clippy --all-targets -- -D warnings",
        ),
        ("cargo fmt --check", "cargo fmt --check"),
        ("cargo check --all-targets", "cargo check --all-targets"),
        ("cargo test", "cargo test"),
        ("bv --robot-triage", "bv --robot-triage"),
        ("bv --robot-next", "bv --robot-next"),
        ("br ready --json", "br ready --json"),
        ("br sync --flush-only", "br sync --flush-only"),
        ("ubs", "ubs"),
    ];
    for (lower_pattern, original_pattern) in KNOWN_COMMAND_PATTERNS {
        if lower.contains(*lower_pattern) {
            return Some((*original_pattern).to_owned());
        }
    }

    let mut in_backticks = false;
    let mut span = String::new();
    for ch in content.chars() {
        if ch == '`' {
            if in_backticks {
                let command = span.trim();
                if looks_like_command(command) {
                    return Some(command.to_owned());
                }
                span.clear();
                in_backticks = false;
            } else {
                in_backticks = true;
            }
            continue;
        }
        if in_backticks {
            span.push(ch);
        }
    }
    None
}

fn looks_like_command(value: &str) -> bool {
    let value = value.trim();
    const COMMAND_PREFIXES: &[&str] = &[
        "cargo ", "ee ", "git ", "gh ", "br ", "bv ", "cass ", "rch ", "ubs",
    ];
    COMMAND_PREFIXES
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn playbook_release_signal(content: &str, tags: &[String]) -> bool {
    let lower = content.to_ascii_lowercase();
    lower.contains("release")
        || tags.iter().any(|tag| {
            matches!(
                tag.as_str(),
                "release" | "ci" | "verification" | "preflight"
            )
        })
}

fn proposed_playbook_rule(group: &PlaybookMemoryGroup) -> String {
    if group.release_signal {
        format!(
            "Run `{}` from the workspace root before release work on main; if it fails, store the failure with `ee remember --kind failure`.",
            group.command_pattern
        )
    } else {
        format!(
            "Run `{}` from the workspace root when the matching workflow applies; if it fails, store the failure with `ee remember --kind failure`.",
            group.command_pattern
        )
    }
}

fn playbook_candidate_confidence(source_memory_count: usize, specificity: f32) -> f32 {
    let evidence_score = (source_memory_count as f32 * 0.06).min(0.30);
    (0.45 + evidence_score + (specificity * 0.20)).min(0.90)
}

fn existing_rule_candidate_contents(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<BTreeSet<String>, DomainError> {
    let mut contents = BTreeSet::new();
    let candidates = connection
        .list_curation_candidates(workspace_id, Some(CandidateType::Rule.as_str()), None, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list existing rule candidates: {error}"),
            repair: Some("ee curate candidates --type rule --json".to_owned()),
        })?;
    for candidate in candidates {
        if let Some(content) = candidate.proposed_content {
            contents.insert(normalize_rule_text(&content));
        }
    }

    let rules = connection
        .list_procedural_rules(workspace_id, None, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list existing procedural rules: {error}"),
            repair: Some("ee rule list --json".to_owned()),
        })?;
    for rule in rules {
        contents.insert(normalize_rule_text(&rule.content));
    }
    Ok(contents)
}

fn normalize_rule_text(content: &str) -> String {
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn parse_playbook_since(raw: Option<&str>) -> Result<Option<DateTime<Utc>>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|error| {
                    rule_read_usage_error(
                        format!("invalid --since timestamp `{value}`: {error}"),
                        "ee playbook extract --help",
                    )
                })
        })
        .transpose()
}

fn memory_is_after_since(memory: &StoredMemory, since: Option<&DateTime<Utc>>) -> bool {
    let Some(since) = since else {
        return true;
    };
    DateTime::parse_from_rfc3339(&memory.created_at)
        .map(|created_at| created_at.with_timezone(&Utc) >= *since)
        .unwrap_or(true)
}

fn validate_playbook_limit(limit: u32) -> Result<(), DomainError> {
    if limit == 0 {
        return Err(rule_read_usage_error(
            "playbook extract --limit must be greater than zero".to_owned(),
            "ee playbook extract --help",
        ));
    }
    if limit > MAX_PLAYBOOK_EXTRACT_LIMIT {
        return Err(rule_read_usage_error(
            format!("playbook extract --limit must be <= {MAX_PLAYBOOK_EXTRACT_LIMIT}"),
            "ee playbook extract --help",
        ));
    }
    Ok(())
}

fn generate_curation_candidate_id() -> String {
    let candidate = CandidateId::from_uuid(uuid::Uuid::now_v7()).to_string();
    format!("curate_{}", candidate.trim_start_matches("cand_"))
}

fn playbook_candidate_audit_details(
    candidate_id: &str,
    command_pattern: &str,
    source_memory_ids: &[String],
    proposed_content: &str,
    confidence: f32,
) -> String {
    serde_json::json!({
        "schema": "ee.audit.playbook_candidate_create.v1",
        "command": "ee playbook extract",
        "candidateId": candidate_id,
        "candidateType": CandidateType::Rule.as_str(),
        "commandPattern": command_pattern,
        "sourceMemoryIds": source_memory_ids,
        "sourceMemoryCount": source_memory_ids.len(),
        "proposedContent": proposed_content,
        "confidence": confidence,
    })
    .to_string()
}

fn prepare_rule_read(
    workspace_path: &Path,
    database_path: Option<&Path>,
    repair: Option<&str>,
) -> Result<PreparedRuleRead, DomainError> {
    let workspace_path =
        resolve_workspace_path(workspace_path, false).map_err(|error| match error {
            DomainError::Configuration { message, .. } => DomainError::Configuration {
                message,
                repair: repair.map(str::to_owned),
            },
            other => other,
        })?;
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    Ok(PreparedRuleRead {
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

fn parse_optional_maturity(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            RuleMaturity::from_str(value)
                .map(|maturity| maturity.as_str().to_owned())
                .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule list --help"))
        })
        .transpose()
}

fn parse_optional_scope(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            RuleScope::from_str(value)
                .map(|scope| scope.as_str().to_owned())
                .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule list --help"))
        })
        .transpose()
}

fn parse_optional_tag(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            Tag::parse(value)
                .map(|tag| tag.to_string())
                .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule list --help"))
        })
        .transpose()
}

fn validate_list_window(limit: u32) -> Result<(), DomainError> {
    if limit == 0 {
        return Err(rule_read_usage_error(
            "rule list --limit must be greater than zero".to_owned(),
            "ee rule list --help",
        ));
    }
    if limit > MAX_RULE_LIST_LIMIT {
        return Err(rule_read_usage_error(
            format!("rule list --limit must be <= {MAX_RULE_LIST_LIMIT}"),
            "ee rule list --help",
        ));
    }
    Ok(())
}

fn load_rule_details(
    connection: &DbConnection,
    stored: StoredProceduralRule,
) -> Result<RuleDetails, DomainError> {
    let tags = connection
        .get_rule_tags(&stored.id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query rule tags: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let source_memory_ids = connection
        .get_rule_source_memory_ids(&stored.id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query rule source memories: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let lifecycle = rule_lifecycle(&stored.maturity, source_memory_ids.len());
    let evidence = rule_evidence(&stored.maturity, source_memory_ids.len());

    Ok(RuleDetails {
        id: stored.id,
        workspace_id: stored.workspace_id,
        content: stored.content,
        confidence: stored.confidence,
        utility: stored.utility,
        importance: stored.importance,
        trust_class: stored.trust_class,
        scope: stored.scope,
        scope_pattern: stored.scope_pattern,
        maturity: stored.maturity,
        protected: stored.protected,
        lifecycle,
        positive_feedback_count: stored.positive_feedback_count,
        negative_feedback_count: stored.negative_feedback_count,
        last_applied_at: stored.last_applied_at,
        last_validated_at: stored.last_validated_at,
        superseded_by: stored.superseded_by,
        source_memory_ids,
        tags,
        evidence,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
        tombstoned_at: stored.tombstoned_at,
    })
}

fn rule_summary_from_details(details: RuleDetails) -> RuleSummary {
    let (content, content_truncated) = truncate_rule_content(&details.content);
    RuleSummary {
        id: details.id,
        content,
        content_truncated,
        maturity: details.maturity,
        lifecycle: details.lifecycle,
        scope: details.scope,
        scope_pattern: details.scope_pattern,
        trust_class: details.trust_class,
        protected: details.protected,
        confidence: details.confidence,
        utility: details.utility,
        importance: details.importance,
        evidence: details.evidence,
        tags: details.tags,
        is_tombstoned: details.tombstoned_at.is_some(),
        created_at: details.created_at,
        updated_at: details.updated_at,
    }
}

fn rule_lifecycle(maturity: &str, source_memory_count: usize) -> RuleLifecycle {
    let parsed = RuleMaturity::from_str(maturity).ok();
    RuleLifecycle {
        maturity: maturity.to_owned(),
        is_active: parsed.is_some_and(RuleMaturity::is_active),
        is_terminal: parsed.is_some_and(RuleMaturity::is_terminal),
        next_action: rule_next_action(parsed, source_memory_count),
    }
}

fn rule_next_action(maturity: Option<RuleMaturity>, source_memory_count: usize) -> String {
    match maturity {
        Some(RuleMaturity::Draft) => "promote to candidate when evidence exists".to_owned(),
        Some(RuleMaturity::Candidate) if source_memory_count == 0 => {
            "attach source memory evidence before validation".to_owned()
        }
        Some(RuleMaturity::Candidate) => "record outcomes or validate evidence".to_owned(),
        Some(RuleMaturity::Validated) => "monitor feedback and decay signals".to_owned(),
        Some(RuleMaturity::Deprecated) => {
            "keep for history; avoid selecting for new context".to_owned()
        }
        Some(RuleMaturity::Superseded) => {
            "follow superseded_by replacement when present".to_owned()
        }
        None => "repair malformed rule maturity".to_owned(),
    }
}

fn rule_evidence(maturity: &str, source_memory_count: usize) -> RuleEvidence {
    let verified = maturity == RuleMaturity::Validated.as_str() && source_memory_count > 0;
    let status = match (source_memory_count, verified) {
        (0, _) => "missing",
        (_, true) => "verified",
        _ => "attached",
    };
    RuleEvidence {
        status: status.to_owned(),
        source_memory_count,
        verified,
        requirement: "validated rules require at least one source memory".to_owned(),
    }
}

fn truncate_rule_content(content: &str) -> (String, bool) {
    let mut chars = content.chars();
    let preview = chars.by_ref().take(80).collect::<String>();
    if chars.next().is_some() {
        (format!("{preview}..."), true)
    } else {
        (preview, false)
    }
}

fn rule_not_found(rule_id: &str) -> DomainError {
    DomainError::NotFound {
        resource: "procedural rule".to_owned(),
        id: rule_id.to_owned(),
        repair: Some("ee rule list --json".to_owned()),
    }
}

fn parse_rule_id_for_command(raw: &str, repair: &str) -> Result<String, DomainError> {
    RuleId::from_str(raw)
        .map(|rule_id| rule_id.to_string())
        .map_err(|error| rule_read_usage_error(format!("invalid rule ID: {error}"), repair))
}

fn load_active_rule(
    connection: &DbConnection,
    workspace_id: &str,
    rule_id: &str,
) -> Result<StoredProceduralRule, DomainError> {
    let Some(rule) =
        connection
            .get_procedural_rule(rule_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query procedural rule: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?
    else {
        return Err(rule_not_found(rule_id));
    };
    if rule.workspace_id != workspace_id || rule.tombstoned_at.is_some() {
        return Err(rule_not_found(rule_id));
    }
    Ok(rule)
}

fn build_rule_lifecycle_evidence(
    connection: &DbConnection,
    workspace_id: &str,
    stored: &StoredProceduralRule,
    options: &RuleMarkOptions<'_>,
) -> Result<RuleLifecycleEvidence, DomainError> {
    let mut evidence = RuleLifecycleEvidence::new()
        .with_helpful_outcomes(options.evidence.helpful_outcomes)
        .with_harmful_outcomes(
            options.evidence.harmful_outcomes,
            options.evidence.distinct_harmful_sources,
        )
        .with_protected_rule(stored.protected)
        .with_manual_curation_approved(options.evidence.manual_curation_approved)
        .with_intervening_helpful_from_harmful_sources(
            options.evidence.intervening_helpful_from_harmful_sources,
        )
        .with_validation_passes(options.evidence.validation_passes)
        .with_validation_contradictions(options.evidence.validation_contradictions)
        .with_review_approved(options.evidence.review_approved);

    if let Some(raw_superseding_rule_id) = options.evidence.superseding_rule_id {
        let superseding_rule_id =
            parse_rule_id_for_command(raw_superseding_rule_id, "ee rule mark --help")?;
        if superseding_rule_id == stored.id {
            return Err(rule_read_usage_error(
                "a rule cannot supersede itself".to_owned(),
                "ee rule mark --help",
            ));
        }
        let superseding = load_active_rule(connection, workspace_id, &superseding_rule_id)?;
        if superseding.maturity == RuleMaturity::Deprecated.as_str() {
            return Err(rule_read_usage_error(
                "a deprecated rule cannot be used as the superseding replacement".to_owned(),
                "ee rule mark --help",
            ));
        }
        evidence = evidence.with_superseding_rule(superseding_rule_id);
    }

    Ok(evidence)
}

fn positive_feedback_delta(trigger: RuleLifecycleTrigger, evidence: &RuleLifecycleEvidence) -> u32 {
    match trigger {
        RuleLifecycleTrigger::OutcomeHelpful => evidence.helpful_outcomes.max(1),
        RuleLifecycleTrigger::ValidationPassed => evidence.validation_passes.max(1),
        RuleLifecycleTrigger::ReviewApproved => u32::from(evidence.review_approved).max(1),
        _ => 0,
    }
}

fn negative_feedback_delta(trigger: RuleLifecycleTrigger, evidence: &RuleLifecycleEvidence) -> u32 {
    match trigger {
        RuleLifecycleTrigger::OutcomeHarmful => evidence.harmful_outcomes.max(1),
        RuleLifecycleTrigger::ValidationContradicted => evidence.validation_contradictions.max(1),
        _ => 0,
    }
}

fn last_validated_marker(trigger: RuleLifecycleTrigger, timestamp: &str) -> Option<String> {
    matches!(
        trigger,
        RuleLifecycleTrigger::ValidationPassed
            | RuleLifecycleTrigger::ValidationContradicted
            | RuleLifecycleTrigger::ReviewApproved
    )
    .then(|| timestamp.to_owned())
}

fn apply_score_delta(score: f32, delta: f64) -> f32 {
    let adjusted = f64::from(score) + delta;
    adjusted.clamp(0.0, 1.0) as f32
}

fn score_changed(previous: f32, next: f32) -> bool {
    (previous - next).abs() > f32::EPSILON
}

struct ApplyMarkToDetail<'a> {
    transition: &'a RuleLifecycleTransition,
    confidence: f32,
    utility: f32,
    superseded_by: Option<String>,
    positive_feedback_delta: u32,
    negative_feedback_delta: u32,
    last_validated_at: Option<String>,
    updated_at: &'a str,
}

fn apply_mark_to_detail(mut detail: RuleDetails, input: ApplyMarkToDetail<'_>) -> RuleDetails {
    detail.maturity = input.transition.next_maturity.as_str().to_owned();
    detail.confidence = input.confidence;
    detail.utility = input.utility;
    detail.lifecycle = rule_lifecycle(&detail.maturity, detail.source_memory_ids.len());
    detail.evidence = rule_evidence(&detail.maturity, detail.source_memory_ids.len());
    detail.positive_feedback_count = detail
        .positive_feedback_count
        .saturating_add(input.positive_feedback_delta);
    detail.negative_feedback_count = detail
        .negative_feedback_count
        .saturating_add(input.negative_feedback_delta);
    if input.last_validated_at.is_some() {
        detail.last_validated_at = input.last_validated_at;
    }
    detail.superseded_by = input.superseded_by;
    detail.updated_at = input.updated_at.to_owned();
    detail
}

fn transition_report(transition: &RuleLifecycleTransition) -> RuleMarkTransition {
    RuleMarkTransition {
        trigger: transition.trigger.as_str().to_owned(),
        action: transition.action.as_str().to_owned(),
        prior_maturity: transition.prior_maturity.as_str().to_owned(),
        next_maturity: transition.next_maturity.as_str().to_owned(),
        allowed: transition.allowed,
        requires_curation: transition.requires_curation,
        audit_required: transition.audit_required,
        confidence_delta: transition.confidence_delta,
        utility_delta: transition.utility_delta,
        reason: transition.reason.clone(),
    }
}

fn lifecycle_evidence_report(evidence: &RuleLifecycleEvidence) -> RuleMarkEvidenceReport {
    RuleMarkEvidenceReport {
        helpful_outcomes: evidence.helpful_outcomes,
        harmful_outcomes: evidence.harmful_outcomes,
        distinct_harmful_sources: evidence.distinct_harmful_sources,
        protected_rule: evidence.protected_rule,
        manual_curation_approved: evidence.manual_curation_approved,
        intervening_helpful_from_harmful_sources: evidence.intervening_helpful_from_harmful_sources,
        validation_passes: evidence.validation_passes,
        validation_contradictions: evidence.validation_contradictions,
        review_approved: evidence.review_approved,
        superseding_rule_id: evidence.superseding_rule_id.clone(),
    }
}

struct RuleMarkReportInput<'a> {
    prepared: &'a PreparedRuleRead,
    rule_id: &'a str,
    dry_run: bool,
    persisted: bool,
    changed: bool,
    audit_id: Option<String>,
    index_job_id: Option<String>,
    transition: &'a RuleLifecycleTransition,
    evidence: &'a RuleLifecycleEvidence,
    previous_rule: RuleDetails,
    rule: RuleDetails,
}

fn rule_mark_report(input: RuleMarkReportInput<'_>) -> RuleMarkReport {
    let status = if input.dry_run {
        if input.changed {
            "would_mark"
        } else {
            "unchanged"
        }
    } else if input.changed {
        "marked"
    } else {
        "recorded"
    };
    let index_status = if input.index_job_id.is_some() {
        "queued"
    } else if input.dry_run {
        "dry_run_not_queued"
    } else {
        "not_queued_no_indexable_change"
    };
    RuleMarkReport {
        schema: RULE_MARK_SCHEMA_V1,
        command: "rule mark",
        version: env!("CARGO_PKG_VERSION"),
        status: status.to_owned(),
        rule_id: input.rule_id.to_owned(),
        workspace_id: input.prepared.workspace_id.clone(),
        workspace_path: input.prepared.workspace_path.display().to_string(),
        database_path: input.prepared.database_path.display().to_string(),
        dry_run: input.dry_run,
        persisted: input.persisted,
        changed: input.changed,
        audit_id: input.audit_id,
        index_job_id: input.index_job_id,
        index_status: index_status.to_owned(),
        transition: transition_report(input.transition),
        evidence: lifecycle_evidence_report(input.evidence),
        previous_rule: input.previous_rule,
        rule: input.rule,
        degraded: Vec::new(),
    }
}

fn validate_rule_update_request(options: &RuleUpdateOptions<'_>) -> Result<(), DomainError> {
    if options.clear_scope_pattern && options.scope_pattern.is_some() {
        return Err(rule_read_usage_error(
            "`--scope-pattern` and `--clear-scope-pattern` cannot be used together".to_owned(),
            "ee rule update --help",
        ));
    }
    if options.protected.is_none()
        && options.content.is_none()
        && options.scope.is_none()
        && options.scope_pattern.is_none()
        && !options.clear_scope_pattern
        && options.trust_class.is_none()
        && options.confidence.is_none()
        && options.utility.is_none()
        && options.importance.is_none()
        && options.tags.is_none()
        && !options.clear_tags
        && options.source_memory_ids.is_none()
        && !options.clear_source_memory_ids
    {
        return Err(rule_read_usage_error(
            "rule update requires at least one field to update".to_owned(),
            "ee rule update --help",
        ));
    }
    if options.tags.is_some() && options.clear_tags {
        return Err(rule_read_usage_error(
            "`--tag` and `--clear-tags` cannot be used together".to_owned(),
            "ee rule update --help",
        ));
    }
    if options.source_memory_ids.is_some() && options.clear_source_memory_ids {
        return Err(rule_read_usage_error(
            "`--source-memory` and `--clear-source-memories` cannot be used together".to_owned(),
            "ee rule update --help",
        ));
    }
    Ok(())
}

fn prepare_rule_update(
    connection: &DbConnection,
    prepared: &PreparedRuleRead,
    previous: &RuleDetails,
    options: &RuleUpdateOptions<'_>,
) -> Result<PreparedRuleUpdate, DomainError> {
    let content = match options.content {
        Some(raw) => {
            let content = MemoryContent::parse(raw)
                .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule update --help"))?
                .as_str()
                .to_owned();
            if content.len() > MAX_RULE_CONTENT_BYTES {
                return Err(rule_read_usage_error(
                    format!(
                        "rule content is too large: {} bytes > {} bytes",
                        content.len(),
                        MAX_RULE_CONTENT_BYTES
                    ),
                    "ee rule update --help",
                ));
            }
            validate_rule_policy(&content)?;
            content
        }
        None => previous.content.clone(),
    };
    let scope = options
        .scope
        .map(RuleScope::from_str)
        .transpose()
        .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule update --help"))?
        .unwrap_or_else(|| RuleScope::from_str(&previous.scope).unwrap_or(RuleScope::Workspace));
    let candidate_scope_pattern = if options.clear_scope_pattern {
        None
    } else if let Some(pattern) = options.scope_pattern {
        Some(pattern.to_owned())
    } else if options.scope.is_some() && !scope.requires_pattern() {
        None
    } else {
        previous.scope_pattern.clone()
    };
    let scope_pattern = prepare_scope_pattern(scope, candidate_scope_pattern.as_deref())?;
    let trust_class = options
        .trust_class
        .map(TrustClass::from_str)
        .transpose()
        .map_err(|error| rule_read_usage_error(error.to_string(), "ee rule update --help"))?
        .map_or_else(
            || previous.trust_class.clone(),
            |value| value.as_str().to_owned(),
        );
    let confidence = parse_update_score(options.confidence, previous.confidence, "confidence")?;
    let utility = parse_update_score(options.utility, previous.utility, "utility")?;
    let importance = parse_update_score(options.importance, previous.importance, "importance")?;
    let protected = options.protected.unwrap_or(previous.protected);
    let tags = if options.clear_tags {
        Some(Vec::new())
    } else {
        options.tags.map(parse_tags).transpose()?
    };
    let source_memory_ids = if options.clear_source_memory_ids {
        Some(Vec::new())
    } else {
        options
            .source_memory_ids
            .map(parse_source_memory_ids)
            .transpose()?
    };
    let effective_source_memory_ids = source_memory_ids
        .as_ref()
        .unwrap_or(&previous.source_memory_ids);
    if previous.maturity == RuleMaturity::Validated.as_str()
        && effective_source_memory_ids.is_empty()
    {
        return Err(rule_read_usage_error(
            "validated rules require at least one source-memory evidence ID".to_owned(),
            "ee rule update --help",
        ));
    }
    if let Some(ids) = &source_memory_ids {
        verify_source_memories(connection, &prepared.workspace_id, ids)?;
    }

    let mut changed_fields = Vec::new();
    push_changed(&mut changed_fields, "content", previous.content != content);
    push_changed(
        &mut changed_fields,
        "scope",
        previous.scope != scope.as_str(),
    );
    push_changed(
        &mut changed_fields,
        "scopePattern",
        previous.scope_pattern != scope_pattern,
    );
    push_changed(
        &mut changed_fields,
        "trustClass",
        previous.trust_class != trust_class,
    );
    push_changed(
        &mut changed_fields,
        "confidence",
        score_changed(previous.confidence, confidence),
    );
    push_changed(
        &mut changed_fields,
        "utility",
        score_changed(previous.utility, utility),
    );
    push_changed(
        &mut changed_fields,
        "importance",
        score_changed(previous.importance, importance),
    );
    push_changed(
        &mut changed_fields,
        "protected",
        previous.protected != protected,
    );
    if let Some(tags) = &tags {
        push_changed(&mut changed_fields, "tags", previous.tags != *tags);
    }
    if let Some(source_memory_ids) = &source_memory_ids {
        push_changed(
            &mut changed_fields,
            "sourceMemoryIds",
            previous.source_memory_ids != *source_memory_ids,
        );
    }

    let updated_at = if changed_fields.is_empty() {
        previous.updated_at.clone()
    } else {
        Utc::now().to_rfc3339()
    };
    let mut next_detail = previous.clone();
    next_detail.content = content.clone();
    next_detail.scope = scope.as_str().to_owned();
    next_detail.scope_pattern = scope_pattern.clone();
    next_detail.trust_class = trust_class.clone();
    next_detail.confidence = confidence;
    next_detail.utility = utility;
    next_detail.importance = importance;
    next_detail.protected = protected;
    if let Some(tags) = &tags {
        next_detail.tags = tags.clone();
    }
    if let Some(source_memory_ids) = &source_memory_ids {
        next_detail.source_memory_ids = source_memory_ids.clone();
        next_detail.evidence = rule_evidence(&next_detail.maturity, source_memory_ids.len());
        next_detail.lifecycle = rule_lifecycle(&next_detail.maturity, source_memory_ids.len());
    }
    next_detail.updated_at = updated_at.clone();

    Ok(PreparedRuleUpdate {
        input: UpdateProceduralRuleInput {
            workspace_id: prepared.workspace_id.clone(),
            content,
            confidence,
            utility,
            importance,
            trust_class,
            scope: scope.as_str().to_owned(),
            scope_pattern,
            protected,
            source_memory_ids,
            tags,
            updated_at,
        },
        changed_fields,
        next_detail,
    })
}

fn parse_update_score(raw: Option<f32>, previous: f32, field: &str) -> Result<f32, DomainError> {
    raw.map(UnitScore::parse)
        .transpose()
        .map_err(|error| {
            rule_read_usage_error(format!("invalid {field}: {error}"), "ee rule update --help")
        })
        .map(|score| score.map_or(previous, UnitScore::into_inner))
}

fn push_changed(changed_fields: &mut Vec<String>, field: &str, changed: bool) {
    if changed {
        changed_fields.push(field.to_owned());
    }
}

struct RuleUpdateReportInput<'a> {
    prepared: &'a PreparedRuleRead,
    rule_id: &'a str,
    dry_run: bool,
    persisted: bool,
    changed: bool,
    changed_fields: Vec<String>,
    audit_id: Option<String>,
    index_job_id: Option<String>,
    previous_rule: RuleDetails,
    rule: RuleDetails,
}

fn rule_update_report(input: RuleUpdateReportInput<'_>) -> RuleUpdateReport {
    let status = if input.dry_run {
        if input.changed {
            "would_update"
        } else {
            "unchanged"
        }
    } else if input.changed {
        "updated"
    } else {
        "unchanged"
    };
    let index_status = if input.index_job_id.is_some() {
        "queued"
    } else if input.dry_run {
        "dry_run_not_queued"
    } else {
        "not_queued_no_change"
    };
    RuleUpdateReport {
        schema: RULE_UPDATE_SCHEMA_V1,
        command: "rule update",
        version: env!("CARGO_PKG_VERSION"),
        status: status.to_owned(),
        rule_id: input.rule_id.to_owned(),
        workspace_id: input.prepared.workspace_id.clone(),
        workspace_path: input.prepared.workspace_path.display().to_string(),
        database_path: input.prepared.database_path.display().to_string(),
        dry_run: input.dry_run,
        persisted: input.persisted,
        changed: input.changed,
        changed_fields: input.changed_fields,
        audit_id: input.audit_id,
        index_job_id: input.index_job_id,
        index_status: index_status.to_owned(),
        previous_rule: input.previous_rule,
        rule: input.rule,
        degraded: Vec::new(),
    }
}

fn prepare_rule_add(options: &RuleAddOptions<'_>) -> Result<PreparedRuleAdd, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let content = MemoryContent::parse(options.content)
        .map_err(|error| rule_usage_error(error.to_string()))?
        .as_str()
        .to_owned();
    if content.len() > MAX_RULE_CONTENT_BYTES {
        return Err(rule_usage_error(format!(
            "rule content is too large: {} bytes > {} bytes",
            content.len(),
            MAX_RULE_CONTENT_BYTES
        )));
    }
    validate_rule_policy(&content)?;

    let scope =
        RuleScope::from_str(options.scope).map_err(|error| rule_usage_error(error.to_string()))?;
    let scope_pattern = prepare_scope_pattern(scope, options.scope_pattern)?;
    let maturity = RuleMaturity::from_str(options.maturity)
        .map_err(|error| rule_usage_error(error.to_string()))?;
    if maturity.is_terminal() {
        return Err(rule_usage_error(
            "`ee rule add` creates active rules; use a lifecycle command to deprecate or supersede"
                .to_owned(),
        ));
    }

    let trust_class = TrustClass::from_str(options.trust_class)
        .map_err(|error| rule_usage_error(error.to_string()))?;
    let source_memory_ids = parse_source_memory_ids(options.source_memory_ids)?;
    if maturity == RuleMaturity::Validated && source_memory_ids.is_empty() {
        return Err(rule_usage_error(
            "validated rules require at least one --source-memory evidence ID".to_owned(),
        ));
    }

    let confidence = match options.confidence {
        Some(value) => UnitScore::parse(value)
            .map_err(|error| rule_usage_error(error.to_string()))?
            .into_inner(),
        None if source_memory_ids.is_empty() => trust_class.initial_confidence().min(0.55),
        None => trust_class.initial_confidence(),
    };
    let utility = UnitScore::parse(options.utility)
        .map_err(|error| rule_usage_error(error.to_string()))?
        .into_inner();
    let importance = UnitScore::parse(options.importance)
        .map_err(|error| rule_usage_error(error.to_string()))?
        .into_inner();
    let tags = parse_tags(options.tags)?;
    let actor = options.actor.map(str::trim).and_then(|actor| {
        if actor.is_empty() {
            None
        } else {
            Some(actor.to_owned())
        }
    });

    Ok(PreparedRuleAdd {
        rule_id: RuleId::now(),
        workspace_id: stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
        content,
        scope,
        scope_pattern,
        maturity,
        trust_class,
        confidence,
        utility,
        importance,
        tags,
        source_memory_ids,
        protected: options.protected,
        actor,
    })
}

fn prepare_scope_pattern(
    scope: RuleScope,
    raw: Option<&str>,
) -> Result<Option<String>, DomainError> {
    let pattern = raw.map(str::trim).filter(|value| !value.is_empty());
    if scope.requires_pattern() && pattern.is_none() {
        return Err(rule_usage_error(format!(
            "scope `{}` requires --scope-pattern",
            scope.as_str()
        )));
    }
    if !scope.requires_pattern() && pattern.is_some() {
        return Err(rule_usage_error(format!(
            "scope `{}` does not accept --scope-pattern",
            scope.as_str()
        )));
    }
    Ok(pattern.map(str::to_owned))
}

fn parse_tags(raw_tags: &[String]) -> Result<Vec<String>, DomainError> {
    let mut unique = BTreeSet::new();
    for tag_arg in raw_tags {
        for raw in tag_arg
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
        {
            let tag = Tag::parse(raw).map_err(|error| rule_usage_error(error.to_string()))?;
            unique.insert(tag.to_string());
        }
    }
    Ok(unique.into_iter().collect())
}

fn parse_source_memory_ids(raw_ids: &[String]) -> Result<Vec<String>, DomainError> {
    let mut unique = BTreeSet::new();
    for id_arg in raw_ids {
        for raw in id_arg.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            let memory_id = MemoryId::from_str(raw)
                .map_err(|error| rule_usage_error(format!("invalid source memory ID: {error}")))?;
            unique.insert(memory_id.to_string());
        }
    }
    Ok(unique.into_iter().collect())
}

fn verify_source_memories(
    connection: &DbConnection,
    workspace_id: &str,
    source_memory_ids: &[String],
) -> Result<(), DomainError> {
    for source_id in source_memory_ids {
        let memory = connection
            .get_memory(source_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query source memory {source_id}: {error}"),
                repair: Some("ee memory show <memory-id> --json".to_owned()),
            })?;
        let Some(memory) = memory else {
            return Err(DomainError::NotFound {
                resource: "source memory".to_owned(),
                id: source_id.clone(),
                repair: Some(
                    "Create or import the evidence memory before adding the rule.".to_owned(),
                ),
            });
        };
        if memory.workspace_id != workspace_id {
            return Err(rule_usage_error(format!(
                "source memory {source_id} belongs to workspace {}, not {}",
                memory.workspace_id, workspace_id
            )));
        }
        if memory.tombstoned_at.is_some() {
            return Err(rule_usage_error(format!(
                "source memory {source_id} is tombstoned and cannot support a new rule"
            )));
        }
    }
    Ok(())
}

fn rule_add_report(
    prepared: &PreparedRuleAdd,
    status: &str,
    persisted: bool,
    audit_id: Option<String>,
    index_job_id: Option<String>,
    verified_evidence: bool,
) -> RuleAddReport {
    let source_memory_count = prepared.source_memory_ids.len();
    let evidence_status = match (source_memory_count, verified_evidence, persisted) {
        (0, _, _) => "missing",
        (_, true, true) => "verified",
        (_, false, false) => "declared_not_verified",
        _ => "declared",
    };
    RuleAddReport {
        schema: RULE_ADD_SCHEMA_V1,
        command: "rule add",
        version: env!("CARGO_PKG_VERSION"),
        status: status.to_owned(),
        rule_id: prepared.rule_id.to_string(),
        workspace_id: prepared.workspace_id.clone(),
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        content: prepared.content.clone(),
        scope: prepared.scope.as_str().to_owned(),
        scope_pattern: prepared.scope_pattern.clone(),
        maturity: prepared.maturity.as_str().to_owned(),
        lifecycle: RuleAddLifecycle {
            initial_maturity: prepared.maturity.as_str().to_owned(),
            is_active: prepared.maturity.is_active(),
            is_terminal: prepared.maturity.is_terminal(),
            next_action: if source_memory_count == 0 {
                "attach evidence with a source memory before promotion".to_owned()
            } else {
                "record outcomes with ee outcome --target-type rule".to_owned()
            },
        },
        trust_class: prepared.trust_class.as_str().to_owned(),
        protected: prepared.protected,
        confidence: prepared.confidence,
        utility: prepared.utility,
        importance: prepared.importance,
        tags: prepared.tags.clone(),
        source_memory_ids: prepared.source_memory_ids.clone(),
        evidence: RuleAddEvidence {
            status: evidence_status.to_owned(),
            source_memory_count,
            verified: verified_evidence,
            requirement: "validated rules require at least one source memory".to_owned(),
        },
        dry_run: !persisted,
        persisted,
        audit_id,
        index_job_id,
        index_status: if persisted {
            "queued".to_owned()
        } else {
            "dry_run_not_queued".to_owned()
        },
        redaction_status: "checked".to_owned(),
        degraded: Vec::new(),
    }
}

fn resolve_workspace_path(path: &Path, dry_run: bool) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    match absolute.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(_error) if dry_run => Ok(absolute),
        Err(error) => Err(DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        }),
    }
}

fn ensure_database_parent_exists(database_path: &Path) -> Result<(), DomainError> {
    let Some(parent) = database_path.parent() else {
        return Ok(());
    };
    if parent.exists() {
        return Ok(());
    }
    Err(DomainError::Storage {
        message: format!("Database directory not found at {}", parent.display()),
        repair: Some("ee init --workspace .".to_owned()),
    })
}

fn ensure_workspace(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
) -> Result<(), DomainError> {
    let path = workspace_path.to_string_lossy().into_owned();
    if connection
        .get_workspace_by_path(&path)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
        .is_some()
    {
        return Ok(());
    }

    connection
        .insert_workspace(
            workspace_id,
            &CreateWorkspaceInput {
                path,
                name: workspace_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to register workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        })
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes()) {
        *target = *source;
    }
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn generate_search_index_job_id() -> String {
    let rule_id = RuleId::now().to_string();
    let payload = rule_id.trim_start_matches("rule_");
    format!("sidx_{payload}")
}

fn rule_add_audit_details(rule_id: &str, input: &CreateProceduralRuleInput) -> String {
    serde_json::json!({
        "schema": "ee.audit.rule_create.v1",
        "command": "ee rule add",
        "ruleId": rule_id,
        "maturity": input.maturity,
        "scope": input.scope,
        "scopePattern": input.scope_pattern,
        "trustClass": input.trust_class,
        "protected": input.protected,
        "confidence": input.confidence,
        "utility": input.utility,
        "importance": input.importance,
        "tagCount": input.tags.len(),
        "sourceMemoryCount": input.source_memory_ids.len(),
    })
    .to_string()
}

fn rule_mark_audit_details(
    rule_id: &str,
    transition: &RuleLifecycleTransition,
    evidence: &RuleLifecycleEvidence,
    changed: bool,
    previous_rule: &RuleDetails,
) -> String {
    serde_json::json!({
        "schema": "ee.audit.rule_mark.v1",
        "command": "ee rule mark",
        "ruleId": rule_id,
        "trigger": transition.trigger.as_str(),
        "action": transition.action.as_str(),
        "priorMaturity": transition.prior_maturity.as_str(),
        "nextMaturity": transition.next_maturity.as_str(),
        "changed": changed,
        "requiresCuration": transition.requires_curation,
        "reason": &transition.reason,
        "previousConfidence": previous_rule.confidence,
        "previousUtility": previous_rule.utility,
        "confidenceDelta": transition.confidence_delta,
        "utilityDelta": transition.utility_delta,
        "evidence": lifecycle_evidence_report(evidence),
    })
    .to_string()
}

fn rule_update_audit_details(
    rule_id: &str,
    changed_fields: &[String],
    previous_rule: &RuleDetails,
    rule: &RuleDetails,
) -> String {
    serde_json::json!({
        "schema": "ee.audit.rule_update.v1",
        "command": "ee rule update",
        "ruleId": rule_id,
        "changedFields": changed_fields,
        "previous": {
            "maturity": &previous_rule.maturity,
            "scope": &previous_rule.scope,
            "scopePattern": &previous_rule.scope_pattern,
            "trustClass": &previous_rule.trust_class,
            "protected": previous_rule.protected,
            "confidence": previous_rule.confidence,
            "utility": previous_rule.utility,
            "importance": previous_rule.importance,
            "tagCount": previous_rule.tags.len(),
            "sourceMemoryCount": previous_rule.source_memory_ids.len(),
        },
        "next": {
            "maturity": &rule.maturity,
            "scope": &rule.scope,
            "scopePattern": &rule.scope_pattern,
            "trustClass": &rule.trust_class,
            "protected": rule.protected,
            "confidence": rule.confidence,
            "utility": rule.utility,
            "importance": rule.importance,
            "tagCount": rule.tags.len(),
            "sourceMemoryCount": rule.source_memory_ids.len(),
        },
    })
    .to_string()
}

fn rule_protect_report(
    prepared: &PreparedRuleRead,
    rule_id: &str,
    protected: bool,
    previous_protected: bool,
    changed: bool,
    dry_run: bool,
    audit_id: Option<String>,
) -> RuleProtectReport {
    RuleProtectReport {
        schema: RULE_PROTECT_SCHEMA_V1,
        command: "rule protect",
        version: env!("CARGO_PKG_VERSION"),
        status: if dry_run {
            "dry_run".to_owned()
        } else if changed {
            "updated".to_owned()
        } else {
            "unchanged".to_owned()
        },
        rule_id: rule_id.to_owned(),
        workspace_id: prepared.workspace_id.clone(),
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        protected,
        previous_protected,
        changed,
        dry_run,
        audit_id,
        degraded: Vec::new(),
    }
}

fn rule_protect_audit_details(rule_id: &str, previous_protected: bool, protected: bool) -> String {
    serde_json::json!({
        "schema": "ee.audit.rule_protect.v1",
        "command": "ee rule protect",
        "ruleId": rule_id,
        "previousProtected": previous_protected,
        "protected": protected,
        "changed": previous_protected != protected,
    })
    .to_string()
}

fn rule_usage_error(message: String) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some("ee rule add --help".to_owned()),
    }
}

fn rule_read_usage_error(message: String, repair: &str) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some(repair.to_owned()),
    }
}

/// Validate that a rule's content is safe to persist.
///
/// Bead bd-17c65.3.1 (C1): the previous implementation rejected on any
/// occurrence of the keywords `password`, `secret`, `token`, etc. as
/// substrings, blocking legitimate meta-policy rules and rules that
/// referenced async cancellation tokens. Replaced with the value-shape
/// detector `policy::redact_secret_like_content` which catches real
/// secret values (API keys, JWTs, PEM blocks, high-entropy tokens)
/// without flagging plain-English mentions.
fn validate_rule_policy(content: &str) -> Result<(), DomainError> {
    let redaction_report = crate::policy::redact_secret_like_content(content);
    if redaction_report.redacted {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing to persist rule content that contains secrets: {}.",
                redaction_report.redacted_reasons.join(", ")
            ),
            repair: Some(
                "Redact the secret and run `ee rule add` again with only durable guidance."
                    .to_owned(),
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    #[test]
    fn playbook_extract_creates_rule_candidate_and_apply_flow_preserves_evidence() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("playbook-extract".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let mut source_ids = Vec::new();
        for seed in 1..=5_u128 {
            let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
            source_ids.push(memory_id.clone());
            connection
                .insert_memory(
                    &memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "semantic".to_owned(),
                        kind: "lesson".to_owned(),
                        content: format!(
                            "Release lesson {seed}: run `cargo fmt --check` before release."
                        ),
                        workflow_id: None,
                        confidence: 0.70,
                        utility: 0.60,
                        importance: 0.55,
                        provenance_uri: None,
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: None,
                        tags: vec!["release".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        connection.close().map_err(|error| error.to_string())?;

        let report = extract_playbook_candidates(&PlaybookExtractOptions {
            workspace_path,
            database_path: Some(&database_path),
            since: None,
            limit: 100,
            dry_run: false,
            actor: Some("test"),
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, PLAYBOOK_EXTRACT_SCHEMA_V1);
        assert_eq!(report.scanned_memory_count, 5);
        assert_eq!(report.candidate_count, 1);
        assert_eq!(report.persisted_count, 1);
        assert_eq!(report.duplicate_count, 0);
        assert_eq!(
            report.candidates[0].proposed_content,
            "Run `cargo fmt --check` from the workspace root before release work on main; if it fails, store the failure with `ee remember --kind failure`."
        );
        assert_eq!(report.candidates[0].source_memory_ids.len(), 5);
        let candidate_id = report.candidates[0]
            .candidate_id
            .clone()
            .ok_or_else(|| "candidate id missing".to_owned())?;

        let validate_report = crate::core::curate::validate_curation_candidate(
            &crate::core::curate::CurateValidateOptions {
                workspace_path,
                database_path: Some(&database_path),
                candidate_id: &candidate_id,
                actor: Some("test"),
                dry_run: false,
            },
        )
        .map_err(|error| error.message())?;
        assert_eq!(
            validate_report.validation.status, "passed",
            "{:?}",
            validate_report.validation.errors
        );

        let apply_report = crate::core::curate::apply_curation_candidate(
            &crate::core::curate::CurateApplyOptions {
                workspace_path,
                database_path: Some(&database_path),
                candidate_id: &candidate_id,
                actor: Some("test"),
                dry_run: false,
            },
        )
        .map_err(|error| error.message())?;
        assert_eq!(apply_report.application.decision, "create_rule");

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let rules = connection
            .list_procedural_rules(&workspace_id, Some("candidate"), Some("workspace"), false)
            .map_err(|error| error.to_string())?;
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].content,
            "Run `cargo fmt --check` from the workspace root before release work on main; if it fails, store the failure with `ee remember --kind failure`."
        );
        let mut expected_sources = source_ids;
        expected_sources.sort();
        let actual_sources = connection
            .get_rule_source_memory_ids(&rules[0].id)
            .map_err(|error| error.to_string())?;
        assert_eq!(actual_sources, expected_sources);
        Ok(())
    }

    #[test]
    fn playbook_import_rejects_rule_count_mismatch_before_apply() -> TestResult {
        let document = PlaybookPortableDocument {
            schema: PLAYBOOK_PORTABLE_SCHEMA_V1.to_owned(),
            exported_at: "2026-05-16T00:00:00Z".to_owned(),
            ee_version: env!("CARGO_PKG_VERSION").to_owned(),
            workspace_id: "wsp_01234567890123456789012345".to_owned(),
            workspace_path: "/source".to_owned(),
            rule_count: 2,
            rules: vec![PlaybookPortableRule {
                source_rule_id: Some("rule_01234567890123456789012345".to_owned()),
                content: "Run cargo fmt --check before release.".to_owned(),
                maturity: RuleMaturity::Candidate.as_str().to_owned(),
                scope: RuleScope::Workspace.as_str().to_owned(),
                scope_pattern: None,
                trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                protected: false,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.6,
                tags: vec!["release".to_owned()],
                source_memory_ids: Vec::new(),
                source_memory_count: 0,
                created_at: Some("2026-05-16T00:00:00Z".to_owned()),
                updated_at: None,
            }],
        };

        let err = match validate_playbook_document(&document) {
            Ok(()) => return Err("mismatched rule_count should reject".to_owned()),
            Err(err) => err,
        };

        ensure(
            matches!(err, DomainError::Import { .. }),
            "expected import error",
        )?;
        ensure(
            err.message().contains("rule_count 2 does not match 1"),
            "error should describe rule_count mismatch",
        )
    }

    #[test]
    fn playbook_import_preflights_all_rules_before_persisting() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join("ee.db");
        let workspace_id = stable_workspace_id(workspace_path);
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("playbook-import-preflight".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let mut valid_rule = PlaybookPortableRule {
            source_rule_id: Some("rule_01234567890123456789012345".to_owned()),
            content: "Run cargo fmt --check before release.".to_owned(),
            maturity: RuleMaturity::Candidate.as_str().to_owned(),
            scope: RuleScope::Workspace.as_str().to_owned(),
            scope_pattern: None,
            trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
            protected: false,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.6,
            tags: vec!["release".to_owned()],
            source_memory_ids: Vec::new(),
            source_memory_count: 0,
            created_at: Some("2026-05-16T00:00:00Z".to_owned()),
            updated_at: None,
        };
        let mut invalid_rule = valid_rule.clone();
        invalid_rule.source_rule_id = Some("rule_22222222222222222222222222".to_owned());
        invalid_rule.content = "Use directory-local release guidance.".to_owned();
        invalid_rule.scope = RuleScope::Directory.as_str().to_owned();
        invalid_rule.scope_pattern = None;
        valid_rule.source_rule_id = Some("rule_11111111111111111111111111".to_owned());
        let document = PlaybookPortableDocument {
            schema: PLAYBOOK_PORTABLE_SCHEMA_V1.to_owned(),
            exported_at: "2026-05-16T00:00:00Z".to_owned(),
            ee_version: env!("CARGO_PKG_VERSION").to_owned(),
            workspace_id: "wsp_01234567890123456789012345".to_owned(),
            workspace_path: "/source".to_owned(),
            rule_count: 2,
            rules: vec![valid_rule, invalid_rule],
        };
        let source_path = workspace_path.join("invalid-playbook.json");
        fs::write(
            &source_path,
            serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let err = match import_playbook(&PlaybookImportOptions {
            workspace_path,
            database_path: Some(&database_path),
            source_path: &source_path,
            dry_run: false,
            actor: Some("test"),
        }) {
            Ok(_) => return Err("invalid later rule should reject import".to_owned()),
            Err(err) => err,
        };

        ensure(
            matches!(err, DomainError::Import { .. }),
            "expected import error",
        )?;
        ensure(
            err.message().contains("scope `directory` requires"),
            "error should include invalid rule reason",
        )?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let rules = connection
            .list_procedural_rules(&workspace_id, None, None, true)
            .map_err(|error| error.to_string())?;
        ensure(
            rules.is_empty(),
            "preflight failure must not persist valid prefix",
        )
    }

    #[cfg(unix)]
    #[test]
    fn playbook_export_rejects_symlinked_output_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_parent = tempdir.path().join("real-parent");
        fs::create_dir_all(&real_parent).map_err(|error| error.to_string())?;
        let linked_parent = tempdir.path().join("linked-parent");
        symlink(&real_parent, &linked_parent).map_err(|error| error.to_string())?;
        let output_path = linked_parent.join("playbook.json");

        let err = match write_side_path_no_overwrite(&output_path, b"{}") {
            Ok(()) => return Err("symlinked output parent should reject".to_owned()),
            Err(err) => err,
        };

        ensure(
            matches!(err, DomainError::PolicyDenied { .. }),
            "expected policy error",
        )?;
        ensure(
            err.message().contains("symbolic link"),
            "error should mention symbolic link",
        )?;
        ensure(
            !real_parent.join("playbook.json").exists(),
            "playbook export must not write through symlinked parent",
        )
    }

    #[cfg(unix)]
    #[test]
    fn playbook_import_rejects_symlinked_source_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_parent = tempdir.path().join("real-parent");
        fs::create_dir_all(&real_parent).map_err(|error| error.to_string())?;
        let source_path = real_parent.join("playbook.json");
        fs::write(
            &source_path,
            b"{\"schema\":\"ee.playbook.portable.v1\",\"ruleCount\":0,\"rules\":[]}",
        )
        .map_err(|error| error.to_string())?;
        let linked_parent = tempdir.path().join("linked-parent");
        symlink(&real_parent, &linked_parent).map_err(|error| error.to_string())?;
        let linked_source = linked_parent.join("playbook.json");

        let err = match import_playbook(&PlaybookImportOptions {
            workspace_path: tempdir.path(),
            database_path: Some(&tempdir.path().join("ee.db")),
            source_path: &linked_source,
            dry_run: true,
            actor: Some("test"),
        }) {
            Ok(report) => {
                return Err(format!(
                    "symlinked import source should reject, got status {}",
                    report.status
                ));
            }
            Err(err) => err,
        };

        ensure(
            matches!(err, DomainError::PolicyDenied { .. }),
            "expected policy error",
        )?;
        ensure(
            err.message().contains("symbolic link"),
            "error should mention symbolic link",
        )
    }

    #[test]
    fn rule_add_dry_run_canonicalizes_tags_and_sources() -> TestResult {
        let source_a = MemoryId::from_uuid(uuid::Uuid::from_u128(2)).to_string();
        let source_b = MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string();
        let tags = vec!["Rust,CI".to_owned(), "rust".to_owned()];
        let sources = vec![source_a.clone(), source_b.clone(), source_a.clone()];
        let report = add_rule(&RuleAddOptions {
            workspace_path: Path::new("."),
            database_path: None,
            content: "Run cargo fmt --check before release.",
            scope: "workspace",
            scope_pattern: None,
            maturity: "candidate",
            confidence: None,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit",
            protected: false,
            tags: &tags,
            source_memory_ids: &sources,
            dry_run: true,
            actor: None,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, RULE_ADD_SCHEMA_V1);
        assert_eq!(report.status, "dry_run");
        assert_eq!(report.tags, vec!["ci".to_owned(), "rust".to_owned()]);
        assert_eq!(report.source_memory_ids, vec![source_b, source_a]);
        assert_eq!(report.evidence.status, "declared_not_verified");
        ensure(!report.persisted, "dry-run must not persist")
    }

    #[test]
    fn rule_add_requires_scope_pattern_for_directory_rules() -> TestResult {
        let err = match add_rule(&RuleAddOptions {
            workspace_path: Path::new("."),
            database_path: None,
            content: "Use scoped rule.",
            scope: "directory",
            scope_pattern: None,
            maturity: "candidate",
            confidence: None,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit",
            protected: false,
            tags: &[],
            source_memory_ids: &[],
            dry_run: true,
            actor: None,
        }) {
            Ok(_) => return Err("directory scope without pattern should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            matches!(err, DomainError::Usage { .. }),
            "expected usage error",
        )
    }

    #[test]
    fn rule_add_rejects_validated_without_evidence() -> TestResult {
        let err = match add_rule(&RuleAddOptions {
            workspace_path: Path::new("."),
            database_path: None,
            content: "A validated rule needs evidence.",
            scope: "workspace",
            scope_pattern: None,
            maturity: "validated",
            confidence: None,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit",
            protected: false,
            tags: &[],
            source_memory_ids: &[],
            dry_run: true,
            actor: None,
        }) {
            Ok(_) => return Err("validated rule without evidence should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            err.message().contains("validated rules require"),
            "error should mention evidence requirement",
        )
    }

    #[test]
    fn rule_list_rejects_zero_limit_before_database_open() -> TestResult {
        let err = match list_rules(&RuleListOptions {
            workspace_path: Path::new("."),
            database_path: Some(Path::new("/definitely/not/ee.db")),
            maturity: None,
            scope: None,
            tag: None,
            include_tombstoned: false,
            limit: 0,
            offset: 0,
        }) {
            Ok(_) => return Err("zero list limit should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            err.message().contains("--limit must be greater than zero"),
            "error should mention limit",
        )
    }

    #[test]
    fn rule_show_rejects_invalid_rule_id_before_database_open() -> TestResult {
        let err = match show_rule(&RuleShowOptions {
            workspace_path: Path::new("."),
            database_path: Some(Path::new("/definitely/not/ee.db")),
            rule_id: "mem_00000000000000000000000001",
            include_tombstoned: false,
        }) {
            Ok(_) => return Err("wrong ID prefix should fail".to_owned()),
            Err(err) => err,
        };

        ensure(
            err.message().contains("invalid rule ID"),
            "error should mention invalid rule ID",
        )
    }
}
