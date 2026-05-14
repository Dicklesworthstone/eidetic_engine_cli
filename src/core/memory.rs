//! Memory retrieval and inspection operations (EE-063, EE-066).
//!
//! Provides the core use case functions for inspecting stored memories:
//! - `get_memory_details`: retrieve a single memory with its tags and metadata
//! - `revise_memory`: create an immutable revision of an existing memory

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use super::index::{
    DEFAULT_INDEX_SUBDIR, IndexProcessingJobReport, process_index_job_for_connection,
};
use super::memory_lifecycle::{
    LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE, LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE,
    LEVEL_TRANSITION_TOMBSTONED_REJECTED_CODE, MemoryLifecycleState, transition_for,
};
use super::search::{SearchOptions, SearchStatus, run_search};
use crate::config::ConfigFile;
use crate::curate::cluster_coherence::{ClusterCoherenceConfig, EmbeddingPoint, agglomerate};
use crate::curate::{CandidateSource, CandidateStatus, CandidateType};
use crate::db::{
    ApplyMemoryLevelTransitionInput, CreateAuditInput, CreateCurationCandidateInput,
    CreateMemoryInput, CreateMemoryLinkInput, CreateSearchIndexJobInput, CreateWorkspaceInput,
    DbConnection, MemoryLinkRelation, MemoryLinkSource, SearchIndexJobType, StoredMemory,
    StoredMemoryLink, audit_actions, generate_audit_id,
};
use crate::models::{
    DomainError, MAX_TAG_BYTES, MemoryContent, MemoryId, MemoryKind, MemoryLevel,
    MemoryValidationError, ProducerMetadata, ProducerSourceSystem, ProvenanceUri, Tag, TrustClass,
    UnitScore, WorkspaceId,
};
use crate::obs::{AuditEvent, AuditOutcome, now_rfc3339_nanos};
use crate::search::HashEmbedder;

/// A memory with its associated tags for display.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDetails {
    /// The stored memory record.
    pub memory: StoredMemory,
    /// Tags associated with this memory.
    pub tags: Vec<String>,
}

/// Options for creating a manual memory through `ee remember`.
#[derive(Clone, Debug)]
pub struct RememberMemoryOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Memory content.
    pub content: &'a str,
    /// Optional workflow lifecycle group.
    pub workflow_id: Option<&'a str>,
    /// Memory level.
    pub level: &'a str,
    /// Memory kind.
    pub kind: &'a str,
    /// Comma-separated tags.
    pub tags: Option<&'a str>,
    /// Confidence score.
    pub confidence: f32,
    /// Optional source provenance URI.
    pub source: Option<&'a str>,
    /// Explicitly allow a secret-detector match while surfacing an audit/degraded signal.
    pub allow_secret_mention: bool,
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<&'a str>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<&'a str>,
    /// Validate and render the write without mutating storage.
    pub dry_run: bool,
    /// Create bounded workflow-local auto-links after a successful write.
    pub auto_link: bool,
    /// Propose a curation candidate after persistence when repeated evidence clusters.
    pub propose_candidates: bool,
}

/// Result of creating a manual memory.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberMemoryReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Created or previewed memory ID.
    pub memory_id: MemoryId,
    /// Canonical workspace ID when resolved.
    pub workspace_id: String,
    /// Canonical workspace path.
    pub workspace_path: PathBuf,
    /// Resolved database path.
    pub database_path: PathBuf,
    /// Canonical memory content.
    pub content: String,
    /// Optional workflow lifecycle group.
    pub workflow_id: Option<String>,
    /// Canonical memory level.
    pub level: MemoryLevel,
    /// Canonical memory kind.
    pub kind: MemoryKind,
    /// Validated confidence score.
    pub confidence: f32,
    /// Canonical tags.
    pub tags: Vec<String>,
    /// Canonical source/provenance URI.
    pub source: Option<String>,
    /// Producer identity metadata for this memory write.
    pub producer: ProducerMetadata,
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<String>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<String>,
    /// Current validity status computed from the stored validity window.
    pub validity_status: String,
    /// Stable shape of the validity window.
    pub validity_window_kind: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether a memory row was persisted.
    pub persisted: bool,
    /// First-version revision number for a newly remembered memory.
    pub revision_number: u32,
    /// Revision group ID once revision tracking is backed by storage.
    pub revision_group_id: Option<String>,
    /// Audit entry created for the write.
    pub audit_id: Option<String>,
    /// Pending index job created for the memory.
    pub index_job_id: Option<String>,
    /// Stable index status for the write.
    pub index_status: String,
    /// Effect IDs once command-effect recording is backed by storage.
    pub effect_ids: Vec<String>,
    /// Staged adjacency suggestions. These do not create durable memory_links rows.
    pub suggested_links: Vec<RememberSuggestedLink>,
    /// Status of suggestion generation.
    pub suggested_link_status: String,
    /// Non-fatal degradations encountered while generating suggestions.
    pub suggested_link_degradations: Vec<RememberSuggestedLinkDegradation>,
    /// Stable redaction/policy status for the accepted content.
    pub redaction_status: String,
    /// Explicit policy-bypass signal when a configured or per-call bypass was used.
    pub policy_bypass: Option<RememberPolicyBypassReport>,
    /// Durable auto-link rows created by remember-time workflow reinforcement.
    pub auto_links: Vec<RememberAutoLink>,
    /// Status of remember-time workflow auto-linking.
    pub auto_link_status: String,
    /// Non-fatal degradations encountered while creating workflow auto-links.
    pub auto_link_degradations: Vec<RememberSuggestedLinkDegradation>,
    /// Curation candidate proposed from this memory's local evidence cluster.
    pub curation_candidate: Option<RememberCurationCandidateProposal>,
    /// Status of remember-time curation proposal.
    pub curation_candidate_status: String,
    /// Non-fatal degradations encountered while proposing curation candidates.
    pub curation_candidate_degradations: Vec<RememberSuggestedLinkDegradation>,
}

fn remember_producer_metadata() -> ProducerMetadata {
    super::memory_scope::current_agent_name().map_or_else(
        || ProducerMetadata::manual_remember(None, None),
        |agent| {
            ProducerMetadata::known_agent(
                ProducerSourceSystem::Cli,
                Some(&agent),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        },
    )
}

/// Options for closing a workflow lifecycle group.
#[derive(Clone, Debug)]
pub struct WorkflowCloseOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Workflow lifecycle group to close.
    pub workflow_id: &'a str,
}

/// Result of closing a workflow lifecycle group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowCloseReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Canonical workspace ID.
    pub workspace_id: String,
    /// Canonical workflow lifecycle group.
    pub workflow_id: String,
    /// Number of working memories promoted to episodic.
    pub promoted_count: u32,
    /// Number of working memories expired instead of promoted.
    pub expired_count: u32,
    /// Promoted memory IDs in deterministic order.
    pub promoted_memory_ids: Vec<String>,
    /// Audit IDs created for promoted memories.
    pub audit_ids: Vec<String>,
}

/// Stable schema for workflow create response.
pub const WORKFLOW_CREATE_SCHEMA_V1: &str = "ee.workflow.create.v1";

/// Options for creating a workflow lifecycle group through `ee workflow create`.
#[derive(Clone, Debug)]
pub struct WorkflowCreateOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Name for the new workflow lifecycle group.
    pub name: &'a str,
    /// Optional description for the workflow.
    pub description: Option<&'a str>,
    /// Preview without creating the workflow record.
    pub dry_run: bool,
}

/// Result of creating a workflow lifecycle group.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCreateReport {
    /// Stable schema identifier for contract tests.
    pub schema: &'static str,
    /// Command that produced this report.
    pub command: &'static str,
    /// Package version for stable output.
    pub version: &'static str,
    /// Canonical workspace ID.
    pub workspace_id: String,
    /// Canonical workspace path.
    pub workspace_path: String,
    /// Database path used.
    pub database_path: String,
    /// Workflow ID (same as name, used as the lifecycle key).
    pub workflow_id: String,
    /// Optional description.
    pub description: Option<String>,
    /// RFC 3339 timestamp when the workflow was created.
    pub created_at: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether the workflow record was persisted.
    pub persisted: bool,
    /// Audit ID for the creation event.
    pub audit_id: Option<String>,
    /// Next action hint for agents.
    pub next_action: String,
}

impl WorkflowCreateReport {
    /// JSON output for machine consumers.
    #[must_use]
    pub fn json_output(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"workflow create","error":"serialization_failed"}}"#,
                WORKFLOW_CREATE_SCHEMA_V1
            )
        })
    }

    /// Human-readable output.
    #[must_use]
    pub fn human_output(&self) -> String {
        let mode = if self.dry_run { "DRY RUN" } else { "CREATED" };
        let mut output = format!("{mode}: workflow `{}`\n\n", self.workflow_id);
        if let Some(desc) = &self.description {
            output.push_str(&format!("  description: {desc}\n"));
        }
        output.push_str(&format!("  workspace: {}\n", self.workspace_id));
        output.push_str(&format!("  created_at: {}\n", self.created_at));
        output.push_str(&format!("  persisted: {}\n", self.persisted));
        output.push_str("\nNext:\n  ");
        output.push_str(&self.next_action);
        output.push('\n');
        output
    }

    /// TOON-formatted output.
    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "WORKFLOW_CREATE|id={}|workspace={}|dry_run={}|persisted={}",
            self.workflow_id, self.workspace_id, self.dry_run, self.persisted
        )
    }
}

/// Stable schema name for remember-time staged link suggestions.
pub const REMEMBER_SUGGESTED_LINK_SCHEMA_V1: &str = "ee.remember.suggested_link.v1";

const REMEMBER_SUGGESTED_LINK_LIMIT: usize = 5;

/// A staged adjacent-memory suggestion returned from `ee remember`.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberSuggestedLink {
    /// Per-item schema for forward-compatible contract tests.
    pub schema: &'static str,
    /// Suggested edge relation.
    pub relation: String,
    /// Existing memory that may be adjacent to the newly remembered memory.
    pub target_memory_id: String,
    /// Deterministic score for ordering and display.
    pub score: f32,
    /// Conservative confidence in the suggestion.
    pub confidence: f32,
    /// Number of evidence features supporting the suggestion.
    pub evidence_count: u32,
    /// Human-readable summary of the evidence.
    pub evidence_summary: String,
    /// Candidate source that produced the suggestion.
    pub source: String,
    /// Canonical tags shared with the newly remembered memory.
    pub matched_tags: Vec<String>,
    /// Explicit next action; no durable link is created automatically.
    pub next_action: String,
}

/// A durable remember-time auto-link created from workflow-local recency.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberAutoLink {
    /// Link row ID.
    pub link_id: String,
    /// Existing memory linked to the newly remembered memory.
    pub target_memory_id: String,
    /// Stored relation used by the graph layer.
    pub relation: String,
    /// Link weight.
    pub weight: f32,
    /// Link source.
    pub source: String,
    /// Audit entry created for the link write.
    pub audit_id: String,
}

/// A durable remember-time curation candidate created from repeated evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberCurationCandidateProposal {
    /// Curation candidate row ID.
    pub candidate_id: String,
    /// Memory IDs that define the deterministic evidence cluster.
    pub member_memory_ids: Vec<String>,
    /// Memory this candidate targets for review.
    pub target_memory_id: String,
    /// Candidate type.
    pub candidate_type: String,
    /// Audit entry created for the candidate write.
    pub audit_id: Option<String>,
    /// Human-readable proposal reason.
    pub reason: String,
}

/// Non-fatal remember suggestion degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RememberSuggestedLinkDegradation {
    /// Stable machine code.
    pub code: String,
    /// Severity string.
    pub severity: String,
    /// Human-readable message.
    pub message: String,
    /// Suggested repair action.
    pub repair: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RememberPolicyBypassMatch {
    pub kind: String,
    pub pattern: String,
    pub matched_text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RememberPolicyBypassReport {
    pub code: String,
    pub severity: String,
    pub kind: String,
    pub message: String,
    pub repair: String,
    pub redacted_reasons: Vec<String>,
    pub matches: Vec<RememberPolicyBypassMatch>,
    pub audit_id: Option<String>,
}

impl RememberPolicyBypassReport {
    fn degradation(
        kind: impl Into<String>,
        redacted_reasons: Vec<String>,
        matches: Vec<RememberPolicyBypassMatch>,
    ) -> Self {
        let kind = kind.into();
        let message = match kind.as_str() {
            "flag" => "Secret-like content persisted because --allow-secret-mention was used.",
            "config_phrase" | "config_regex" | "config" => {
                "Secret-like content persisted because workspace secret-detector allow config matched."
            }
            _ => "Secret-like content persisted through an explicit policy bypass.",
        };
        Self {
            code: "policy_bypass_used".to_owned(),
            severity: "info".to_owned(),
            kind,
            message: message.to_owned(),
            repair: "Review the memory and its audit row before relying on this content."
                .to_owned(),
            redacted_reasons,
            matches,
            audit_id: None,
        }
    }

    fn with_audit_id(mut self, audit_id: String) -> Self {
        self.audit_id = Some(audit_id);
        self
    }
}

/// Create a manual memory and publish its single-document index job.
///
/// Dry-run mode validates and returns the canonical record shape without
/// opening or mutating storage.
pub fn remember_memory(
    options: &RememberMemoryOptions<'_>,
) -> Result<RememberMemoryReport, DomainError> {
    let prepared = prepare_remember_memory(options)?;
    if options.dry_run {
        return Ok(RememberMemoryReport {
            version: env!("CARGO_PKG_VERSION"),
            memory_id: prepared.memory_id,
            workspace_id: prepared.workspace_id,
            workspace_path: prepared.workspace_path,
            database_path: prepared.database_path,
            content: prepared.content,
            workflow_id: prepared.workflow_id,
            level: prepared.level,
            kind: prepared.kind,
            confidence: prepared.confidence,
            tags: prepared.tags,
            source: prepared.provenance_uri,
            producer: remember_producer_metadata(),
            valid_from: prepared.valid_from,
            valid_to: prepared.valid_to,
            validity_status: prepared.validity_status,
            validity_window_kind: prepared.validity_window_kind,
            dry_run: true,
            persisted: false,
            revision_number: 1,
            revision_group_id: None,
            audit_id: None,
            index_job_id: None,
            index_status: "dry_run_not_queued".to_owned(),
            effect_ids: Vec::new(),
            suggested_links: Vec::new(),
            suggested_link_status: "dry_run_not_evaluated".to_owned(),
            suggested_link_degradations: Vec::new(),
            redaction_status: "checked".to_owned(),
            policy_bypass: prepared.policy_bypass,
            auto_links: Vec::new(),
            auto_link_status: "dry_run_not_evaluated".to_owned(),
            auto_link_degradations: Vec::new(),
            curation_candidate: None,
            curation_candidate_status: "dry_run_not_evaluated".to_owned(),
            curation_candidate_degradations: Vec::new(),
        });
    }

    let mut write_replay_guard = RememberWriteReplayGuard::arm(&prepared.workspace_path)?;

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

    let memory_id = prepared.memory_id.to_string();
    let audit_id = generate_audit_id();
    let policy_bypass_audit_id = prepared.policy_bypass.as_ref().map(|_| generate_audit_id());
    let index_job_id = generate_search_index_job_id();
    let memory_input = CreateMemoryInput {
        workspace_id: prepared.workspace_id.clone(),
        level: prepared.level.as_str().to_owned(),
        kind: prepared.kind.as_str().to_owned(),
        content: prepared.content.clone(),
        workflow_id: prepared.workflow_id.clone(),
        confidence: prepared.confidence,
        utility: UnitScore::neutral().into_inner(),
        importance: UnitScore::neutral().into_inner(),
        provenance_uri: prepared.provenance_uri.clone(),
        trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
        trust_subclass: super::memory_scope::remember_trust_subclass("ee remember"),
        tags: prepared.tags.clone(),
        valid_from: prepared.valid_from.clone(),
        valid_to: prepared.valid_to.clone(),
    };
    let policy_bypass = prepared
        .policy_bypass
        .clone()
        .zip(policy_bypass_audit_id)
        .map(|(bypass, audit_id)| bypass.with_audit_id(audit_id));
    let audit_details = remember_audit_details(&memory_id, &memory_input, policy_bypass.as_ref());
    let index_input = CreateSearchIndexJobInput {
        workspace_id: prepared.workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(memory_id.clone()),
        documents_total: 1,
    };

    store_remembered_memory_with_retry(
        &connection,
        &memory_id,
        &audit_id,
        &index_job_id,
        &memory_input,
        &audit_details,
        &index_input,
        policy_bypass.as_ref(),
    )?;

    append_remember_audit_jsonl(&prepared, &audit_id, &memory_id, &memory_input)?;

    let (auto_links, auto_link_status, auto_link_degradations) =
        match create_auto_links_for_remember(
            &connection,
            &prepared.workspace_id,
            &memory_id,
            prepared.workflow_id.as_deref(),
            options.auto_link,
        ) {
            Ok(auto_links) => {
                let status = auto_link_status(
                    prepared.workflow_id.as_deref(),
                    options.auto_link,
                    &auto_links,
                );
                // G7 (bd-17c65.7.6): commit to honest-unimplemented for
                // the workflow-less case. When no workflow_id is provided
                // we cannot meaningfully auto-link — surface that as a
                // non-failure info degraded entry pointing at the
                // explicit `ee memory link` path.
                let degradations = if status == "no_workflow_required" {
                    vec![RememberSuggestedLinkDegradation {
                        code: "auto_link_disabled".to_owned(),
                        severity: "info".to_owned(),
                        message:
                            "Automatic memory linking requires a workflow context. Use `ee memory link <from> <to> --relation <type>` to add explicit links."
                                .to_owned(),
                        repair: "ee memory link --help".to_owned(),
                    }]
                } else {
                    Vec::new()
                };
                (auto_links, status.to_owned(), degradations)
            }
            Err(error) => (
                Vec::new(),
                "degraded".to_owned(),
                vec![RememberSuggestedLinkDegradation {
                    code: "remember_auto_link_failed".to_owned(),
                    severity: "low".to_owned(),
                    message: format!(
                        "Remembered the memory, but workflow auto-linking failed: {}",
                        error.message()
                    ),
                    repair: "Run `ee doctor --json` and inspect memory link indexes.".to_owned(),
                }],
            ),
        };

    let (suggested_links, suggested_link_status, suggested_link_degradations) =
        match suggest_links_for_remember(
            &connection,
            &prepared.workspace_id,
            &memory_id,
            &prepared.tags,
        ) {
            Ok(suggested_links) => {
                let status = if suggested_links.is_empty() {
                    "no_candidates"
                } else {
                    "ready"
                };
                (suggested_links, status.to_owned(), Vec::new())
            }
            Err(error) => (
                Vec::new(),
                "degraded".to_owned(),
                vec![RememberSuggestedLinkDegradation {
                    code: "remember_link_suggestion_failed".to_owned(),
                    severity: "low".to_owned(),
                    message: format!(
                        "Remembered the memory, but link suggestions failed: {}",
                        error.message()
                    ),
                    repair: "Run `ee doctor --json` and inspect memory tag/link indexes."
                        .to_owned(),
                }],
            ),
        };

    let index_dir = prepared
        .workspace_path
        .join(".ee")
        .join(DEFAULT_INDEX_SUBDIR);
    let index_report = process_index_job_for_connection(&connection, &index_job_id, &index_dir)
        .map_err(|error| DomainError::SearchIndex {
            message: format!("Remembered memory but failed to publish search index: {error}"),
            repair: Some("ee index rebuild --workspace .".to_owned()),
        })?;
    let index_status = remember_index_status(&index_report);

    let (curation_candidate, curation_candidate_status, curation_candidate_degradations) =
        match propose_curation_candidate_for_remember(
            &connection,
            &prepared,
            &memory_id,
            &memory_input,
            options.propose_candidates,
        ) {
            Ok(report) => (
                report.candidate,
                report.status.to_owned(),
                report.degradations,
            ),
            Err(error) => (
                None,
                "degraded".to_owned(),
                vec![RememberSuggestedLinkDegradation {
                    code: "auto_propose_failed".to_owned(),
                    severity: "low".to_owned(),
                    message: format!(
                        "Remembered the memory, but curation candidate proposal failed: {}",
                        error.message()
                    ),
                    repair: "Run `ee curate candidates --json` and inspect the review queue."
                        .to_owned(),
                }],
            ),
        };

    write_replay_guard.mark_clean()?;

    Ok(RememberMemoryReport {
        version: env!("CARGO_PKG_VERSION"),
        memory_id: prepared.memory_id,
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path,
        database_path: prepared.database_path,
        content: prepared.content,
        workflow_id: prepared.workflow_id,
        level: prepared.level,
        kind: prepared.kind,
        confidence: prepared.confidence,
        tags: prepared.tags,
        source: prepared.provenance_uri,
        producer: remember_producer_metadata(),
        valid_from: prepared.valid_from,
        valid_to: prepared.valid_to,
        validity_status: prepared.validity_status,
        validity_window_kind: prepared.validity_window_kind,
        dry_run: false,
        persisted: true,
        revision_number: 1,
        revision_group_id: None,
        audit_id: Some(audit_id),
        index_job_id: Some(index_job_id),
        index_status,
        effect_ids: Vec::new(),
        suggested_links,
        suggested_link_status,
        suggested_link_degradations,
        redaction_status: "checked".to_owned(),
        policy_bypass,
        auto_links,
        auto_link_status,
        auto_link_degradations,
        curation_candidate,
        curation_candidate_status,
        curation_candidate_degradations,
    })
}

/// Close a workflow and promote eligible working memories to episodic.
pub fn close_workflow(
    options: &WorkflowCloseOptions<'_>,
) -> Result<WorkflowCloseReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, false)?;
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let workflow_id = parse_workflow_id(Some(options.workflow_id))?
        .ok_or_else(|| remember_usage_error("workflow id cannot be empty".to_owned()))?;
    let workspace_id = stable_workspace_id(&workspace_path);
    let closed_at = Utc::now().to_rfc3339();

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee init --workspace .".to_string()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;

    let promotions = connection
        .promote_workflow_working_memories_audited(
            &workspace_id,
            &workflow_id,
            "ee workflow close",
            &closed_at,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to close workflow: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let promoted_count = capped_u32(promotions.len());
    let promoted_memory_ids = promotions
        .iter()
        .map(|promotion| promotion.memory_id.clone())
        .collect();
    let audit_ids = promotions
        .into_iter()
        .map(|promotion| promotion.audit_id)
        .collect();

    Ok(WorkflowCloseReport {
        version: env!("CARGO_PKG_VERSION"),
        workspace_id,
        workflow_id,
        promoted_count,
        expired_count: 0,
        promoted_memory_ids,
        audit_ids,
    })
}

/// Create a new workflow lifecycle group.
///
/// Workflows are lightweight lifecycle markers that group related memories.
/// They are created explicitly (this function) or implicitly when using
/// `ee remember --workflow <name>`. This function is idempotent: creating
/// a workflow that already has memories is a no-op success.
pub fn create_workflow(
    options: &WorkflowCreateOptions<'_>,
) -> Result<WorkflowCreateReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let workflow_id = parse_workflow_id(Some(options.name))?
        .ok_or_else(|| remember_usage_error("workflow name cannot be empty".to_owned()))?;
    let workspace_id = stable_workspace_id(&workspace_path);
    let created_at = Utc::now().to_rfc3339();
    let description = options.description.map(str::to_owned);

    let next_action = format!(
        "ee remember --workflow {} \"<content>\" --level working",
        workflow_id
    );

    if options.dry_run {
        return Ok(WorkflowCreateReport {
            schema: WORKFLOW_CREATE_SCHEMA_V1,
            command: "workflow create",
            version: env!("CARGO_PKG_VERSION"),
            workspace_id,
            workspace_path: workspace_path.display().to_string(),
            database_path: database_path.display().to_string(),
            workflow_id,
            description,
            created_at,
            dry_run: true,
            persisted: false,
            audit_id: None,
            next_action,
        });
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee init --workspace .".to_string()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;

    let audit_id = generate_audit_id();
    let details = serde_json::json!({
        "workflow_id": workflow_id,
        "description": description,
        "created_at": created_at,
    })
    .to_string();
    let audit_input = CreateAuditInput {
        workspace_id: Some(workspace_id.clone()),
        actor: None,
        action: audit_actions::WORKFLOW_CREATE.to_string(),
        target_type: Some("workflow".to_string()),
        target_id: Some(workflow_id.clone()),
        details: Some(details),
    };

    connection
        .insert_audit(&audit_id, &audit_input)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to create audit record: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    Ok(WorkflowCreateReport {
        schema: WORKFLOW_CREATE_SCHEMA_V1,
        command: "workflow create",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id,
        workspace_path: workspace_path.display().to_string(),
        database_path: database_path.display().to_string(),
        workflow_id,
        description,
        created_at,
        dry_run: false,
        persisted: true,
        audit_id: Some(audit_id),
        next_action,
    })
}

fn remember_index_status(report: &IndexProcessingJobReport) -> String {
    match report.outcome.as_str() {
        "completed" | "completed_no_documents" => "indexed".to_owned(),
        "skipped" => "queued".to_owned(),
        "failed" => "failed".to_owned(),
        other => other.to_owned(),
    }
}

#[derive(Clone, Debug)]
struct PreparedRememberMemory {
    memory_id: MemoryId,
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
    content: String,
    workflow_id: Option<String>,
    level: MemoryLevel,
    kind: MemoryKind,
    confidence: f32,
    tags: Vec<String>,
    provenance_uri: Option<String>,
    policy_bypass: Option<RememberPolicyBypassReport>,
    valid_from: Option<String>,
    valid_to: Option<String>,
    validity_status: String,
    validity_window_kind: String,
}

struct RememberWriteReplayGuard {
    workspace_path: PathBuf,
    armed: bool,
}

impl RememberWriteReplayGuard {
    fn arm(workspace_path: &Path) -> Result<Self, DomainError> {
        super::write_owner::mark_write_replay_required(workspace_path).map_err(|error| {
            DomainError::Storage {
                message: format!("Failed to record write-spool recovery marker: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            }
        })?;
        Ok(Self {
            workspace_path: workspace_path.to_path_buf(),
            armed: true,
        })
    }

    fn mark_clean(&mut self) -> Result<(), DomainError> {
        super::write_owner::mark_write_replay_clean(&self.workspace_path).map_err(|error| {
            DomainError::Storage {
                message: format!("Failed to clear write-spool recovery marker: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            }
        })?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for RememberWriteReplayGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = super::write_owner::mark_write_replay_clean(&self.workspace_path);
        }
    }
}

fn prepare_remember_memory(
    options: &RememberMemoryOptions<'_>,
) -> Result<PreparedRememberMemory, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let content = MemoryContent::parse(options.content)
        .map_err(|error| remember_usage_error(error.to_string()))?
        .as_str()
        .to_owned();
    let policy_bypass =
        validate_remember_policy(&content, &workspace_path, options.allow_secret_mention)?;
    let workflow_id = parse_workflow_id(options.workflow_id)?;
    let level = MemoryLevel::from_str(options.level)
        .map_err(|error| remember_usage_error(error.to_string()))?;
    let kind = MemoryKind::from_str(options.kind)
        .map_err(|error| remember_usage_error(error.to_string()))?;
    let confidence = UnitScore::parse(options.confidence)
        .map_err(|error| remember_usage_error(error.to_string()))?
        .into_inner();
    let tags = parse_tags(options.tags)?;
    let provenance_uri = options
        .source
        .map(|source| {
            ProvenanceUri::from_str(source)
                .map(|uri| uri.to_string())
                .map_err(|error| remember_usage_error(format!("invalid provenance URI: {error}")))
        })
        .transpose()?;
    let validity = prepare_validity_window(options.valid_from, options.valid_to)?;

    Ok(PreparedRememberMemory {
        memory_id: MemoryId::now(),
        workspace_id: stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
        content,
        workflow_id,
        level,
        kind,
        confidence,
        tags,
        provenance_uri,
        policy_bypass,
        valid_from: validity.valid_from,
        valid_to: validity.valid_to,
        validity_status: validity.status,
        validity_window_kind: validity.window_kind,
    })
}

#[derive(Clone, Debug, PartialEq)]
struct PreparedValidityWindow {
    valid_from: Option<String>,
    valid_to: Option<String>,
    status: String,
    window_kind: String,
}

/// Stable validity metadata derived from a memory's validity window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryValidity {
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<String>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<String>,
    /// Current status: unknown, current, future, expired, or invalid.
    pub status: String,
    /// Window shape: unbounded, starts_at, ends_at, bounded, or instant.
    pub window_kind: String,
}

/// Stable freshness state for evidence referenced by a memory provenance URI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceFreshnessStatus {
    /// The referenced source still appears to contain the remembered evidence.
    Fresh,
    /// The referenced source file no longer exists.
    MissingSource,
    /// The referenced source exists but no longer contains the remembered evidence.
    ChangedSource,
    /// The referenced source exists but cannot be read.
    UnreachableSource,
    /// The provenance scheme is valid but cannot be freshness-checked locally.
    UnsupportedSource,
    /// No checkable provenance was available.
    Unknown,
}

impl EvidenceFreshnessStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::MissingSource => "missing_source",
            Self::ChangedSource => "changed_source",
            Self::UnreachableSource => "unreachable_source",
            Self::UnsupportedSource => "unsupported_source",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn should_report(self) -> bool {
        !matches!(self, Self::Fresh | Self::Unknown)
    }
}

/// Result of checking a memory's provenance against the current workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceFreshness {
    /// Stable freshness status.
    pub status: EvidenceFreshnessStatus,
    /// Canonical provenance URI being checked, when one exists.
    pub provenance_uri: Option<String>,
    /// Human-readable summary safe for degraded arrays and provenance notes.
    pub detail: String,
    /// Suggested repair when the state is actionable.
    pub repair: Option<String>,
}

/// Compute stable display metadata for stored validity timestamps.
#[must_use]
pub fn memory_validity(valid_from: &Option<String>, valid_to: &Option<String>) -> MemoryValidity {
    let parsed_from = valid_from
        .as_deref()
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc));
    let parsed_to = valid_to
        .as_deref()
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc));
    let status = match (
        valid_from.as_ref(),
        valid_to.as_ref(),
        parsed_from,
        parsed_to,
    ) {
        (Some(_), _, None, _) | (_, Some(_), _, None) => "invalid",
        (_, _, from, to) => classify_validity_status(from, to),
    };

    MemoryValidity {
        valid_from: valid_from.clone(),
        valid_to: valid_to.clone(),
        status: status.to_owned(),
        window_kind: validity_window_kind(valid_from.as_deref(), valid_to.as_deref()).to_owned(),
    }
}

/// Check whether a memory's explicit provenance still supports its content.
#[must_use]
pub fn assess_memory_evidence_freshness(
    memory: &StoredMemory,
    workspace_path: Option<&Path>,
) -> EvidenceFreshness {
    let Some(raw_provenance) = memory.provenance_uri.as_deref() else {
        return EvidenceFreshness {
            status: EvidenceFreshnessStatus::Unknown,
            provenance_uri: None,
            detail: "Memory has no explicit provenance URI to freshness-check.".to_owned(),
            repair: None,
        };
    };

    let provenance = match ProvenanceUri::from_str(raw_provenance) {
        Ok(provenance) => provenance,
        Err(error) => {
            return EvidenceFreshness {
                status: EvidenceFreshnessStatus::Unknown,
                provenance_uri: Some(raw_provenance.to_owned()),
                detail: format!("Memory provenance URI could not be parsed: {error}."),
                repair: Some("Revise the memory with a valid provenance URI.".to_owned()),
            };
        }
    };

    match &provenance {
        ProvenanceUri::File { path, span } => {
            let source_path = resolve_provenance_file_path(path, workspace_path);
            let canonical_uri = provenance.to_string();
            let source_text = match fs::read_to_string(&source_path) {
                Ok(contents) => match span {
                    Some(_) => extract_line_span(&contents, *span).unwrap_or_default(),
                    None => contents,
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return EvidenceFreshness {
                        status: EvidenceFreshnessStatus::MissingSource,
                        provenance_uri: Some(canonical_uri),
                        detail: format!(
                            "Referenced provenance file {} is missing.",
                            source_path.display()
                        ),
                        repair: Some(
                            "Restore the file or revise the memory provenance URI; rebuild the index if the memory content changes."
                                .to_owned(),
                        ),
                    };
                }
                Err(error) => {
                    return EvidenceFreshness {
                        status: EvidenceFreshnessStatus::UnreachableSource,
                        provenance_uri: Some(canonical_uri),
                        detail: format!(
                            "Referenced provenance file {} could not be read: {error}.",
                            source_path.display()
                        ),
                        repair: Some(
                            "Fix file permissions or revise the memory provenance URI.".to_owned(),
                        ),
                    };
                }
            };

            if evidence_text_matches(&source_text, &memory.content) {
                EvidenceFreshness {
                    status: EvidenceFreshnessStatus::Fresh,
                    provenance_uri: Some(canonical_uri),
                    detail: format!(
                        "Referenced provenance file {} still contains the remembered evidence.",
                        source_path.display()
                    ),
                    repair: None,
                }
            } else {
                EvidenceFreshness {
                    status: EvidenceFreshnessStatus::ChangedSource,
                    provenance_uri: Some(canonical_uri),
                    detail: format!(
                        "Referenced provenance file {} no longer contains the remembered evidence.",
                        source_path.display()
                    ),
                    repair: Some(
                        "Inspect the source, then re-remember or revise this memory if needed; rebuild the index if the remembered content changes."
                            .to_owned(),
                    ),
                }
            }
        }
        ProvenanceUri::CassSession { .. }
        | ProvenanceUri::EeMemory(_)
        | ProvenanceUri::Web { .. }
        | ProvenanceUri::AgentMail { .. } => EvidenceFreshness {
            status: EvidenceFreshnessStatus::UnsupportedSource,
            provenance_uri: Some(provenance.to_string()),
            detail: format!(
                "Provenance scheme `{}` cannot be freshness-checked by the local file verifier.",
                provenance.scheme()
            ),
            repair: Some(
                "Re-import the source or attach file:// provenance when local freshness is required."
                    .to_owned(),
            ),
        },
    }
}

fn resolve_provenance_file_path(path: &str, workspace_path: Option<&Path>) -> PathBuf {
    let source_path = PathBuf::from(path);
    if source_path.is_absolute() {
        source_path
    } else {
        workspace_path
            .map(|workspace| workspace.join(source_path.as_path()))
            .unwrap_or(source_path)
    }
}

fn extract_line_span(contents: &str, span: Option<crate::models::LineSpan>) -> Option<String> {
    let span = span?;
    let start = usize::try_from(span.start.saturating_sub(1)).ok()?;
    let end = span.end.unwrap_or(span.start);
    let count = usize::try_from(end.saturating_sub(span.start).saturating_add(1)).ok()?;
    let lines = contents.lines().skip(start).take(count).collect::<Vec<_>>();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn evidence_text_matches(source_text: &str, memory_content: &str) -> bool {
    let source_text = source_text.trim();
    let memory_content = memory_content.trim();
    if source_text.is_empty() || memory_content.is_empty() {
        return false;
    }
    source_text.contains(memory_content) || memory_content.contains(source_text)
}

fn prepare_validity_window(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> Result<PreparedValidityWindow, DomainError> {
    let parsed_from = parse_validity_timestamp("valid_from", valid_from)?;
    let parsed_to = parse_validity_timestamp("valid_to", valid_to)?;

    if let (Some(from), Some(to)) = (parsed_from.as_ref(), parsed_to.as_ref()) {
        if from > to {
            return Err(remember_usage_error(
                "valid_from must be less than or equal to valid_to".to_owned(),
            ));
        }
    }

    let valid_from = parsed_from.map(normalize_validity_timestamp);
    let valid_to = parsed_to.map(normalize_validity_timestamp);

    Ok(PreparedValidityWindow {
        status: memory_validity(&valid_from, &valid_to).status,
        window_kind: validity_window_kind(valid_from.as_deref(), valid_to.as_deref()).to_owned(),
        valid_from,
        valid_to,
    })
}

fn parse_validity_timestamp(
    field_name: &str,
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, DomainError> {
    value
        .map(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(remember_usage_error(format!(
                    "{field_name} must be a non-empty RFC3339 timestamp"
                )));
            }
            DateTime::parse_from_rfc3339(trimmed)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|error| {
                    remember_usage_error(format!(
                        "{field_name} must be an RFC3339 timestamp: {error}"
                    ))
                })
        })
        .transpose()
}

fn normalize_validity_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn classify_validity_status(
    valid_from: Option<DateTime<Utc>>,
    valid_to: Option<DateTime<Utc>>,
) -> &'static str {
    match (valid_from, valid_to) {
        (None, None) => "unknown",
        (from, to) => {
            let now = Utc::now();
            if from.is_some_and(|timestamp| now < timestamp) {
                "future"
            } else if to.is_some_and(|timestamp| now > timestamp) {
                "expired"
            } else {
                "current"
            }
        }
    }
}

fn validity_window_kind(valid_from: Option<&str>, valid_to: Option<&str>) -> &'static str {
    match (valid_from, valid_to) {
        (None, None) => "unbounded",
        (Some(from), Some(to)) if from == to => "instant",
        (Some(_), Some(_)) => "bounded",
        (Some(_), None) => "starts_at",
        (None, Some(_)) => "ends_at",
    }
}

fn parse_tags(tags: Option<&str>) -> Result<Vec<String>, DomainError> {
    let mut unique = BTreeSet::new();
    if let Some(tags) = tags {
        for raw in tags.split(',').map(str::trim).filter(|tag| !tag.is_empty()) {
            let tag = Tag::parse(raw).map_err(|error| remember_tag_usage_error(raw, &error))?;
            unique.insert(tag.to_string());
        }
    }
    Ok(unique.into_iter().collect())
}

fn remember_tag_usage_error(raw: &str, error: &MemoryValidationError) -> DomainError {
    let normalized_candidate = normalize_tag_candidate(raw);
    let rejected = tag_rejection_matches(raw, error);
    let details = serde_json::json!({
        "detailCode": "policy_tag_rejected_with_details",
        "rejectedKind": "tag",
        "tag": raw,
        "rejectedInput": raw,
        "acceptedPattern": r"^[\p{Alphabetic}\p{Mark}\p{Number}._:-]{1,64}$",
        "acceptedExamples": ["release", "v0.1.0", "policy.detector", "security:auth-bypass"],
        "matchedAt": rejected,
        "normalizedFormCandidate": normalized_candidate,
        "maxBytes": MAX_TAG_BYTES,
    });
    DomainError::UsageWithDetails {
        message: match error {
            MemoryValidationError::InvalidTag { .. } => {
                format!("tag `{raw}` contains characters outside the accepted set.")
            }
            MemoryValidationError::EmptyTag => "tag cannot be empty.".to_owned(),
            MemoryValidationError::TagTooLong { limit, .. } => {
                format!("tag `{raw}` exceeds the {limit}-byte limit.")
            }
            other => other.to_string(),
        },
        repair: Some(
            "Use only accepted tag characters, for example `v0.1.0` or `policy.detector`."
                .to_owned(),
        ),
        details_json: details.to_string(),
    }
}

fn normalize_tag_candidate(input: &str) -> String {
    unicode_normalization::UnicodeNormalization::nfc(input.trim())
        .map(|ch| {
            if ch.is_ascii_uppercase() {
                ch.to_ascii_lowercase()
            } else {
                ch
            }
        })
        .collect()
}

fn previous_char_boundary(input: &str, mut index: usize) -> usize {
    while index > 0 && !input.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn tag_rejection_matches(input: &str, error: &MemoryValidationError) -> Vec<serde_json::Value> {
    match error {
        MemoryValidationError::EmptyTag => vec![serde_json::json!({
            "start": 0,
            "end": 0,
            "reason": "empty",
        })],
        MemoryValidationError::TagTooLong { .. } => {
            let start = previous_char_boundary(input, MAX_TAG_BYTES.min(input.len()));
            vec![serde_json::json!({
                "start": start,
                "end": input.len(),
                "reason": "too_long",
            })]
        }
        MemoryValidationError::InvalidTag { .. } => input
            .char_indices()
            .filter_map(|(start, ch)| {
                tag_rejection_reason(ch).map(|reason| {
                    serde_json::json!({
                        "start": start,
                        "end": start + ch.len_utf8(),
                        "reason": reason,
                    })
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn tag_rejection_reason(ch: char) -> Option<&'static str> {
    if ch.is_whitespace() {
        Some("space_disallowed")
    } else if ch.is_control() {
        Some("control_disallowed")
    } else if ch.is_ascii() {
        if matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | ':' | '-') {
            None
        } else if matches!(
            ch,
            ',' | '='
                | '/'
                | '\\'
                | ';'
                | '*'
                | '?'
                | '|'
                | '<'
                | '>'
                | '"'
                | '\''
                | '`'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '@'
                | '#'
                | '$'
                | '%'
                | '^'
                | '&'
                | '+'
                | '~'
        ) {
            Some("reserved_delimiter")
        } else {
            Some("symbol_disallowed")
        }
    } else if ch.is_alphanumeric()
        || matches!(
            unicode_normalization::char::canonical_combining_class(ch),
            1..=255
        )
    {
        None
    } else {
        Some("unicode_disallowed")
    }
}

fn parse_workflow_id(workflow_id: Option<&str>) -> Result<Option<String>, DomainError> {
    let Some(raw) = workflow_id else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(remember_usage_error(
            "workflow id cannot be empty".to_owned(),
        ));
    }
    if trimmed.len() > 128 {
        return Err(remember_usage_error(
            "workflow id must be at most 128 bytes".to_owned(),
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

/// Validate that a memory's content is safe to persist.
///
/// Bead bd-17c65.3.1 (C1): the previous implementation also rejected any
/// content containing the keywords `password`, `secret`, `token`,
/// `credential`, etc. as substrings. This blocked legitimate meta-policy
/// memories like "context packs must never include secrets" and async-
/// runtime memories that mentioned "cancel token". The value-shape
/// detector (`policy::redact_secret_like_content`) already catches real
/// secret VALUES (API keys, JWTs, PEM blocks, high-entropy tokens) without
/// flagging plain-English mentions. The keyword fallthrough is removed.
fn validate_remember_policy(
    content: &str,
    workspace_path: &Path,
    allow_secret_mention: bool,
) -> Result<Option<RememberPolicyBypassReport>, DomainError> {
    let redaction_report = crate::policy::redact_secret_like_content(content);
    if !redaction_report.redacted {
        return Ok(None);
    }

    let secret_matches = redaction_report.matches;
    let mut redacted_reasons = redaction_report
        .redacted_reasons
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    redacted_reasons.sort_unstable();
    redacted_reasons.dedup();

    let allow_config = load_secret_detector_allow_config(workspace_path)?;
    let configured_matches = secret_detector_allow_matches(content, &allow_config)?;
    if !configured_matches.is_empty() {
        let masked = mask_allow_match_spans(content, &configured_matches);
        let masked_report = crate::policy::redact_secret_like_content(&masked);
        if !masked_report.redacted {
            let kind = configured_bypass_kind(&configured_matches);
            return Ok(Some(RememberPolicyBypassReport::degradation(
                kind,
                redacted_reasons,
                configured_matches,
            )));
        }
    }

    if allow_secret_mention {
        return Ok(Some(RememberPolicyBypassReport::degradation(
            "flag",
            redacted_reasons,
            Vec::new(),
        )));
    }

    Err(remember_secret_policy_denied_error(
        redacted_reasons,
        &secret_matches,
    ))
}

fn remember_secret_policy_denied_error(
    redacted_reasons: Vec<String>,
    matches: &[crate::policy::SecretRedactionMatch],
) -> DomainError {
    let matched_at = matches
        .iter()
        .map(|matched| {
            serde_json::json!({
                "start": matched.start,
                "end": matched.end,
                "pattern_id": matched.pattern_id,
            })
        })
        .collect::<Vec<_>>();
    let detected_patterns = {
        let mut patterns = redacted_reasons.clone();
        patterns.sort_unstable();
        patterns.dedup();
        patterns
    };
    let detected_pattern = detected_patterns
        .first()
        .cloned()
        .unwrap_or_else(|| "secret_like_value".to_owned());
    let details = serde_json::json!({
        "detailCode": "policy_secret_detected_with_offsets",
        "rejectedKind": "content",
        "detectedPattern": detected_pattern,
        "detectedPatterns": detected_patterns,
        "matchedAt": matched_at,
        "bypassFlag": "--allow-secret-mention",
        "configKey": "policy.secret_detector.allow_phrases",
        "configRegexKey": "policy.secret_detector.allow_regex",
    });
    DomainError::PolicyDeniedWithDetails {
        message: format!(
            "Refusing to persist memory content that contains secrets: {}.",
            redacted_reasons.join(", ")
        ),
        repair: Some(
            "Redact the secret or run `ee remember --allow-secret-mention` only for auditable non-secret mentions."
                .to_owned(),
        ),
        details_json: details.to_string(),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SecretDetectorAllowConfig {
    allow_phrases: Vec<String>,
    allow_regex: Vec<String>,
}

fn load_secret_detector_allow_config(
    workspace_path: &Path,
) -> Result<SecretDetectorAllowConfig, DomainError> {
    let path = workspace_path.join(".ee").join("config.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SecretDetectorAllowConfig::default());
        }
        Err(error) => {
            return Err(DomainError::Configuration {
                message: format!(
                    "Failed to read workspace config {}: {error}",
                    path.display()
                ),
                repair: Some("Fix or remove .ee/config.toml.".to_owned()),
            });
        }
    };
    let config = ConfigFile::parse(&contents).map_err(|error| DomainError::Configuration {
        message: format!(
            "Failed to parse workspace config {}: {error}",
            path.display()
        ),
        repair: Some("Fix [policy.secret_detector] in .ee/config.toml.".to_owned()),
    })?;
    Ok(SecretDetectorAllowConfig {
        allow_phrases: config
            .policy
            .secret_detector
            .allow_phrases
            .unwrap_or_default(),
        allow_regex: config
            .policy
            .secret_detector
            .allow_regex
            .unwrap_or_default(),
    })
}

fn configured_bypass_kind(matches: &[RememberPolicyBypassMatch]) -> &'static str {
    let has_phrase = matches.iter().any(|item| item.kind == "config_phrase");
    let has_regex = matches.iter().any(|item| item.kind == "config_regex");
    match (has_phrase, has_regex) {
        (true, true) => "config",
        (true, false) => "config_phrase",
        (false, true) => "config_regex",
        (false, false) => "config",
    }
}

fn secret_detector_allow_matches(
    content: &str,
    config: &SecretDetectorAllowConfig,
) -> Result<Vec<RememberPolicyBypassMatch>, DomainError> {
    let mut matches = Vec::new();
    for phrase in &config.allow_phrases {
        let trimmed = phrase.trim();
        if trimmed.is_empty() {
            continue;
        }
        for (start, end) in find_case_insensitive_spans(content, trimmed) {
            let (span_start, span_end) = containing_sentence_span(content, start, end);
            matches.push(RememberPolicyBypassMatch {
                kind: "config_phrase".to_owned(),
                pattern: trimmed.to_owned(),
                matched_text: content[start..end].to_owned(),
                start: span_start,
                end: span_end,
            });
        }
    }

    for pattern in &config.allow_regex {
        let regex =
            regex_lite::Regex::new(pattern).map_err(|error| DomainError::Configuration {
                message: format!("Invalid policy.secret_detector.allow_regex `{pattern}`: {error}"),
                repair: Some(
                    "Fix [policy.secret_detector].allow_regex in .ee/config.toml.".to_owned(),
                ),
            })?;
        for matched in regex.find_iter(content) {
            matches.push(RememberPolicyBypassMatch {
                kind: "config_regex".to_owned(),
                pattern: pattern.clone(),
                matched_text: matched.as_str().to_owned(),
                start: matched.start(),
                end: matched.end(),
            });
        }
    }

    matches.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.pattern.cmp(&right.pattern))
    });
    matches.dedup();
    Ok(matches)
}

fn find_case_insensitive_spans(content: &str, needle: &str) -> Vec<(usize, usize)> {
    let lowercase_content = content.to_ascii_lowercase();
    let lowercase_needle = needle.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut offset = 0;
    while let Some(relative_start) = lowercase_content[offset..].find(&lowercase_needle) {
        let start = offset + relative_start;
        let end = start + lowercase_needle.len();
        if content.is_char_boundary(start) && content.is_char_boundary(end) {
            spans.push((start, end));
        }
        offset = end;
    }
    spans
}

fn containing_sentence_span(content: &str, start: usize, end: usize) -> (usize, usize) {
    let prefix = &content[..start];
    let span_start = prefix
        .rfind(['.', '!', '?', '\n'])
        .map_or(0, |index| index + 1);
    let suffix = &content[end..];
    let span_end = suffix
        .find(['.', '!', '?', '\n'])
        .map_or(content.len(), |index| end + index + 1);
    (trim_span_start(content, span_start, span_end), span_end)
}

fn trim_span_start(content: &str, mut start: usize, end: usize) -> usize {
    while start < end {
        let Some(next) = content[start..end].chars().next() else {
            break;
        };
        if !next.is_whitespace() {
            break;
        }
        start += next.len_utf8();
    }
    start
}

fn mask_allow_match_spans(content: &str, matches: &[RememberPolicyBypassMatch]) -> String {
    if matches.is_empty() {
        return content.to_owned();
    }

    let mut spans = matches
        .iter()
        .map(|item| (item.start, item.end))
        .collect::<Vec<_>>();
    spans.sort_unstable();

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }

    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for (start, end) in merged {
        out.push_str(&content[cursor..start]);
        for ch in content[start..end].chars() {
            out.push(if ch == '\n' { '\n' } else { ' ' });
        }
        cursor = end;
    }
    out.push_str(&content[cursor..]);
    out
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

    let input = CreateWorkspaceInput {
        path: path.clone(),
        name: workspace_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    };

    match connection.insert_workspace(workspace_id, &input) {
        Ok(()) => Ok(()),
        Err(error) if workspace_insert_lost_race(&error) => {
            if connection
                .get_workspace_by_path(&path)
                .map_err(|query_error| DomainError::Storage {
                    message: format!("Failed to query raced workspace: {query_error}"),
                    repair: Some("ee doctor".to_owned()),
                })?
                .is_some()
            {
                Ok(())
            } else {
                Err(DomainError::Storage {
                    message: format!("Failed to register workspace after insert race: {error}"),
                    repair: Some("ee doctor".to_owned()),
                })
            }
        }
        Err(error) => Err(DomainError::Storage {
            message: format!("Failed to register workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        }),
    }
}

fn workspace_insert_lost_race(error: &impl ToString) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("unique constraint failed: workspaces.path")
        || message.contains("unique constraint failed: workspaces.id")
}

#[allow(
    clippy::too_many_arguments,
    reason = "transaction retry helper mirrors the existing storage/audit/index inputs"
)]
fn store_remembered_memory_with_retry(
    connection: &DbConnection,
    memory_id: &str,
    audit_id: &str,
    index_job_id: &str,
    memory_input: &CreateMemoryInput,
    audit_details: &str,
    index_input: &CreateSearchIndexJobInput,
    policy_bypass: Option<&RememberPolicyBypassReport>,
) -> Result<(), DomainError> {
    const MAX_ATTEMPTS: usize = 8;

    for attempt in 0..MAX_ATTEMPTS {
        match connection.with_transaction(|| {
            connection.insert_memory(memory_id, memory_input)?;
            connection.insert_audit(
                audit_id,
                &CreateAuditInput {
                    workspace_id: Some(memory_input.workspace_id.clone()),
                    actor: Some("ee remember".to_owned()),
                    action: audit_actions::MEMORY_CREATE.to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(memory_id.to_owned()),
                    details: Some(audit_details.to_owned()),
                },
            )?;
            if let Some(policy_bypass) = policy_bypass {
                if let Some(policy_audit_id) = policy_bypass.audit_id.as_deref() {
                    connection.insert_audit(
                        policy_audit_id,
                        &CreateAuditInput {
                            workspace_id: Some(memory_input.workspace_id.clone()),
                            actor: Some("ee remember".to_owned()),
                            action: audit_actions::POLICY_BYPASS.to_owned(),
                            target_type: Some("memory".to_owned()),
                            target_id: Some(memory_id.to_owned()),
                            details: Some(policy_bypass_audit_details(policy_bypass)),
                        },
                    )?;
                }
            }
            connection.insert_search_index_job(index_job_id, index_input)
        }) {
            Ok(()) => return Ok(()),
            Err(error) if remember_write_contention_is_retryable(&error) => {
                let _ = connection.rollback();
                if memory_exists_after_commit_ambiguity(connection, memory_id)? {
                    return Ok(());
                }
                if attempt + 1 < MAX_ATTEMPTS {
                    std::thread::sleep(remember_write_retry_delay(attempt));
                } else {
                    return Err(DomainError::Storage {
                        message: format!(
                            "Failed to store memory after contention retries: {error}"
                        ),
                        repair: Some("ee doctor".to_string()),
                    });
                }
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!("Failed to store memory: {error}"),
                    repair: Some("ee doctor".to_string()),
                });
            }
        }
    }

    Err(DomainError::Storage {
        message: "Failed to store memory: retry loop exhausted".to_owned(),
        repair: Some("ee doctor".to_string()),
    })
}

fn memory_exists_after_commit_ambiguity(
    connection: &DbConnection,
    memory_id: &str,
) -> Result<bool, DomainError> {
    connection
        .get_memory(memory_id)
        .map(|memory| memory.is_some())
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query memory after write contention: {error}"),
            repair: Some("ee doctor".to_string()),
        })
}

fn remember_write_contention_is_retryable(error: &impl ToString) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("database is busy")
        || message.contains("snapshot conflict")
        || message.contains("database is locked")
        || message.contains("sqlite_busy")
}

fn remember_write_retry_delay(attempt: usize) -> Duration {
    let capped = attempt.min(6) as u64;
    Duration::from_millis(10 * (1 << capped))
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn generate_search_index_job_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("sidx_{payload}")
}

fn generate_memory_link_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("link_{payload}")
}

fn remember_audit_details(
    memory_id: &str,
    input: &CreateMemoryInput,
    policy_bypass: Option<&RememberPolicyBypassReport>,
) -> String {
    serde_json::json!({
        "schema": "ee.audit.memory_create.v1",
        "command": "ee remember",
        "memoryId": memory_id,
        "level": input.level,
        "kind": input.kind,
        "confidence": input.confidence,
        "trustClass": input.trust_class,
        "trustSubclass": input.trust_subclass,
        "provenanceUri": input.provenance_uri,
        "workflowId": input.workflow_id,
        "tagCount": input.tags.len(),
        "policyBypass": policy_bypass.map(policy_bypass_audit_json),
    })
    .to_string()
}

fn policy_bypass_audit_details(policy_bypass: &RememberPolicyBypassReport) -> String {
    serde_json::json!({
        "schema": "ee.audit.policy_bypass.v1",
        "command": "ee remember",
        "policyBypass": policy_bypass_audit_json(policy_bypass),
    })
    .to_string()
}

fn policy_bypass_audit_json(policy_bypass: &RememberPolicyBypassReport) -> serde_json::Value {
    serde_json::json!({
        "code": &policy_bypass.code,
        "severity": &policy_bypass.severity,
        "kind": &policy_bypass.kind,
        "message": &policy_bypass.message,
        "repair": &policy_bypass.repair,
        "redactedReasons": &policy_bypass.redacted_reasons,
        "matches": policy_bypass.matches.iter().map(|item| {
            serde_json::json!({
                "kind": &item.kind,
                "pattern": &item.pattern,
                "matchedText": &item.matched_text,
                "start": item.start,
                "end": item.end,
            })
        }).collect::<Vec<_>>(),
        "auditId": &policy_bypass.audit_id,
    })
}

fn append_remember_audit_jsonl(
    prepared: &PreparedRememberMemory,
    audit_id: &str,
    memory_id: &str,
    input: &CreateMemoryInput,
) -> Result<(), DomainError> {
    let audit_dir = prepared
        .database_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| prepared.workspace_path.join(".ee"));
    let audit_path = audit_dir.join("audit.jsonl");
    let event = AuditEvent::new(
        now_rfc3339_nanos(),
        "ee remember",
        audit_actions::MEMORY_CREATE,
        format!("memory:{memory_id}"),
        AuditOutcome::Success,
    )
    .with_field("audit_id", serde_json::json!(audit_id))
    .with_field(
        "workspace_id",
        serde_json::json!(input.workspace_id.clone()),
    )
    .with_field("memory_id", serde_json::json!(memory_id))
    .with_field("level", serde_json::json!(input.level.clone()))
    .with_field("kind", serde_json::json!(input.kind.clone()))
    .with_field("command", serde_json::json!("ee remember"));

    event
        .append_to_path(&audit_path)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Remembered memory but failed to append audit JSONL stream at {}: {error}",
                audit_path.display()
            ),
            repair: Some("ee doctor".to_owned()),
        })
}

const REMEMBER_AUTO_LINK_LIMIT: u32 = 8;
const REMEMBER_AUTO_LINK_WEIGHT: f32 = 0.5;

fn create_auto_links_for_remember(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    workflow_id: Option<&str>,
    enabled: bool,
) -> Result<Vec<RememberAutoLink>, DomainError> {
    if !enabled {
        return Ok(Vec::new());
    }
    let Some(workflow_id) = workflow_id else {
        return Ok(Vec::new());
    };

    let candidates = connection
        .list_recent_workflow_memories(
            workspace_id,
            workflow_id,
            memory_id,
            REMEMBER_AUTO_LINK_LIMIT,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workflow memories for auto-linking: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let mut auto_links = Vec::new();

    for candidate in candidates {
        let exists = connection
            .memory_link_exists_between(memory_id, &candidate.id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query existing memory links: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?;
        if exists {
            continue;
        }

        let link_id = generate_memory_link_id();
        let audit_id = generate_audit_id();
        let reinforced_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let input = CreateMemoryLinkInput {
            src_memory_id: memory_id.to_owned(),
            dst_memory_id: candidate.id.clone(),
            relation: MemoryLinkRelation::Related,
            weight: REMEMBER_AUTO_LINK_WEIGHT,
            confidence: REMEMBER_AUTO_LINK_WEIGHT,
            directed: false,
            evidence_count: 1,
            last_reinforced_at: Some(reinforced_at),
            source: MemoryLinkSource::Auto,
            created_by: Some("ee remember".to_owned()),
            metadata_json: Some(
                serde_json::json!({
                    "schema": "ee.memory_link.hebbian_auto.v1",
                    "linkKind": "hebbian",
                    "workflowId": workflow_id,
                    "reason": "same_workflow_recent_memory",
                })
                .to_string(),
            ),
        };
        let audit_details = serde_json::json!({
            "schema": "ee.audit.memory_link_auto_create.v1",
            "command": "ee remember",
            "linkId": &link_id,
            "srcMemoryId": memory_id,
            "dstMemoryId": &candidate.id,
            "workflowId": workflow_id,
            "relation": input.relation.as_str(),
            "source": input.source.as_str(),
            "weight": input.weight,
            "linkKind": "hebbian",
        })
        .to_string();

        connection
            .with_transaction(|| {
                connection.insert_memory_link(&link_id, &input)?;
                connection.insert_audit(
                    &audit_id,
                    &CreateAuditInput {
                        workspace_id: Some(workspace_id.to_owned()),
                        actor: Some("ee remember".to_owned()),
                        action: audit_actions::MEMORY_LINK_CREATE.to_owned(),
                        target_type: Some("memory_link".to_owned()),
                        target_id: Some(link_id.clone()),
                        details: Some(audit_details.clone()),
                    },
                )
            })
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to create workflow auto-link: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?;

        auto_links.push(RememberAutoLink {
            link_id,
            target_memory_id: candidate.id,
            relation: input.relation.as_str().to_owned(),
            weight: input.weight,
            source: input.source.as_str().to_owned(),
            audit_id,
        });
    }

    Ok(auto_links)
}

fn auto_link_status(
    workflow_id: Option<&str>,
    enabled: bool,
    auto_links: &[RememberAutoLink],
) -> &'static str {
    if !enabled {
        "disabled"
    } else if workflow_id.is_none() {
        // G7 (bd-17c65.7.6): honest-unimplemented. Without a workflow
        // context we cannot meaningfully auto-link. The status name
        // explicitly says "required" so an agent reading it knows this
        // is NOT a failure — it's an expected state outside a workflow.
        // The caller emits an `auto_link_disabled` info-severity
        // degraded entry pointing at the explicit `ee memory link`
        // surface as the recovery path.
        "no_workflow_required"
    } else if auto_links.is_empty() {
        "no_candidates"
    } else {
        "linked"
    }
}

const REMEMBER_CURATION_NEIGHBOR_LIMIT: usize = 10;
const REMEMBER_CURATION_CLUSTER_THRESHOLD: usize =
    crate::curate::cluster_coherence::DEFAULT_MIN_CLUSTER_SIZE;
#[cfg(not(test))]
const REMEMBER_CURATION_SYNC_BUDGET_MS: u128 = 50;
#[cfg(test)]
const REMEMBER_CURATION_TEST_SYNC_BUDGET_MS: u128 = 5_000;

struct RememberCurationProposalReport {
    candidate: Option<RememberCurationCandidateProposal>,
    status: &'static str,
    degradations: Vec<RememberSuggestedLinkDegradation>,
}

struct RememberCoherentCurationCluster {
    members: Vec<StoredMemory>,
    cluster_id: String,
    silhouette_score: f64,
    threshold: f64,
    embedding_snapshot_hash: String,
}

fn propose_curation_candidate_for_remember(
    connection: &DbConnection,
    prepared: &PreparedRememberMemory,
    memory_id: &str,
    memory_input: &CreateMemoryInput,
    enabled: bool,
) -> Result<RememberCurationProposalReport, DomainError> {
    if !enabled {
        return Ok(RememberCurationProposalReport {
            candidate: None,
            status: "disabled",
            degradations: Vec::new(),
        });
    }
    if memory_input.tags.is_empty() {
        return Ok(RememberCurationProposalReport {
            candidate: None,
            status: "skipped_too_few_neighbors",
            degradations: vec![RememberSuggestedLinkDegradation {
                code: "auto_propose_skipped_too_few_neighbors".to_owned(),
                severity: "info".to_owned(),
                message:
                    "No tags were supplied, so remember-time candidate proposal had no cluster key."
                        .to_owned(),
                repair: "Use `ee remember --tags <tag>` for memories that should participate in proposal clustering."
                    .to_owned(),
            }],
        });
    }

    let started = Instant::now();
    let mut degradations = Vec::new();
    let mut member_ids = match remember_search_neighbor_ids(prepared, memory_input) {
        Ok(ids) => ids,
        Err(error) => {
            degradations.push(RememberSuggestedLinkDegradation {
                code: "auto_propose_search_neighbor_lookup_failed".to_owned(),
                severity: "info".to_owned(),
                message: format!(
                    "Frankensearch neighbor lookup was unavailable during remember-time proposal: {error}"
                ),
                repair: "Falling back to deterministic tag-overlap clustering.".to_owned(),
            });
            Vec::new()
        }
    };
    append_tag_overlap_neighbor_ids(
        connection,
        &prepared.workspace_id,
        &mut member_ids,
        &memory_input.tags,
    )?;

    let cluster = remember_candidate_cluster(
        connection,
        &prepared.workspace_id,
        memory_id,
        memory_input,
        member_ids,
    )?;
    if cluster.len() < REMEMBER_CURATION_CLUSTER_THRESHOLD {
        return Ok(RememberCurationProposalReport {
            candidate: None,
            status: "skipped_too_few_neighbors",
            degradations,
        });
    }
    let Some(coherent_cluster) =
        remember_candidate_coherent_cluster(connection, &prepared.workspace_path, &cluster)?
    else {
        return Ok(RememberCurationProposalReport {
            candidate: None,
            status: "skipped_low_coherence",
            degradations,
        });
    };
    if let Some(rule_id) = remember_existing_rule_covering_cluster(
        connection,
        &prepared.workspace_id,
        memory_input,
        &coherent_cluster.members,
    )? {
        degradations.push(RememberSuggestedLinkDegradation {
            code: "auto_propose_skipped_existing_rule_covers".to_owned(),
            severity: "info".to_owned(),
            message: format!(
                "An existing procedural rule already covers this remember-time evidence cluster: {rule_id}."
            ),
            repair: "Review the existing rule with `ee rule show <rule-id> --json` before proposing another candidate."
                .to_owned(),
        });
        return Ok(RememberCurationProposalReport {
            candidate: None,
            status: "skipped_existing_rule_covers",
            degradations,
        });
    }

    let mut member_memory_ids = coherent_cluster
        .members
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<Vec<_>>();
    member_memory_ids.sort();
    member_memory_ids.dedup();

    let candidate_id = remember_curation_candidate_id(&prepared.workspace_id, &member_memory_ids);
    let already_exists = connection
        .get_curation_candidate(&prepared.workspace_id, &candidate_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to check existing curation candidate: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?
        .is_some();
    let reason = remember_curation_candidate_reason(memory_input, &member_memory_ids);
    let target_memory_id = member_memory_ids
        .first()
        .cloned()
        .unwrap_or_else(|| memory_id.to_owned());
    if already_exists {
        return Ok(RememberCurationProposalReport {
            candidate: Some(RememberCurationCandidateProposal {
                candidate_id,
                member_memory_ids,
                target_memory_id,
                candidate_type: CandidateType::Rule.as_str().to_owned(),
                audit_id: None,
                reason,
            }),
            status: "already_exists",
            degradations,
        });
    }

    if started.elapsed().as_millis() > remember_curation_sync_budget_ms() {
        degradations.push(RememberSuggestedLinkDegradation {
            code: "auto_propose_deferred_to_maintenance".to_owned(),
            severity: "info".to_owned(),
            message: "Remember-time proposal exceeded the synchronous budget before durable write."
                .to_owned(),
            repair:
                "Run `ee review workspace --propose --json` to produce candidates from workspace evidence."
                    .to_owned(),
        });
        return Ok(RememberCurationProposalReport {
            candidate: None,
            status: "deferred_to_maintenance",
            degradations,
        });
    }

    let audit_id = generate_audit_id();
    let proposed_content =
        remember_curation_candidate_content(memory_input, &coherent_cluster.members);
    let proposed_confidence = remember_curation_candidate_confidence(&coherent_cluster.members);
    let source_id = member_memory_ids.join(",");
    let audit_details = remember_curation_candidate_audit_details(
        &candidate_id,
        memory_id,
        &member_memory_ids,
        &reason,
        &coherent_cluster,
    );

    connection
        .with_transaction(|| {
            connection.insert_curation_candidate(
                &candidate_id,
                &CreateCurationCandidateInput {
                    workspace_id: prepared.workspace_id.clone(),
                    candidate_type: CandidateType::Rule.as_str().to_owned(),
                    target_memory_id: target_memory_id.clone(),
                    proposed_content: Some(proposed_content.clone()),
                    proposed_confidence: Some(proposed_confidence),
                    proposed_trust_class: None,
                    source_type: CandidateSource::AgentInference.as_str().to_owned(),
                    source_id: Some(source_id.clone()),
                    reason: reason.clone(),
                    confidence: proposed_confidence,
                    status: Some(CandidateStatus::Pending.as_str().to_owned()),
                    created_at: None,
                    ttl_expires_at: None,
                },
            )?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(prepared.workspace_id.clone()),
                    actor: Some("ee remember".to_owned()),
                    action: audit_actions::CURATION_CANDIDATE_CREATE.to_owned(),
                    target_type: Some("curation_candidate".to_owned()),
                    target_id: Some(candidate_id.clone()),
                    details: Some(audit_details.clone()),
                },
            )
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to insert remember-time curation candidate: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;

    Ok(RememberCurationProposalReport {
        candidate: Some(RememberCurationCandidateProposal {
            candidate_id,
            member_memory_ids,
            target_memory_id,
            candidate_type: CandidateType::Rule.as_str().to_owned(),
            audit_id: Some(audit_id),
            reason,
        }),
        status: "proposed",
        degradations,
    })
}

fn remember_search_neighbor_ids(
    prepared: &PreparedRememberMemory,
    memory_input: &CreateMemoryInput,
) -> Result<Vec<String>, String> {
    if remember_search_neighbors_disabled() {
        return Err(format!(
            "disabled by {}",
            crate::config::env_registry::EnvVar::DisableRememberSearchNeighbors.name()
        ));
    }

    let report = run_search(&SearchOptions {
        workspace_path: prepared.workspace_path.clone(),
        database_path: Some(prepared.database_path.clone()),
        index_dir: Some(
            prepared
                .workspace_path
                .join(".ee")
                .join(DEFAULT_INDEX_SUBDIR),
        ),
        query: memory_input.content.clone(),
        limit: u32::try_from(REMEMBER_CURATION_NEIGHBOR_LIMIT + 1).unwrap_or(u32::MAX),
        speed: crate::search::SpeedMode::Default,
        explain: false,
        as_of: None,
        include_tombstoned: false,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: Some(0.0),
        source_mode: crate::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        memory_scope: crate::models::MemoryScope::Swarm,
        strict_scope: false,
    })
    .map_err(|error| error.to_string())?;

    if matches!(
        report.status,
        SearchStatus::IndexError | SearchStatus::IndexNotFound
    ) {
        return Err(format!("search status {}", report.status.as_str()));
    }

    Ok(report
        .results
        .into_iter()
        .map(|hit| hit.doc_id)
        .take(REMEMBER_CURATION_NEIGHBOR_LIMIT + 1)
        .collect())
}

fn append_tag_overlap_neighbor_ids(
    connection: &DbConnection,
    workspace_id: &str,
    member_ids: &mut Vec<String>,
    tags: &[String],
) -> Result<(), DomainError> {
    let mut tag_matches: BTreeMap<String, usize> = BTreeMap::new();
    for tag in tags {
        let tagged_ids = connection
            .list_memories_by_tag(workspace_id, tag)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query tag-overlap curation neighbors: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        for memory_id in tagged_ids {
            *tag_matches.entry(memory_id).or_default() += 1;
        }
    }
    let mut ranked = tag_matches.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_id, left_count), (right_id, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_id.cmp(right_id))
    });
    for (memory_id, _) in ranked {
        if !member_ids.contains(&memory_id) {
            member_ids.push(memory_id);
        }
    }
    Ok(())
}

fn remember_candidate_cluster(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    memory_input: &CreateMemoryInput,
    member_ids: Vec<String>,
) -> Result<Vec<StoredMemory>, DomainError> {
    let required_tags = memory_input.tags.iter().cloned().collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut cluster = Vec::new();
    for candidate_id in std::iter::once(memory_id.to_owned()).chain(member_ids) {
        if !seen.insert(candidate_id.clone()) {
            continue;
        }
        let Some(memory) =
            connection
                .get_memory(&candidate_id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to load curation neighbor memory: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                })?
        else {
            continue;
        };
        if memory.workspace_id != workspace_id
            || memory.tombstoned_at.is_some()
            || memory.level != memory_input.level
            || memory.kind != memory_input.kind
        {
            continue;
        }
        if memory_input.workflow_id.is_some() && memory.workflow_id != memory_input.workflow_id {
            continue;
        }
        let candidate_tags =
            connection
                .get_memory_tags(&memory.id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to load curation neighbor tags: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                })?;
        if !required_tags.is_empty()
            && !candidate_tags
                .iter()
                .any(|tag| required_tags.contains(tag.as_str()))
        {
            continue;
        }
        cluster.push(memory);
        if cluster.len() > REMEMBER_CURATION_NEIGHBOR_LIMIT {
            break;
        }
    }
    cluster.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(cluster)
}

fn remember_candidate_coherent_cluster(
    connection: &DbConnection,
    workspace_path: &Path,
    cluster: &[StoredMemory],
) -> Result<Option<RememberCoherentCurationCluster>, DomainError> {
    let config = remember_cluster_coherence_config(workspace_path)?;
    if cluster.len() < config.min_cluster_size {
        return Ok(None);
    }

    let memory_ids = cluster
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    let tags_by_memory = connection
        .get_memory_tags_batch(&memory_ids)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load curation cluster memory tags: {error}"),
            repair: Some("ee memory tags <memory-id> --json".to_owned()),
        })?;
    let embedder = HashEmbedder::default_256();
    let points = cluster
        .iter()
        .map(|memory| {
            let tags = tags_by_memory
                .get(&memory.id)
                .map_or(&[] as &[String], Vec::as_slice);
            EmbeddingPoint::new(
                memory.id.clone(),
                embedder
                    .embed_sync(&remember_curation_cluster_embedding_text(memory, tags))
                    .into_iter()
                    .map(f64::from)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let embedding_snapshot_hash = remember_curation_embedding_snapshot_hash(&points, config);
    let report = agglomerate(&points, config).map_err(|error| DomainError::SearchIndex {
        message: format!("Failed to score remember-time curation cluster coherence: {error}"),
        repair: Some("Run `ee learn cluster --json` to inspect clustering inputs.".to_owned()),
    })?;
    let mut clusters = report.clusters;
    clusters.sort_by(|left, right| {
        right
            .member_count
            .cmp(&left.member_count)
            .then_with(|| {
                right
                    .silhouette_score
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&left.silhouette_score.unwrap_or(f64::NEG_INFINITY))
            })
            .then_with(|| left.cluster_id.cmp(&right.cluster_id))
    });
    let Some(best_cluster) = clusters.into_iter().next() else {
        return Ok(None);
    };
    if !best_cluster.accepted {
        return Ok(None);
    }
    let Some(silhouette_score) = best_cluster.silhouette_score else {
        return Ok(None);
    };

    let memories_by_id = cluster
        .iter()
        .map(|memory| (memory.id.as_str(), memory))
        .collect::<BTreeMap<_, _>>();
    let members = best_cluster
        .member_memory_ids
        .iter()
        .filter_map(|memory_id| {
            memories_by_id
                .get(memory_id.as_str())
                .map(|memory| (*memory).clone())
        })
        .collect::<Vec<_>>();
    if members.len() < config.min_cluster_size {
        return Ok(None);
    }

    Ok(Some(RememberCoherentCurationCluster {
        members,
        cluster_id: best_cluster.cluster_id,
        silhouette_score,
        threshold: config.merge_threshold,
        embedding_snapshot_hash,
    }))
}

fn remember_cluster_coherence_config(
    workspace_path: &Path,
) -> Result<ClusterCoherenceConfig, DomainError> {
    let config_path = workspace_path.join(".ee").join("config.toml");
    let threshold = match fs::read_to_string(&config_path) {
        Ok(contents) => {
            let config =
                ConfigFile::parse(&contents).map_err(|error| DomainError::Configuration {
                    message: format!(
                        "Failed to parse workspace learn config {}: {error}",
                        config_path.display()
                    ),
                    repair: Some("Fix [learn] in .ee/config.toml.".to_owned()),
                })?;
            config
                .learn
                .cluster_coherence_threshold
                .unwrap_or(crate::curate::cluster_coherence::DEFAULT_CLUSTER_COHERENCE_THRESHOLD)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::curate::cluster_coherence::DEFAULT_CLUSTER_COHERENCE_THRESHOLD
        }
        Err(error) => {
            return Err(DomainError::Configuration {
                message: format!(
                    "Failed to read workspace learn config {}: {error}",
                    config_path.display()
                ),
                repair: Some("Check .ee/config.toml and retry `ee remember`.".to_owned()),
            });
        }
    };
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(DomainError::Configuration {
            message: format!(
                "Config key `learn.cluster_coherence_threshold` must be finite and between 0.0 and 1.0, got {threshold}."
            ),
            repair: Some("Use a threshold between 0.0 and 1.0 in [learn].".to_owned()),
        });
    }

    Ok(ClusterCoherenceConfig {
        merge_threshold: threshold,
        silhouette_cutoff: crate::curate::cluster_coherence::DEFAULT_CLUSTER_SILHOUETTE_CUTOFF,
        min_cluster_size: crate::curate::cluster_coherence::DEFAULT_MIN_CLUSTER_SIZE,
    })
}

fn remember_curation_cluster_embedding_text(memory: &StoredMemory, tags: &[String]) -> String {
    let mut tags = tags.to_vec();
    tags.sort();
    format!(
        "level:{}\nkind:{}\ntags:{}\ncontent:{}",
        memory.level,
        memory.kind,
        tags.join(" "),
        memory.content
    )
}

fn remember_curation_embedding_snapshot_hash(
    points: &[EmbeddingPoint],
    config: ClusterCoherenceConfig,
) -> String {
    let mut sorted = points.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.remember_curation_embedding_snapshot.v1\n");
    remember_curation_hash_field(
        &mut hasher,
        "threshold",
        &format!("{:.6}", config.merge_threshold),
    );
    for point in sorted {
        remember_curation_hash_field(&mut hasher, "memory_id", &point.memory_id);
        for value in &point.embedding {
            remember_curation_hash_field(&mut hasher, "value", &format!("{value:.9}"));
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn remember_curation_hash_field(hasher: &mut blake3::Hasher, field: &str, value: &str) {
    hasher.update(field.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.len().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    hasher.update(b"\n");
}

fn remember_existing_rule_covering_cluster(
    connection: &DbConnection,
    workspace_id: &str,
    memory_input: &CreateMemoryInput,
    cluster: &[StoredMemory],
) -> Result<Option<String>, DomainError> {
    let proposal_tags = memory_input.tags.iter().cloned().collect::<BTreeSet<_>>();
    let cluster_tokens = remember_curation_cluster_tokens(memory_input, cluster);
    if proposal_tags.is_empty() || cluster_tokens.is_empty() {
        return Ok(None);
    }

    let rules = connection
        .list_procedural_rules(workspace_id, None, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to inspect existing procedural rules: {error}"),
            repair: Some("ee rule list --json".to_owned()),
        })?;
    for rule in rules {
        let rule_tags =
            connection
                .get_rule_tags(&rule.id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to inspect procedural rule tags: {error}"),
                    repair: Some(format!("ee rule show {} --json", rule.id)),
                })?;
        if !rule_tags.iter().any(|tag| proposal_tags.contains(tag)) {
            continue;
        }
        let rule_tokens = remember_curation_content_tokens(&rule.content);
        let overlap = cluster_tokens
            .intersection(&rule_tokens)
            .take(REMEMBER_CURATION_COVERING_RULE_MIN_TOKEN_OVERLAP)
            .count();
        if overlap >= REMEMBER_CURATION_COVERING_RULE_MIN_TOKEN_OVERLAP {
            return Ok(Some(rule.id));
        }
    }

    Ok(None)
}

const REMEMBER_CURATION_COVERING_RULE_MIN_TOKEN_OVERLAP: usize = 3;

fn remember_curation_cluster_tokens(
    memory_input: &CreateMemoryInput,
    cluster: &[StoredMemory],
) -> BTreeSet<String> {
    let mut tokens = remember_curation_content_tokens(&memory_input.content);
    for memory in cluster {
        tokens.extend(remember_curation_content_tokens(&memory.content));
    }
    tokens
}

fn remember_curation_content_tokens(content: &str) -> BTreeSet<String> {
    content
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|token| {
            let token = token.trim().to_ascii_lowercase();
            if token.len() < 3
                || REMEMBER_CURATION_COVERING_RULE_STOPWORDS.contains(&token.as_str())
            {
                None
            } else {
                Some(token)
            }
        })
        .collect()
}

const REMEMBER_CURATION_COVERING_RULE_STOPWORDS: &[&str] = &[
    "about", "after", "and", "before", "for", "from", "into", "memory", "rule", "that", "the",
    "this", "with",
];

fn remember_curation_candidate_id(workspace_id: &str, member_memory_ids: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\n");
    for memory_id in member_memory_ids {
        hasher.update(memory_id.as_bytes());
        hasher.update(b"\n");
    }
    let suffix = hasher.finalize().to_hex().to_string();
    format!("curate_{}", &suffix[..26])
}

fn remember_curation_candidate_reason(
    memory_input: &CreateMemoryInput,
    member_memory_ids: &[String],
) -> String {
    format!(
        "Remember-time proposal clustered {} {} `{}` memories sharing tag(s): {}.",
        member_memory_ids.len(),
        memory_input.level,
        memory_input.kind,
        memory_input.tags.join(",")
    )
}

fn remember_curation_candidate_content(
    memory_input: &CreateMemoryInput,
    cluster: &[StoredMemory],
) -> String {
    let exemplar = cluster
        .iter()
        .min_by(|left, right| left.id.cmp(&right.id))
        .map(|memory| memory.content.as_str())
        .unwrap_or(memory_input.content.as_str());
    format!(
        "Consolidate repeated {} `{}` memories tagged [{}]: {}",
        memory_input.level,
        memory_input.kind,
        memory_input.tags.join(","),
        exemplar
    )
}

fn remember_curation_candidate_confidence(cluster: &[StoredMemory]) -> f32 {
    let sum = cluster.iter().map(|memory| memory.confidence).sum::<f32>();
    let count = cluster.len().max(1) as f32;
    (sum / count).clamp(0.05, 0.95)
}

fn remember_curation_candidate_audit_details(
    candidate_id: &str,
    trigger_memory_id: &str,
    member_memory_ids: &[String],
    reason: &str,
    coherent_cluster: &RememberCoherentCurationCluster,
) -> String {
    serde_json::json!({
        "schema": "ee.audit.remember_curation_candidate_create.v1",
        "command": "ee remember",
        "candidateId": candidate_id,
        "triggerMemoryId": trigger_memory_id,
        "memberMemoryIds": member_memory_ids,
        "reason": reason,
        "cluster": {
            "algorithm": "average_linkage_agglomerative",
            "clusterId": &coherent_cluster.cluster_id,
            "memberCount": coherent_cluster.members.len(),
            "silhouette": coherent_cluster.silhouette_score,
            "threshold": coherent_cluster.threshold,
            "embeddingSnapshotHash": &coherent_cluster.embedding_snapshot_hash,
        },
    })
    .to_string()
}

#[cfg(not(test))]
fn remember_curation_sync_budget_ms() -> u128 {
    crate::config::env_registry::read(
        crate::config::env_registry::EnvVar::RememberCurationSyncBudgetMs,
    )
    .and_then(|raw| raw.parse::<u128>().ok())
    .filter(|budget_ms| *budget_ms > 0)
    .unwrap_or(REMEMBER_CURATION_SYNC_BUDGET_MS)
}

#[cfg(test)]
fn remember_curation_sync_budget_ms() -> u128 {
    REMEMBER_CURATION_TEST_SYNC_BUDGET_MS
}

fn remember_search_neighbors_disabled() -> bool {
    crate::config::env_registry::read(
        crate::config::env_registry::EnvVar::DisableRememberSearchNeighbors,
    )
    .is_some_and(|raw| {
        let trimmed = raw.trim();
        !(trimmed.is_empty()
            || trimmed == "0"
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed.eq_ignore_ascii_case("no")
            || trimmed.eq_ignore_ascii_case("off"))
    })
}

fn suggest_links_for_remember(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    tags: &[String],
) -> Result<Vec<RememberSuggestedLink>, DomainError> {
    if tags.is_empty() {
        return Ok(Vec::new());
    }

    let mut matches: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tag in tags {
        let tagged_memory_ids =
            connection
                .list_memories_by_tag(workspace_id, tag)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to query memories by tag for suggestions: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                })?;
        for target_memory_id in tagged_memory_ids {
            if target_memory_id == memory_id {
                continue;
            }
            matches
                .entry(target_memory_id)
                .or_default()
                .insert(tag.clone());
        }
    }

    if matches.is_empty() {
        return Ok(Vec::new());
    }

    let existing_links = connection
        .list_memory_links_for_memory(memory_id, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query existing memory links for suggestions: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let mut existing_targets = BTreeSet::new();
    for link in existing_links {
        if link.src_memory_id == memory_id {
            existing_targets.insert(link.dst_memory_id);
        } else if link.dst_memory_id == memory_id {
            existing_targets.insert(link.src_memory_id);
        }
    }

    Ok(build_suggested_links_from_matches(
        memory_id,
        matches,
        &existing_targets,
        tags.len(),
        REMEMBER_SUGGESTED_LINK_LIMIT,
    ))
}

fn build_suggested_links_from_matches(
    memory_id: &str,
    matches: BTreeMap<String, BTreeSet<String>>,
    existing_targets: &BTreeSet<String>,
    tag_count: usize,
    limit: usize,
) -> Vec<RememberSuggestedLink> {
    let mut candidates: Vec<(String, Vec<String>)> = matches
        .into_iter()
        .filter(|(target_memory_id, matched_tags)| {
            target_memory_id != memory_id
                && !matched_tags.is_empty()
                && !existing_targets.contains(target_memory_id)
        })
        .map(|(target_memory_id, matched_tags)| {
            (
                target_memory_id,
                matched_tags.into_iter().collect::<Vec<_>>(),
            )
        })
        .collect();

    candidates.sort_by(|(left_id, left_tags), (right_id, right_tags)| {
        right_tags
            .len()
            .cmp(&left_tags.len())
            .then_with(|| left_id.cmp(right_id))
    });

    candidates
        .into_iter()
        .take(limit)
        .map(|(target_memory_id, matched_tags)| {
            let evidence_count = u32::try_from(matched_tags.len()).unwrap_or(u32::MAX);
            RememberSuggestedLink {
                schema: REMEMBER_SUGGESTED_LINK_SCHEMA_V1,
                relation: "co_tag".to_owned(),
                target_memory_id,
                score: co_tag_score(matched_tags.len(), tag_count),
                confidence: co_tag_confidence(matched_tags.len()),
                evidence_count,
                evidence_summary: summarize_matched_tags(&matched_tags),
                source: "tag_cooccurrence".to_owned(),
                matched_tags,
                next_action:
                    "Review this staged link; apply only through an explicit curation/apply command."
                        .to_owned(),
            }
        })
        .collect()
}

fn summarize_matched_tags(tags: &[String]) -> String {
    if tags.len() == 1 {
        return format!("Shares tag `{}` with the newly remembered memory.", tags[0]);
    }

    let rendered = tags
        .iter()
        .map(|tag| format!("`{tag}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Shares {} tags with the newly remembered memory: {rendered}.",
        tags.len()
    )
}

fn co_tag_score(matched_tag_count: usize, total_tag_count: usize) -> f32 {
    let matched = usize_count_to_f32(matched_tag_count);
    let total = usize_count_to_f32(total_tag_count.max(1));
    (0.55 + ((matched / total) * 0.4)).min(0.95)
}

fn co_tag_confidence(matched_tag_count: usize) -> f32 {
    (0.5 + (usize_count_to_f32(matched_tag_count) * 0.1)).min(0.9)
}

fn usize_count_to_f32(value: usize) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn capped_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn remember_usage_error(message: String) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some("ee remember --help".to_owned()),
    }
}

/// Options for retrieving a memory.
#[derive(Clone, Debug)]
pub struct GetMemoryOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID to retrieve.
    pub memory_id: &'a str,
    /// Whether to include tombstoned memories.
    pub include_tombstoned: bool,
}

/// Result of a memory show operation.
#[derive(Clone, Debug)]
pub struct MemoryShowReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// The memory details if found.
    pub memory: Option<MemoryDetails>,
    /// Whether the memory was found.
    pub found: bool,
    /// Whether the memory is tombstoned (soft-deleted).
    pub is_tombstoned: bool,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

impl MemoryShowReport {
    /// Create a report for a found memory.
    #[must_use]
    pub fn found(details: MemoryDetails) -> Self {
        let is_tombstoned = details.memory.tombstoned_at.is_some();
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory: Some(details),
            found: true,
            is_tombstoned,
            error: None,
        }
    }

    /// Create a report for a not-found memory.
    #[must_use]
    pub fn not_found() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory: None,
            found: false,
            is_tombstoned: false,
            error: None,
        }
    }

    /// Create a report for a database error.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory: None,
            found: false,
            is_tombstoned: false,
            error: Some(message),
        }
    }
}

/// Retrieve a memory by ID with its tags.
///
/// Returns `None` if the memory does not exist. If `include_tombstoned` is false,
/// tombstoned memories are treated as not found.
pub fn get_memory_details(options: &GetMemoryOptions<'_>) -> MemoryShowReport {
    let conn = match open_migrated_memory_database(options.database_path) {
        Ok(c) => c,
        Err(message) => return MemoryShowReport::error(message),
    };

    let memory = match conn.get_memory(options.memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return MemoryShowReport::not_found(),
        Err(e) => return MemoryShowReport::error(format!("Failed to query memory: {e}")),
    };

    // Check if tombstoned and whether to include it
    if memory.tombstoned_at.is_some() && !options.include_tombstoned {
        return MemoryShowReport::not_found();
    }

    let tags = match conn.get_memory_tags(options.memory_id) {
        Ok(t) => t,
        Err(e) => return MemoryShowReport::error(format!("Failed to query tags: {e}")),
    };

    // Bead bd-17c65.7.7 (G8): best-effort audit row so L3 has a
    // last_accessed signal for `ee memory show` / `ee show <mem_id>`
    // alias dispatch and G1 can count show-inspection activity. Failure
    // to append is silently swallowed — never block the read.
    let details = serde_json::json!({"surface": "memory.show"}).to_string();
    let audit_input = crate::db::CreateAuditInput {
        workspace_id: Some(memory.workspace_id.clone()),
        actor: None,
        action: crate::db::audit_actions::MEMORY_SHOW.to_owned(),
        target_type: Some("memory".to_owned()),
        target_id: Some(options.memory_id.to_owned()),
        details: Some(details),
    };
    let _ = conn.insert_audit(&crate::db::generate_audit_id(), &audit_input);

    MemoryShowReport::found(MemoryDetails { memory, tags })
}

/// Options for listing memories.
#[derive(Clone, Debug)]
pub struct ListMemoriesOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Workspace path (used to derive workspace_id).
    pub workspace_path: &'a Path,
    /// Filter by memory level.
    pub level: Option<&'a str>,
    /// Filter by tag.
    pub tag: Option<&'a str>,
    /// Maximum number of memories to return.
    pub limit: u32,
    /// Whether to include tombstoned memories.
    pub include_tombstoned: bool,
}

/// Result of a memory list operation.
#[derive(Clone, Debug)]
pub struct MemoryListReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// List of memory summaries.
    pub memories: Vec<MemorySummary>,
    /// Total count of memories matching the filter.
    pub total_count: u32,
    /// Whether results were truncated due to limit.
    pub truncated: bool,
    /// Filter applied.
    pub filter: MemoryListFilter,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

/// Summary of a memory for list output.
#[derive(Clone, Debug)]
pub struct MemorySummary {
    /// Memory ID.
    pub id: String,
    /// Memory level.
    pub level: String,
    /// Memory kind.
    pub kind: String,
    /// Memory body text. May be truncated for list views — when truncated,
    /// `content_truncated` is `true` and the value ends with "...".
    pub content: String,
    /// True if `content` was truncated for the list view. False when the full
    /// body is returned (including when the body itself is empty).
    pub content_truncated: bool,
    /// Confidence score.
    pub confidence: f32,
    /// Provenance URI (EE-072: preserve provenance through JSON output).
    pub provenance_uri: Option<String>,
    /// Whether tombstoned.
    pub is_tombstoned: bool,
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<String>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<String>,
    /// Current validity status computed from the stored validity window.
    pub validity_status: String,
    /// Stable shape of the validity window.
    pub validity_window_kind: String,
    /// Creation timestamp.
    pub created_at: String,
}

/// Filter applied to memory list.
#[derive(Clone, Debug, Default)]
pub struct MemoryListFilter {
    /// Level filter if applied.
    pub level: Option<String>,
    /// Tag filter if applied.
    pub tag: Option<String>,
    /// Include tombstoned.
    pub include_tombstoned: bool,
}

impl MemoryListReport {
    /// Create a successful report.
    #[must_use]
    pub fn success(
        memories: Vec<MemorySummary>,
        total_count: u32,
        truncated: bool,
        filter: MemoryListFilter,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memories,
            total_count,
            truncated,
            filter,
            error: None,
        }
    }

    /// Create an error report.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memories: Vec::new(),
            total_count: 0,
            truncated: false,
            filter: MemoryListFilter::default(),
            error: Some(message),
        }
    }
}

const CONTENT_PREVIEW_LEN: usize = 80;

fn open_migrated_memory_database(database_path: &Path) -> Result<DbConnection, String> {
    let conn = DbConnection::open_file(database_path)
        .map_err(|error| format!("Failed to open database: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("Failed to migrate database: {error}"))?;
    Ok(conn)
}

fn truncate_content(content: &str) -> (String, bool) {
    let char_count = content.chars().count();
    if char_count <= CONTENT_PREVIEW_LEN {
        (content.to_string(), false)
    } else {
        let truncated: String = content.chars().take(CONTENT_PREVIEW_LEN).collect();
        (format!("{truncated}..."), true)
    }
}

/// List memories matching the given criteria.
pub fn list_memories(options: &ListMemoriesOptions<'_>) -> MemoryListReport {
    let conn = match open_migrated_memory_database(options.database_path) {
        Ok(c) => c,
        Err(message) => return MemoryListReport::error(message),
    };

    let filter = MemoryListFilter {
        level: options.level.map(String::from),
        tag: options.tag.map(String::from),
        include_tombstoned: options.include_tombstoned,
    };

    // Match `remember`'s workspace-ID derivation so absolute paths,
    // relative paths, and symlinked paths all address the same records.
    let workspace_path = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&workspace_path);

    // If filtering by tag, get memory IDs first
    let memory_ids: Option<Vec<String>> = if let Some(tag) = options.tag {
        match conn.list_memories_by_tag(&workspace_id, tag) {
            Ok(ids) => Some(ids),
            Err(e) => return MemoryListReport::error(format!("Failed to query by tag: {e}")),
        }
    } else {
        None
    };

    // Get memories
    let stored = match conn.list_memories(&workspace_id, options.level, options.include_tombstoned)
    {
        Ok(m) => m,
        Err(e) => return MemoryListReport::error(format!("Failed to list memories: {e}")),
    };

    // Filter by tag if needed
    let filtered: Vec<_> = if let Some(ref ids) = memory_ids {
        stored.into_iter().filter(|m| ids.contains(&m.id)).collect()
    } else {
        stored
    };

    let total_count = filtered.len() as u32;
    let truncated = total_count > options.limit;

    let memories: Vec<MemorySummary> = filtered
        .into_iter()
        .take(options.limit as usize)
        .map(|m| {
            let validity = memory_validity(&m.valid_from, &m.valid_to);
            let (content, content_truncated) = truncate_content(&m.content);
            MemorySummary {
                id: m.id,
                level: m.level,
                kind: m.kind,
                content,
                content_truncated,
                confidence: m.confidence,
                provenance_uri: m.provenance_uri,
                is_tombstoned: m.tombstoned_at.is_some(),
                valid_from: validity.valid_from,
                valid_to: validity.valid_to,
                validity_status: validity.status,
                validity_window_kind: validity.window_kind,
                created_at: m.created_at,
            }
        })
        .collect();

    MemoryListReport::success(memories, total_count, truncated, filter)
}

/// Stable schema name for `ee memory expire` reports.
pub const MEMORY_EXPIRE_SCHEMA_V1: &str = "ee.memory.expire.v1";

/// Stable schema name for `ee memory level` reports.
pub const MEMORY_LEVEL_SCHEMA_V1: &str = "ee.memory.level.v1";

/// Stable schema name for `ee memory tags` reports.
pub const MEMORY_TAGS_SCHEMA_V1: &str = "ee.memory.tags.v1";

/// Stable schema name for `ee memory link` reports.
pub const MEMORY_LINK_SCHEMA_V1: &str = "ee.memory.link.v1";

/// Options for expiring a memory without deleting it.
#[derive(Clone, Debug)]
pub struct ExpireMemoryOptions<'a> {
    /// Workspace path used to derive the canonical workspace ID.
    pub workspace_path: &'a Path,
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID to expire.
    pub memory_id: &'a str,
    /// Optional operator-supplied reason.
    pub reason: Option<&'a str>,
    /// Actor recorded in the audit row.
    pub actor: Option<&'a str>,
    /// Preview without writing.
    pub dry_run: bool,
    /// Treat already-tombstoned memories as visible for idempotency reporting.
    pub include_tombstoned: bool,
}

/// Report for `ee memory expire`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryExpireReport {
    /// Report schema.
    pub schema: &'static str,
    /// Package version for stable output.
    pub version: &'static str,
    /// Memory ID.
    pub memory_id: String,
    /// Workspace ID.
    pub workspace_id: String,
    /// Operation status.
    pub status: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether durable state changed.
    pub persisted: bool,
    /// Whether the command changed memory state or would change it in dry-run mode.
    pub changed: bool,
    /// Previous validity end timestamp, if any.
    pub previous_valid_to: Option<String>,
    /// Current validity end timestamp after the operation, if known.
    pub valid_to: Option<String>,
    /// Previous tombstone timestamp, if any.
    pub previous_tombstoned_at: Option<String>,
    /// Current tombstone timestamp after the operation, if known.
    pub tombstoned_at: Option<String>,
    /// Audit row ID when an expiration was committed.
    pub audit_id: Option<String>,
    /// Search-index job ID queued after a committed change.
    pub index_job_id: Option<String>,
    /// Stable index status string.
    pub index_status: String,
    /// Idempotency posture.
    pub idempotency: String,
}

/// Options for applying a canonical manual memory-level transition.
#[derive(Clone, Debug)]
pub struct MemoryLevelOptions<'a> {
    /// Workspace path used to derive the canonical workspace ID.
    pub workspace_path: &'a Path,
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID to transition.
    pub memory_id: &'a str,
    /// Target level (`working`, `episodic`, `semantic`, or `procedural`).
    pub level: &'a str,
    /// Optional compare-and-set source level.
    pub expected_level: Option<&'a str>,
    /// Operator-supplied transition reason. Required for manual transitions.
    pub reason: Option<&'a str>,
    /// Actor recorded in audit rows.
    pub actor: Option<&'a str>,
    /// Preview without writing.
    pub dry_run: bool,
    /// Return a tombstoned-state report instead of hiding tombstoned memories.
    pub include_tombstoned: bool,
}

/// Report for `ee memory level`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryLevelReport {
    /// Report schema.
    pub schema: &'static str,
    /// Package version for stable output.
    pub version: &'static str,
    /// Memory ID.
    pub memory_id: String,
    /// Workspace ID.
    pub workspace_id: String,
    /// Operation status.
    pub status: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether durable state changed.
    pub persisted: bool,
    /// Whether the command changed memory state or would change it in dry-run mode.
    pub changed: bool,
    /// Previous level before the transition.
    pub previous_level: String,
    /// Final or previewed level.
    pub level: String,
    /// Canonical transition event.
    pub event: Option<String>,
    /// Canonical transition reason.
    pub reason: Option<String>,
    /// Whether the transition is automatic.
    pub automatic: bool,
    /// Evidence references written to the audit row.
    pub evidence_refs: Vec<String>,
    /// Audit row ID when a transition was committed.
    pub audit_id: Option<String>,
    /// Search-index job ID queued after a committed transition.
    pub index_job_id: Option<String>,
    /// Stable index status string.
    pub index_status: String,
    /// Idempotency posture.
    pub idempotency: String,
}

/// Requested tag mutation mode for a memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryTagsMode {
    /// No mutation; list existing tags.
    List,
    /// Add/remove the provided tag sets.
    Patch {
        /// Tags to add.
        add: Vec<String>,
        /// Tags to remove.
        remove: Vec<String>,
    },
    /// Replace all tags with this exact set.
    Set(Vec<String>),
    /// Remove all tags.
    Clear,
}

/// Options for listing or mutating memory tags.
#[derive(Clone, Debug)]
pub struct MemoryTagsOptions<'a> {
    /// Workspace path used to derive the canonical workspace ID.
    pub workspace_path: &'a Path,
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID.
    pub memory_id: &'a str,
    /// Requested mode.
    pub mode: MemoryTagsMode,
    /// Actor recorded in audit rows.
    pub actor: Option<&'a str>,
    /// Preview without writing.
    pub dry_run: bool,
    /// Allow read-only listing for tombstoned memories.
    pub include_tombstoned: bool,
}

/// Report for `ee memory tags`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryTagsReport {
    /// Report schema.
    pub schema: &'static str,
    /// Package version for stable output.
    pub version: &'static str,
    /// Memory ID.
    pub memory_id: String,
    /// Workspace ID.
    pub workspace_id: String,
    /// Operation status.
    pub status: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether durable state changed.
    pub persisted: bool,
    /// Whether the command changed memory state or would change it in dry-run mode.
    pub changed: bool,
    /// Previous canonical tags.
    pub previous_tags: Vec<String>,
    /// Final or previewed canonical tags.
    pub tags: Vec<String>,
    /// Effective tags added by the request.
    pub added_tags: Vec<String>,
    /// Effective tags removed by the request.
    pub removed_tags: Vec<String>,
    /// Audit row IDs when a change was committed.
    pub audit_ids: Vec<String>,
    /// Search-index job ID queued after a committed change.
    pub index_job_id: Option<String>,
    /// Stable index status string.
    pub index_status: String,
    /// Idempotency posture.
    pub idempotency: String,
}

/// Requested link operation for a memory.
#[derive(Clone, Debug, PartialEq)]
pub enum MemoryLinkMode {
    /// List links incident to the memory, optionally filtered by relation.
    List {
        /// Optional relation filter.
        relation: Option<MemoryLinkRelation>,
    },
    /// Create a link from the memory to a target memory.
    Create {
        /// Target memory ID.
        target_memory_id: String,
        /// Typed relation.
        relation: MemoryLinkRelation,
        /// Link weight from 0.0 to 1.0.
        weight: f32,
        /// Confidence from 0.0 to 1.0.
        confidence: f32,
        /// Whether the edge is directed.
        directed: bool,
        /// Count of supporting evidence spans.
        evidence_count: u32,
        /// Link source.
        source: MemoryLinkSource,
        /// Optional JSON metadata.
        metadata_json: Option<String>,
    },
}

/// Options for listing or creating memory links.
#[derive(Clone, Debug)]
pub struct MemoryLinkOptions<'a> {
    /// Workspace path used to derive the canonical workspace ID.
    pub workspace_path: &'a Path,
    /// Database path.
    pub database_path: &'a Path,
    /// Source or incident memory ID.
    pub memory_id: &'a str,
    /// Requested operation.
    pub mode: MemoryLinkMode,
    /// Actor recorded in audit rows.
    pub actor: Option<&'a str>,
    /// Preview without writing.
    pub dry_run: bool,
    /// Allow read-only listing for tombstoned memories.
    pub include_tombstoned: bool,
}

/// Stable memory-link item used by `ee memory link` output.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MemoryLinkItem {
    /// Durable link ID. Dry-run planned links have no ID yet.
    pub link_id: Option<String>,
    /// Source memory ID.
    pub source_memory_id: String,
    /// Target memory ID.
    pub target_memory_id: String,
    /// Relation string.
    pub relation: String,
    /// Whether the link is directed.
    pub directed: bool,
    /// Link weight rounded for stable JSON output.
    pub weight: f64,
    /// Link confidence rounded for stable JSON output.
    pub confidence: f64,
    /// Evidence count.
    pub evidence_count: u32,
    /// Link source string.
    pub source: String,
    /// Created timestamp for persisted links.
    pub created_at: Option<String>,
    /// Creator recorded on the link row.
    pub created_by: Option<String>,
}

/// Report for `ee memory link`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MemoryLinkReport {
    /// Report schema.
    pub schema: &'static str,
    /// Package version for stable output.
    pub version: &'static str,
    /// Source or incident memory ID.
    pub memory_id: String,
    /// Workspace ID.
    pub workspace_id: String,
    /// Operation status.
    pub status: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether durable state changed.
    pub persisted: bool,
    /// Whether the command changed state or would change it in dry-run mode.
    pub changed: bool,
    /// Incident or resulting links in deterministic order.
    pub links: Vec<MemoryLinkItem>,
    /// Created, planned, or existing link for create mode.
    pub link: Option<MemoryLinkItem>,
    /// Audit row ID when a link was committed.
    pub audit_id: Option<String>,
    /// Idempotency posture.
    pub idempotency: String,
}

fn memory_command_storage_error(message: impl Into<String>) -> DomainError {
    DomainError::Storage {
        message: message.into(),
        repair: Some("ee doctor".to_owned()),
    }
}

fn memory_command_not_found(memory_id: &str) -> DomainError {
    DomainError::NotFound {
        resource: "memory".to_owned(),
        id: memory_id.to_owned(),
        repair: Some("ee memory list".to_owned()),
    }
}

fn memory_command_workspace_id(workspace_path: &Path) -> String {
    let workspace_path = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    stable_workspace_id(&workspace_path)
}

fn get_memory_for_workspace(
    conn: &DbConnection,
    memory_id: &str,
    workspace_id: &str,
) -> Result<StoredMemory, DomainError> {
    let memory = conn
        .get_memory(memory_id)
        .map_err(|error| memory_command_storage_error(format!("Failed to query memory: {error}")))?
        .ok_or_else(|| memory_command_not_found(memory_id))?;

    if memory.workspace_id != workspace_id {
        return Err(memory_command_not_found(memory_id));
    }

    Ok(memory)
}

fn expire_audit_details(reason: Option<&str>) -> String {
    serde_json::json!({
        "schema": "ee.audit.memory_expire.v1",
        "reason": reason,
        "deletion": "none_valid_to_only",
    })
    .to_string()
}

/// Expire a memory by setting its validity end timestamp. No files or rows are deleted.
pub fn expire_memory(options: &ExpireMemoryOptions<'_>) -> Result<MemoryExpireReport, DomainError> {
    let conn = open_migrated_memory_database(options.database_path)
        .map_err(memory_command_storage_error)?;
    let workspace_id = memory_command_workspace_id(options.workspace_path);
    let memory = get_memory_for_workspace(&conn, options.memory_id, &workspace_id)?;
    let expires_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    if memory.tombstoned_at.is_some() {
        if !options.include_tombstoned {
            return Err(DomainError::PolicyDenied {
                message: "Memory is tombstoned and cannot be expired.".to_owned(),
                repair: Some("Use ee memory show to inspect the tombstoned memory.".to_owned()),
            });
        }

        return Ok(MemoryExpireReport {
            schema: MEMORY_EXPIRE_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "already_expired".to_owned(),
            dry_run: options.dry_run,
            persisted: false,
            changed: false,
            previous_valid_to: memory.valid_to.clone(),
            valid_to: memory.valid_to,
            previous_tombstoned_at: memory.tombstoned_at.clone(),
            tombstoned_at: memory.tombstoned_at,
            audit_id: None,
            index_job_id: None,
            index_status: "not_scheduled".to_owned(),
            idempotency: "no_change".to_owned(),
        });
    }
    if memory_validity(&memory.valid_from, &memory.valid_to).status == "expired" {
        return Ok(MemoryExpireReport {
            schema: MEMORY_EXPIRE_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "already_expired".to_owned(),
            dry_run: options.dry_run,
            persisted: false,
            changed: false,
            previous_valid_to: memory.valid_to.clone(),
            valid_to: memory.valid_to,
            previous_tombstoned_at: None,
            tombstoned_at: None,
            audit_id: None,
            index_job_id: None,
            index_status: "not_scheduled".to_owned(),
            idempotency: "no_change".to_owned(),
        });
    }

    if options.dry_run {
        return Ok(MemoryExpireReport {
            schema: MEMORY_EXPIRE_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "would_expire".to_owned(),
            dry_run: true,
            persisted: false,
            changed: true,
            previous_valid_to: memory.valid_to,
            valid_to: Some(expires_at),
            previous_tombstoned_at: None,
            tombstoned_at: None,
            audit_id: None,
            index_job_id: None,
            index_status: "dry_run_not_queued".to_owned(),
            idempotency: "would_change".to_owned(),
        });
    }

    let audit_id = generate_audit_id();
    let actor = options.actor.or(Some("ee memory expire"));
    let details = expire_audit_details(options.reason);
    let index_job_id = generate_search_index_job_id();
    let index_input = CreateSearchIndexJobInput {
        workspace_id: workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(options.memory_id.to_owned()),
        documents_total: 1,
    };

    conn.with_transaction(|| {
        let expired = conn.expire_memory_valid_to(options.memory_id, &expires_at)?;
        if !expired {
            return Ok(None);
        }
        conn.insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.clone()),
                actor: actor.map(str::to_owned),
                action: audit_actions::MEMORY_EXPIRE.to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(options.memory_id.to_owned()),
                details: Some(details.clone()),
            },
        )?;
        if memory.level == "semantic" {
            let mut evidence_refs = vec![expires_at.clone()];
            if let Some(reason) = options.reason {
                evidence_refs.push(reason.to_owned());
            }
            let _ = conn.apply_memory_level_transition_in_current_transaction(
                options.memory_id,
                &ApplyMemoryLevelTransitionInput {
                    workspace_id: workspace_id.clone(),
                    expected_level: Some(memory.level.clone()),
                    level: "episodic".to_owned(),
                    updated_at: expires_at.clone(),
                    actor: actor.map(str::to_owned),
                    reason: "time_bound_fact".to_owned(),
                    automatic: true,
                    event: "valid_to.set".to_owned(),
                    evidence_refs,
                    source_action: Some(audit_actions::MEMORY_EXPIRE.to_owned()),
                },
            )?;
        }
        conn.insert_search_index_job(&index_job_id, &index_input)?;
        Ok(Some(()))
    })
    .map_err(|error| memory_command_storage_error(format!("Failed to expire memory: {error}")))?;

    let refreshed = conn
        .get_memory(options.memory_id)
        .map_err(|error| {
            memory_command_storage_error(format!("Failed to reload expired memory: {error}"))
        })?
        .ok_or_else(|| memory_command_not_found(options.memory_id))?;
    let refreshed_validity = memory_validity(&refreshed.valid_from, &refreshed.valid_to);
    let changed = refreshed_validity.status == "expired";

    Ok(MemoryExpireReport {
        schema: MEMORY_EXPIRE_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        memory_id: options.memory_id.to_owned(),
        workspace_id,
        status: if changed {
            "expired".to_owned()
        } else {
            "already_expired".to_owned()
        },
        dry_run: false,
        persisted: changed,
        changed,
        previous_valid_to: memory.valid_to,
        valid_to: refreshed.valid_to,
        previous_tombstoned_at: None,
        tombstoned_at: refreshed.tombstoned_at,
        audit_id: Some(audit_id),
        index_job_id: Some(index_job_id),
        index_status: "queued".to_owned(),
        idempotency: "changed".to_owned(),
    })
}

fn memory_lifecycle_state_from_level(level: &str) -> Option<MemoryLifecycleState> {
    match level {
        "working" => Some(MemoryLifecycleState::Working),
        "episodic" => Some(MemoryLifecycleState::Episodic),
        "semantic" => Some(MemoryLifecycleState::Semantic),
        "procedural" => Some(MemoryLifecycleState::Procedural),
        _ => None,
    }
}

fn manual_level_transition_event(
    previous_level: &str,
    target_level: &str,
) -> Result<&'static str, DomainError> {
    match (previous_level, target_level) {
        ("working", "episodic") => Ok("manual.promote_to_episodic"),
        ("episodic", "semantic") => Ok("manual.promote_to_semantic"),
        ("semantic", "procedural") => Ok("manual.promote_to_procedural"),
        ("procedural", "semantic") => Ok("manual.demote_to_semantic"),
        _ => Err(DomainError::Usage {
            message: format!(
                "Unsupported manual memory level transition: {previous_level} -> {target_level}."
            ),
            repair: Some(
                "Use the canonical adjacent transitions: working->episodic, episodic->semantic, semantic->procedural, or procedural->semantic.".to_owned(),
            ),
        }),
    }
}

fn required_manual_transition_reason(reason: Option<&str>) -> Result<String, DomainError> {
    reason
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| DomainError::UsageCodeWithDetails {
            code: LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE,
            message: "Manual memory level transition requires evidence via --reason.".to_owned(),
            repair: Some(
                "Use ee memory level <memory-id> --to episodic --reason \"workflow completed\"."
                    .to_owned(),
            ),
            details_json: serde_json::json!({
                "failureModeCode": LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE,
                "transitionSurface": "memory level",
                "missingEvidence": ["reason"],
                "requiredFlag": "--reason",
            })
            .to_string(),
        })
}

fn memory_level_target(level: &str) -> Result<MemoryLevel, DomainError> {
    MemoryLevel::from_str(level).map_err(|_| DomainError::Usage {
        message: format!("Unknown memory level: {level}"),
        repair: Some("Use one of: working, episodic, semantic, procedural.".to_owned()),
    })
}

fn level_transition_concurrent_conflict_error(
    memory_id: &str,
    planned_previous_level: &str,
    target_level: &str,
    observed: Option<&StoredMemory>,
) -> DomainError {
    DomainError::UsageCodeWithDetails {
        code: LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE,
        message: format!("Memory level transition for {memory_id} lost a concurrent update race."),
        repair: Some(format!(
            "Run ee memory show {memory_id} --json, then retry the transition from the current level."
        )),
        details_json: serde_json::json!({
            "failureModeCode": LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE,
            "transitionSurface": "memory level",
            "memoryId": memory_id,
            "plannedPreviousLevel": planned_previous_level,
            "targetLevel": target_level,
            "observedLevel": observed.map(|memory| memory.level.clone()),
            "observedTombstonedAt": observed.and_then(|memory| memory.tombstoned_at.clone()),
        })
        .to_string(),
    }
}

/// Apply a manual memory-level transition using the canonical lifecycle table.
pub fn update_memory_level(
    options: &MemoryLevelOptions<'_>,
) -> Result<MemoryLevelReport, DomainError> {
    let target_level = memory_level_target(options.level)?;
    let target_level = target_level.as_str();
    let expected_level = options
        .expected_level
        .map(memory_level_target)
        .transpose()?
        .map(|level| level.as_str().to_owned());
    let conn = open_migrated_memory_database(options.database_path)
        .map_err(memory_command_storage_error)?;
    let workspace_id = memory_command_workspace_id(options.workspace_path);
    let memory = get_memory_for_workspace(&conn, options.memory_id, &workspace_id)?;

    if memory.tombstoned_at.is_some() {
        if options.include_tombstoned {
            return Ok(MemoryLevelReport {
                schema: MEMORY_LEVEL_SCHEMA_V1,
                version: env!("CARGO_PKG_VERSION"),
                memory_id: options.memory_id.to_owned(),
                workspace_id,
                status: "tombstoned".to_owned(),
                dry_run: options.dry_run,
                persisted: false,
                changed: false,
                previous_level: memory.level.clone(),
                level: memory.level,
                event: None,
                reason: None,
                automatic: false,
                evidence_refs: Vec::new(),
                audit_id: None,
                index_job_id: None,
                index_status: "not_scheduled".to_owned(),
                idempotency: "no_change".to_owned(),
            });
        }

        return Err(DomainError::UsageCodeWithDetails {
            code: LEVEL_TRANSITION_TOMBSTONED_REJECTED_CODE,
            message: "Memory is tombstoned and cannot change level.".to_owned(),
            repair: Some("Use ee memory history to inspect the tombstone, then ee curate untombstone before applying a level transition.".to_owned()),
            details_json: serde_json::json!({
                "failureModeCode": LEVEL_TRANSITION_TOMBSTONED_REJECTED_CODE,
                "transitionSurface": "memory level",
                "memoryId": options.memory_id,
                "currentLevel": memory.level,
                "targetLevel": target_level,
                "tombstonedAt": memory.tombstoned_at,
            })
            .to_string(),
        });
    }

    let planned_previous_level = expected_level.unwrap_or_else(|| memory.level.clone());
    if memory.level != planned_previous_level {
        return Err(level_transition_concurrent_conflict_error(
            options.memory_id,
            &planned_previous_level,
            target_level,
            Some(&memory),
        ));
    }

    if memory.level == target_level {
        return Ok(MemoryLevelReport {
            schema: MEMORY_LEVEL_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "already_level".to_owned(),
            dry_run: options.dry_run,
            persisted: false,
            changed: false,
            previous_level: memory.level.clone(),
            level: memory.level,
            event: None,
            reason: None,
            automatic: false,
            evidence_refs: Vec::new(),
            audit_id: None,
            index_job_id: None,
            index_status: "not_scheduled".to_owned(),
            idempotency: "no_change".to_owned(),
        });
    }

    let manual_reason = required_manual_transition_reason(options.reason)?;
    let event = manual_level_transition_event(&planned_previous_level, target_level)?;
    let from_state =
        memory_lifecycle_state_from_level(&planned_previous_level).ok_or_else(|| {
            DomainError::Storage {
                message: format!("Memory has unknown stored level: {planned_previous_level}"),
                repair: Some("Run ee doctor --json to inspect database consistency.".to_owned()),
            }
        })?;
    let transition = transition_for(from_state, event).ok_or_else(|| DomainError::Usage {
        message: format!(
            "No canonical memory lifecycle transition exists for {planned_previous_level} via {event}."
        ),
        repair: Some("See docs for the memory level lifecycle transition table.".to_owned()),
    })?;
    let actor = options.actor.or(Some("ee memory level"));
    let evidence_refs = vec![
        format!("actor:{}", actor.unwrap_or("ee memory level")),
        format!("reason:{manual_reason}"),
    ];

    if options.dry_run {
        return Ok(MemoryLevelReport {
            schema: MEMORY_LEVEL_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "would_transition".to_owned(),
            dry_run: true,
            persisted: false,
            changed: true,
            previous_level: planned_previous_level,
            level: target_level.to_owned(),
            event: Some(event.to_owned()),
            reason: Some(transition.reason.to_owned()),
            automatic: transition.automatic,
            evidence_refs,
            audit_id: None,
            index_job_id: None,
            index_status: "dry_run_not_queued".to_owned(),
            idempotency: "would_change".to_owned(),
        });
    }

    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let index_job_id = generate_search_index_job_id();
    let index_input = CreateSearchIndexJobInput {
        workspace_id: workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(options.memory_id.to_owned()),
        documents_total: 1,
    };

    let audit_id = conn
        .with_transaction(|| {
            let audit_id = conn.apply_memory_level_transition_in_current_transaction(
                options.memory_id,
                &ApplyMemoryLevelTransitionInput {
                    workspace_id: workspace_id.clone(),
                    expected_level: Some(planned_previous_level.clone()),
                    level: target_level.to_owned(),
                    updated_at: updated_at.clone(),
                    actor: actor.map(str::to_owned),
                    reason: transition.reason.to_owned(),
                    automatic: transition.automatic,
                    event: event.to_owned(),
                    evidence_refs: evidence_refs.clone(),
                    source_action: Some("memory.level".to_owned()),
                },
            )?;
            if audit_id.is_some() {
                conn.insert_search_index_job(&index_job_id, &index_input)?;
            }
            Ok(audit_id)
        })
        .map_err(|error| {
            memory_command_storage_error(format!("Failed to transition memory level: {error}"))
        })?;

    if audit_id.is_none() {
        let observed = conn.get_memory(options.memory_id).map_err(|error| {
            memory_command_storage_error(format!(
                "Failed to reload memory after concurrent transition conflict: {error}"
            ))
        })?;
        return Err(level_transition_concurrent_conflict_error(
            options.memory_id,
            &planned_previous_level,
            target_level,
            observed.as_ref(),
        ));
    }

    Ok(MemoryLevelReport {
        schema: MEMORY_LEVEL_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        memory_id: options.memory_id.to_owned(),
        workspace_id,
        status: "transitioned".to_owned(),
        dry_run: false,
        persisted: true,
        changed: true,
        previous_level: planned_previous_level,
        level: target_level.to_owned(),
        event: Some(event.to_owned()),
        reason: Some(transition.reason.to_owned()),
        automatic: transition.automatic,
        evidence_refs,
        audit_id,
        index_job_id: Some(index_job_id),
        index_status: "queued".to_owned(),
        idempotency: "changed".to_owned(),
    })
}

fn unique_sorted_tags(tags: impl IntoIterator<Item = String>) -> Vec<String> {
    tags.into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn tag_difference(left: &[String], right: &[String]) -> Vec<String> {
    let right: BTreeSet<&String> = right.iter().collect();
    left.iter()
        .filter(|tag| !right.contains(tag))
        .cloned()
        .collect()
}

fn tag_patch_result(current: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let remove_set: BTreeSet<&String> = remove.iter().collect();
    let kept = current
        .iter()
        .filter(|tag| !remove_set.contains(tag))
        .cloned();
    unique_sorted_tags(kept.chain(add.iter().cloned()))
}

fn tag_audit_action(mode: &MemoryTagsMode, added: &[String], removed: &[String]) -> String {
    match mode {
        MemoryTagsMode::Patch { .. } if !added.is_empty() && removed.is_empty() => {
            audit_actions::MEMORY_TAG_ADD.to_owned()
        }
        MemoryTagsMode::Patch { .. } if added.is_empty() && !removed.is_empty() => {
            audit_actions::MEMORY_TAG_REMOVE.to_owned()
        }
        _ => audit_actions::MEMORY_TAG_SET.to_owned(),
    }
}

fn tag_audit_details(
    previous: &[String],
    next: &[String],
    added: &[String],
    removed: &[String],
) -> String {
    serde_json::json!({
        "schema": "ee.audit.memory_tags.v1",
        "previous_tags": previous,
        "tags": next,
        "added_tags": added,
        "removed_tags": removed,
    })
    .to_string()
}

fn validate_memory_link_unit_score(label: &str, value: f32) -> Result<(), DomainError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(DomainError::Usage {
            message: format!("{label} must be a finite number from 0.0 to 1.0."),
            repair: Some(format!("Use --{label} 0.8.")),
        })
    }
}

fn validate_memory_link_metadata(metadata_json: Option<&str>) -> Result<(), DomainError> {
    if let Some(metadata_json) = metadata_json {
        serde_json::from_str::<serde_json::Value>(metadata_json).map_err(|error| {
            DomainError::Usage {
                message: format!("Invalid memory link metadata JSON: {error}"),
                repair: Some("Use --metadata '{\"reason\":\"explicit\"}'.".to_owned()),
            }
        })?;
    }
    Ok(())
}

fn memory_link_score_output(value: f32) -> f64 {
    (f64::from(value) * 1_000_000.0).round() / 1_000_000.0
}

fn stored_memory_link_item(link: &StoredMemoryLink) -> MemoryLinkItem {
    MemoryLinkItem {
        link_id: Some(link.id.clone()),
        source_memory_id: link.src_memory_id.clone(),
        target_memory_id: link.dst_memory_id.clone(),
        relation: link.relation.clone(),
        directed: link.directed,
        weight: memory_link_score_output(link.weight),
        confidence: memory_link_score_output(link.confidence),
        evidence_count: link.evidence_count,
        source: link.source.clone(),
        created_at: Some(link.created_at.clone()),
        created_by: link.created_by.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn planned_memory_link_item(
    source_memory_id: &str,
    target_memory_id: &str,
    relation: MemoryLinkRelation,
    directed: bool,
    weight: f32,
    confidence: f32,
    evidence_count: u32,
    source: MemoryLinkSource,
    created_by: Option<&str>,
) -> MemoryLinkItem {
    MemoryLinkItem {
        link_id: None,
        source_memory_id: source_memory_id.to_owned(),
        target_memory_id: target_memory_id.to_owned(),
        relation: relation.as_str().to_owned(),
        directed,
        weight: memory_link_score_output(weight),
        confidence: memory_link_score_output(confidence),
        evidence_count,
        source: source.as_str().to_owned(),
        created_at: None,
        created_by: created_by.map(str::to_owned),
    }
}

fn existing_memory_link_for_create(
    conn: &DbConnection,
    source_memory_id: &str,
    target_memory_id: &str,
    relation: MemoryLinkRelation,
    directed: bool,
) -> Result<Option<StoredMemoryLink>, DomainError> {
    let links = conn
        .list_memory_links_for_memory(source_memory_id, Some(relation))
        .map_err(|error| {
            memory_command_storage_error(format!("Failed to query memory links: {error}"))
        })?;

    Ok(links.into_iter().find(|link| {
        let exact_direction =
            link.src_memory_id == source_memory_id && link.dst_memory_id == target_memory_id;
        let undirected_equivalent = (!directed || !link.directed)
            && link.src_memory_id == target_memory_id
            && link.dst_memory_id == source_memory_id;
        exact_direction || undirected_equivalent
    }))
}

fn memory_link_audit_details(link: &MemoryLinkItem, metadata_json: Option<&str>) -> String {
    serde_json::json!({
        "schema": "ee.audit.memory_link.v1",
        "linkId": link.link_id,
        "sourceMemoryId": link.source_memory_id,
        "targetMemoryId": link.target_memory_id,
        "relation": link.relation,
        "directed": link.directed,
        "weight": link.weight,
        "confidence": link.confidence,
        "evidenceCount": link.evidence_count,
        "source": link.source,
        "metadata": metadata_json.and_then(|metadata| {
            serde_json::from_str::<serde_json::Value>(metadata).ok()
        }),
    })
    .to_string()
}

/// List or create durable memory links through the source-of-truth DB table.
pub fn update_memory_link(
    options: &MemoryLinkOptions<'_>,
) -> Result<MemoryLinkReport, DomainError> {
    let conn = open_migrated_memory_database(options.database_path)
        .map_err(memory_command_storage_error)?;
    let workspace_id = memory_command_workspace_id(options.workspace_path);
    let source_memory = get_memory_for_workspace(&conn, options.memory_id, &workspace_id)?;

    match &options.mode {
        MemoryLinkMode::List { relation } => {
            if source_memory.tombstoned_at.is_some() && !options.include_tombstoned {
                return Err(DomainError::NotFound {
                    resource: "memory".to_owned(),
                    id: options.memory_id.to_owned(),
                    repair: Some("Use ee memory link <id> --include-tombstoned.".to_owned()),
                });
            }

            let links = conn
                .list_memory_links_for_memory(options.memory_id, *relation)
                .map_err(|error| {
                    memory_command_storage_error(format!("Failed to query memory links: {error}"))
                })?
                .iter()
                .map(stored_memory_link_item)
                .collect::<Vec<_>>();

            Ok(MemoryLinkReport {
                schema: MEMORY_LINK_SCHEMA_V1,
                version: env!("CARGO_PKG_VERSION"),
                memory_id: options.memory_id.to_owned(),
                workspace_id,
                status: "listed".to_owned(),
                dry_run: options.dry_run,
                persisted: false,
                changed: false,
                links,
                link: None,
                audit_id: None,
                idempotency: "read_only".to_owned(),
            })
        }
        MemoryLinkMode::Create {
            target_memory_id,
            relation,
            weight,
            confidence,
            directed,
            evidence_count,
            source,
            metadata_json,
        } => {
            validate_memory_link_unit_score("weight", *weight)?;
            validate_memory_link_unit_score("confidence", *confidence)?;
            validate_memory_link_metadata(metadata_json.as_deref())?;

            if options.memory_id == target_memory_id {
                return Err(DomainError::Usage {
                    message: "Memory links cannot target the same memory as their source."
                        .to_owned(),
                    repair: Some("Use two different memory IDs.".to_owned()),
                });
            }

            let target_memory = get_memory_for_workspace(&conn, target_memory_id, &workspace_id)?;
            if source_memory.tombstoned_at.is_some() || target_memory.tombstoned_at.is_some() {
                return Err(DomainError::PolicyDenied {
                    message: "Cannot create memory links involving expired memories.".to_owned(),
                    repair: Some(
                        "Use ee memory show --include-tombstoned to inspect them.".to_owned(),
                    ),
                });
            }

            if let Some(existing) = existing_memory_link_for_create(
                &conn,
                options.memory_id,
                target_memory_id,
                *relation,
                *directed,
            )? {
                let item = stored_memory_link_item(&existing);
                return Ok(MemoryLinkReport {
                    schema: MEMORY_LINK_SCHEMA_V1,
                    version: env!("CARGO_PKG_VERSION"),
                    memory_id: options.memory_id.to_owned(),
                    workspace_id,
                    status: "already_exists".to_owned(),
                    dry_run: options.dry_run,
                    persisted: false,
                    changed: false,
                    links: vec![item.clone()],
                    link: Some(item),
                    audit_id: None,
                    idempotency: "no_change".to_owned(),
                });
            }

            let created_by = options.actor.or(Some("ee memory link"));
            let planned = planned_memory_link_item(
                options.memory_id,
                target_memory_id,
                *relation,
                *directed,
                *weight,
                *confidence,
                *evidence_count,
                *source,
                created_by,
            );

            if options.dry_run {
                return Ok(MemoryLinkReport {
                    schema: MEMORY_LINK_SCHEMA_V1,
                    version: env!("CARGO_PKG_VERSION"),
                    memory_id: options.memory_id.to_owned(),
                    workspace_id,
                    status: "would_create".to_owned(),
                    dry_run: true,
                    persisted: false,
                    changed: true,
                    links: vec![planned.clone()],
                    link: Some(planned),
                    audit_id: None,
                    idempotency: "would_change".to_owned(),
                });
            }

            let link_id = generate_memory_link_id();
            let audit_id = generate_audit_id();
            let input = CreateMemoryLinkInput {
                src_memory_id: options.memory_id.to_owned(),
                dst_memory_id: target_memory_id.clone(),
                relation: *relation,
                weight: *weight,
                confidence: *confidence,
                directed: *directed,
                evidence_count: *evidence_count,
                last_reinforced_at: None,
                source: *source,
                created_by: created_by.map(str::to_owned),
                metadata_json: metadata_json.clone(),
            };
            let audit_link = MemoryLinkItem {
                link_id: Some(link_id.clone()),
                ..planned
            };
            let audit_details = memory_link_audit_details(&audit_link, metadata_json.as_deref());

            conn.with_transaction(|| {
                conn.insert_memory_link(&link_id, &input)?;
                conn.insert_audit(
                    &audit_id,
                    &CreateAuditInput {
                        workspace_id: Some(workspace_id.clone()),
                        actor: created_by.map(str::to_owned),
                        action: audit_actions::MEMORY_LINK_CREATE.to_owned(),
                        target_type: Some("memory_link".to_owned()),
                        target_id: Some(link_id.clone()),
                        details: Some(audit_details.clone()),
                    },
                )
            })
            .map_err(|error| {
                memory_command_storage_error(format!("Failed to create memory link: {error}"))
            })?;

            let created = conn
                .get_memory_link(&link_id)
                .map_err(|error| {
                    memory_command_storage_error(format!("Failed to reload memory link: {error}"))
                })?
                .ok_or_else(|| {
                    memory_command_storage_error("Failed to reload memory link after creation")
                })?;
            let item = stored_memory_link_item(&created);

            Ok(MemoryLinkReport {
                schema: MEMORY_LINK_SCHEMA_V1,
                version: env!("CARGO_PKG_VERSION"),
                memory_id: options.memory_id.to_owned(),
                workspace_id,
                status: "created".to_owned(),
                dry_run: false,
                persisted: true,
                changed: true,
                links: vec![item.clone()],
                link: Some(item),
                audit_id: Some(audit_id),
                idempotency: "changed".to_owned(),
            })
        }
    }
}

/// List or mutate tags for a memory.
pub fn update_memory_tags(
    options: &MemoryTagsOptions<'_>,
) -> Result<MemoryTagsReport, DomainError> {
    let conn = open_migrated_memory_database(options.database_path)
        .map_err(memory_command_storage_error)?;
    let workspace_id = memory_command_workspace_id(options.workspace_path);
    let memory = get_memory_for_workspace(&conn, options.memory_id, &workspace_id)?;

    if memory.tombstoned_at.is_some() {
        if matches!(options.mode, MemoryTagsMode::List) {
            if !options.include_tombstoned {
                return Err(DomainError::NotFound {
                    resource: "memory".to_owned(),
                    id: options.memory_id.to_owned(),
                    repair: Some("Use ee memory tags <id> --include-tombstoned.".to_owned()),
                });
            }
        } else {
            return Err(DomainError::PolicyDenied {
                message: "Cannot mutate tags on an expired memory.".to_owned(),
                repair: Some("Use ee memory show --include-tombstoned to inspect it.".to_owned()),
            });
        }
    }

    let current_tags = conn.get_memory_tags(options.memory_id).map_err(|error| {
        memory_command_storage_error(format!("Failed to query memory tags: {error}"))
    })?;

    if matches!(options.mode, MemoryTagsMode::List) {
        return Ok(MemoryTagsReport {
            schema: MEMORY_TAGS_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "listed".to_owned(),
            dry_run: options.dry_run,
            persisted: false,
            changed: false,
            previous_tags: current_tags.clone(),
            tags: current_tags,
            added_tags: Vec::new(),
            removed_tags: Vec::new(),
            audit_ids: Vec::new(),
            index_job_id: None,
            index_status: "not_scheduled".to_owned(),
            idempotency: "read_only".to_owned(),
        });
    }

    let next_tags = match &options.mode {
        MemoryTagsMode::List => current_tags.clone(),
        MemoryTagsMode::Patch { add, remove } => tag_patch_result(&current_tags, add, remove),
        MemoryTagsMode::Set(tags) => tags.clone(),
        MemoryTagsMode::Clear => Vec::new(),
    };
    let next_tags = unique_sorted_tags(next_tags);
    let added_tags = tag_difference(&next_tags, &current_tags);
    let removed_tags = tag_difference(&current_tags, &next_tags);
    let changed = !added_tags.is_empty() || !removed_tags.is_empty();

    if !changed {
        return Ok(MemoryTagsReport {
            schema: MEMORY_TAGS_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "unchanged".to_owned(),
            dry_run: options.dry_run,
            persisted: false,
            changed: false,
            previous_tags: current_tags.clone(),
            tags: current_tags,
            added_tags,
            removed_tags,
            audit_ids: Vec::new(),
            index_job_id: None,
            index_status: if options.dry_run {
                "dry_run_not_queued".to_owned()
            } else {
                "not_scheduled".to_owned()
            },
            idempotency: "no_change".to_owned(),
        });
    }

    if options.dry_run {
        return Ok(MemoryTagsReport {
            schema: MEMORY_TAGS_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            memory_id: options.memory_id.to_owned(),
            workspace_id,
            status: "would_update".to_owned(),
            dry_run: true,
            persisted: false,
            changed: true,
            previous_tags: current_tags,
            tags: next_tags,
            added_tags,
            removed_tags,
            audit_ids: Vec::new(),
            index_job_id: None,
            index_status: "dry_run_not_queued".to_owned(),
            idempotency: "would_change".to_owned(),
        });
    }

    let audit_id = generate_audit_id();
    let index_job_id = generate_search_index_job_id();
    let audit_action = tag_audit_action(&options.mode, &added_tags, &removed_tags);
    let audit_details = tag_audit_details(&current_tags, &next_tags, &added_tags, &removed_tags);
    let actor = options.actor.or(Some("ee memory tags"));
    let index_input = CreateSearchIndexJobInput {
        workspace_id: workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(options.memory_id.to_owned()),
        documents_total: 1,
    };

    conn.with_transaction(|| {
        if !removed_tags.is_empty() {
            conn.remove_memory_tags(options.memory_id, &removed_tags)?;
        }
        if !added_tags.is_empty() {
            conn.add_memory_tags(options.memory_id, &added_tags)?;
        }
        conn.insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.clone()),
                actor: actor.map(str::to_owned),
                action: audit_action.clone(),
                target_type: Some("memory".to_owned()),
                target_id: Some(options.memory_id.to_owned()),
                details: Some(audit_details.clone()),
            },
        )?;
        conn.insert_search_index_job(&index_job_id, &index_input)
    })
    .map_err(|error| {
        memory_command_storage_error(format!("Failed to update memory tags: {error}"))
    })?;

    let final_tags = conn.get_memory_tags(options.memory_id).map_err(|error| {
        memory_command_storage_error(format!("Failed to reload memory tags: {error}"))
    })?;

    Ok(MemoryTagsReport {
        schema: MEMORY_TAGS_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        memory_id: options.memory_id.to_owned(),
        workspace_id,
        status: "updated".to_owned(),
        dry_run: false,
        persisted: true,
        changed: true,
        previous_tags: current_tags,
        tags: final_tags,
        added_tags,
        removed_tags,
        audit_ids: vec![audit_id],
        index_job_id: Some(index_job_id),
        index_status: "queued".to_owned(),
        idempotency: "changed".to_owned(),
    })
}

/// Options for retrieving memory history.
#[derive(Clone, Debug)]
pub struct GetMemoryHistoryOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID to retrieve history for.
    pub memory_id: &'a str,
    /// Maximum number of history entries to return.
    pub limit: u32,
}

/// A single entry in the memory history timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryHistoryEntry {
    /// Audit entry ID.
    pub audit_id: String,
    /// Timestamp of the event.
    pub timestamp: String,
    /// Actor who performed the action (if known).
    pub actor: Option<String>,
    /// Action performed (e.g., "create", "update", "tombstone").
    pub action: String,
    /// Details about the change (JSON string if available).
    pub details: Option<String>,
}

/// Result of a memory history operation.
#[derive(Clone, Debug)]
pub struct MemoryHistoryReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Memory ID for which history was requested.
    pub memory_id: String,
    /// Whether the memory exists.
    pub memory_exists: bool,
    /// Whether the memory is tombstoned.
    pub is_tombstoned: bool,
    /// History entries ordered from newest to oldest.
    pub entries: Vec<MemoryHistoryEntry>,
    /// Total number of history entries for this memory.
    pub total_count: u32,
    /// Whether results were truncated due to limit.
    pub truncated: bool,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

impl MemoryHistoryReport {
    /// Create a report for a found memory with history.
    #[must_use]
    pub fn found(
        memory_id: String,
        is_tombstoned: bool,
        entries: Vec<MemoryHistoryEntry>,
        total_count: u32,
        truncated: bool,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            memory_exists: true,
            is_tombstoned,
            entries,
            total_count,
            truncated,
            error: None,
        }
    }

    /// Create a report for a not-found memory.
    #[must_use]
    pub fn not_found(memory_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            memory_exists: false,
            is_tombstoned: false,
            entries: Vec::new(),
            total_count: 0,
            truncated: false,
            error: None,
        }
    }

    /// Create a report for a database error.
    #[must_use]
    pub fn error(memory_id: String, message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            memory_exists: false,
            is_tombstoned: false,
            entries: Vec::new(),
            total_count: 0,
            truncated: false,
            error: Some(message),
        }
    }
}

/// Retrieve the history of a memory by querying audit log entries.
///
/// Returns all audit entries for the specified memory, ordered from newest to oldest.
/// If the memory does not exist, returns a not-found report.
pub fn get_memory_history(options: &GetMemoryHistoryOptions<'_>) -> MemoryHistoryReport {
    let conn = match open_migrated_memory_database(options.database_path) {
        Ok(c) => c,
        Err(message) => return MemoryHistoryReport::error(options.memory_id.to_string(), message),
    };

    // First check if memory exists
    let memory = match conn.get_memory(options.memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return MemoryHistoryReport::not_found(options.memory_id.to_string()),
        Err(e) => {
            return MemoryHistoryReport::error(
                options.memory_id.to_string(),
                format!("Failed to query memory: {e}"),
            );
        }
    };

    let is_tombstoned = memory.tombstoned_at.is_some();

    // Get audit entries for this memory
    let all_entries = match conn.list_audit_by_target("memory", options.memory_id, None) {
        Ok(entries) => entries,
        Err(e) => {
            return MemoryHistoryReport::error(
                options.memory_id.to_string(),
                format!("Failed to query audit log: {e}"),
            );
        }
    };

    let total_count = all_entries.len() as u32;
    let truncated = total_count > options.limit;

    let entries: Vec<MemoryHistoryEntry> = all_entries
        .into_iter()
        .take(options.limit as usize)
        .map(|e| MemoryHistoryEntry {
            audit_id: e.id,
            timestamp: e.timestamp,
            actor: e.actor,
            action: e.action,
            details: e.details,
        })
        .collect();

    MemoryHistoryReport::found(
        options.memory_id.to_string(),
        is_tombstoned,
        entries,
        total_count,
        truncated,
    )
}

// =============================================================================
// Memory Revise (EE-066)
//
// Immutable revision creates a new memory that supersedes an existing one.
// The original memory remains unchanged; a supersession link connects them.
// =============================================================================

/// Reason for revising a memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviseReason {
    /// Content was corrected or clarified.
    Correction,
    /// Content was updated with new information.
    Update,
    /// Content was refined for clarity.
    Refinement,
    /// Content was consolidated from multiple sources.
    Consolidation,
    /// Custom reason provided by the user.
    Custom(String),
}

impl ReviseReason {
    /// Stable wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Correction => "correction",
            Self::Update => "update",
            Self::Refinement => "refinement",
            Self::Consolidation => "consolidation",
            Self::Custom(s) => s.as_str(),
        }
    }

    /// Parse a reason string.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        match input {
            "correction" => Self::Correction,
            "update" => Self::Update,
            "refinement" => Self::Refinement,
            "consolidation" => Self::Consolidation,
            other => Self::Custom(other.to_owned()),
        }
    }
}

#[allow(clippy::derivable_impls)]
impl Default for ReviseReason {
    fn default() -> Self {
        Self::Update
    }
}

/// Options for revising a memory.
#[derive(Clone, Debug)]
pub struct ReviseMemoryOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// ID of the memory to revise.
    pub original_memory_id: &'a str,
    /// New content (if changing).
    pub content: Option<&'a str>,
    /// New level (if changing).
    pub level: Option<&'a str>,
    /// New kind (if changing).
    pub kind: Option<&'a str>,
    /// New confidence (if changing).
    pub confidence: Option<f32>,
    /// New tags (if changing).
    pub tags: Option<Vec<String>>,
    /// New provenance URI (if changing).
    pub provenance_uri: Option<&'a str>,
    /// Reason for the revision.
    pub reason: ReviseReason,
    /// Actor performing the revision.
    pub actor: Option<&'a str>,
    /// Whether to perform a dry run (no changes).
    pub dry_run: bool,
}

/// Result of a memory revise operation.
#[derive(Clone, Debug)]
pub struct MemoryReviseReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Whether the operation was a dry run.
    pub dry_run: bool,
    /// Whether the revision was successful.
    pub success: bool,
    /// Original memory ID that was revised.
    pub original_id: String,
    /// New memory ID (if created).
    pub new_id: Option<String>,
    /// Revision group ID linking all versions.
    pub revision_group_id: Option<String>,
    /// Revision number within the group.
    pub revision_number: Option<u32>,
    /// Reason for the revision.
    pub reason: String,
    /// Fields that were changed.
    pub changed_fields: Vec<String>,
    /// Error message if revision failed.
    pub error: Option<String>,
}

impl MemoryReviseReport {
    /// Create a successful revision report.
    #[must_use]
    pub fn success(
        original_id: String,
        new_id: String,
        revision_group_id: String,
        revision_number: u32,
        reason: ReviseReason,
        changed_fields: Vec<String>,
        dry_run: bool,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run,
            success: true,
            original_id,
            new_id: Some(new_id),
            revision_group_id: Some(revision_group_id),
            revision_number: Some(revision_number),
            reason: reason.as_str().to_owned(),
            changed_fields,
            error: None,
        }
    }

    /// Create a dry-run preview report.
    #[must_use]
    pub fn dry_run_preview(
        original_id: String,
        reason: ReviseReason,
        changed_fields: Vec<String>,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: true,
            success: true,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: reason.as_str().to_owned(),
            changed_fields,
            error: None,
        }
    }

    /// Create an unavailable write report while preserving the computed preview.
    #[must_use]
    pub fn write_unavailable(
        original_id: String,
        reason: ReviseReason,
        changed_fields: Vec<String>,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: reason.as_str().to_owned(),
            changed_fields,
            error: Some(
                "Memory revision writes are unavailable until immutable revision storage and supersession links are implemented; rerun with --dry-run to preview changes."
                    .to_owned(),
            ),
        }
    }

    /// Create a not-found error report.
    #[must_use]
    pub fn not_found(original_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some("Memory not found".to_owned()),
        }
    }

    /// Create a tombstoned error report.
    #[must_use]
    pub fn tombstoned(original_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some("Cannot revise tombstoned memory".to_owned()),
        }
    }

    /// Create a superseded-revision error report.
    #[must_use]
    pub fn superseded(original_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some(
                "Cannot revise superseded memory; revise the current revision instead".to_owned(),
            ),
        }
    }

    /// Create a no-changes error report.
    #[must_use]
    pub fn no_changes(original_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some("No changes specified".to_owned()),
        }
    }

    /// Create a database error report.
    #[must_use]
    pub fn error(original_id: String, message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some(message),
        }
    }
}

/// Revise an existing memory by creating a new immutable version.
///
/// This function:
/// 1. Validates the original memory exists and is not tombstoned
/// 2. Determines which fields are being changed
/// 3. Creates a new memory with updated fields
/// 4. Links the new memory to the original via supersession
/// 5. Marks the original as superseded
///
/// If `dry_run` is true, no changes are made but the report shows what would happen.
pub fn revise_memory(options: &ReviseMemoryOptions<'_>) -> MemoryReviseReport {
    let conn = match open_migrated_memory_database(options.database_path) {
        Ok(c) => c,
        Err(message) => {
            return MemoryReviseReport::error(options.original_memory_id.to_owned(), message);
        }
    };

    // Get the original memory
    let original = match conn.get_memory(options.original_memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return MemoryReviseReport::not_found(options.original_memory_id.to_owned()),
        Err(e) => {
            return MemoryReviseReport::error(
                options.original_memory_id.to_owned(),
                format!("Failed to query memory: {e}"),
            );
        }
    };

    // Check if tombstoned
    if original.tombstoned_at.is_some() {
        return MemoryReviseReport::tombstoned(options.original_memory_id.to_owned());
    }

    if original.valid_to.is_some() {
        return MemoryReviseReport::superseded(options.original_memory_id.to_owned());
    }

    // Determine what fields are changing
    let mut changed_fields = Vec::new();

    if let Some(content) = options.content {
        if content != original.content {
            changed_fields.push("content".to_owned());
        }
    }
    if let Some(level) = options.level {
        if level != original.level {
            changed_fields.push("level".to_owned());
        }
    }
    if let Some(kind) = options.kind {
        if kind != original.kind {
            changed_fields.push("kind".to_owned());
        }
    }
    if let Some(confidence) = options.confidence {
        if (confidence - original.confidence).abs() > f32::EPSILON {
            changed_fields.push("confidence".to_owned());
        }
    }
    if options.tags.is_some() {
        changed_fields.push("tags".to_owned());
    }
    if let Some(provenance) = options.provenance_uri {
        let current = original.provenance_uri.as_deref().unwrap_or("");
        if provenance != current {
            changed_fields.push("provenance_uri".to_owned());
        }
    }

    // If no changes, return early
    if changed_fields.is_empty() {
        return MemoryReviseReport::no_changes(options.original_memory_id.to_owned());
    }

    // If dry run, return preview
    if options.dry_run {
        return MemoryReviseReport::dry_run_preview(
            options.original_memory_id.to_owned(),
            options.reason.clone(),
            changed_fields,
        );
    }

    // N15.2 (bd-17c65.14.15.3): turn on the immutable-revision write path.
    //
    // The transaction does three things atomically:
    //   1. Inserts a new memory row with a fresh `id` but the same
    //      `logical_id` as the original (the revision chain identifier
    //      that V043 added). The new row carries `valid_from = now()`
    //      and `valid_to = NULL` — it becomes the live row.
    //   2. Sets the original row's `valid_to = now()`, marking it
    //      superseded but not tombstoned.
    //   3. Records a `memory.revise` audit entry with `from_id`,
    //      `to_id`, `logical_id`, `revision_number`, `changed_fields`,
    //      and the caller's reason.
    let logical_id = match conn.get_memory_logical_id(options.original_memory_id) {
        Ok(Some(id)) => id,
        Ok(None) => {
            // Pre-V043 rows or a race that deleted the row between
            // `get_memory` and now. Fall back to the original id —
            // post-V043 backfill guarantees logical_id == id for
            // singletons anyway.
            options.original_memory_id.to_owned()
        }
        Err(error) => {
            return MemoryReviseReport::error(
                options.original_memory_id.to_owned(),
                format!("Failed to read revision chain identifier: {error}"),
            );
        }
    };
    let prior_chain_count = match conn.count_memory_chain(&logical_id) {
        Ok(n) => n,
        Err(error) => {
            return MemoryReviseReport::error(
                options.original_memory_id.to_owned(),
                format!("Failed to count revision chain: {error}"),
            );
        }
    };
    let revision_number = prior_chain_count + 1;
    let inherited_tags: Vec<String> = match conn.get_memory_tags(options.original_memory_id) {
        Ok(tags) => tags,
        Err(error) => {
            return MemoryReviseReport::error(
                options.original_memory_id.to_owned(),
                format!("Failed to read existing tags: {error}"),
            );
        }
    };
    let new_tags = options.tags.clone().unwrap_or(inherited_tags);
    let new_content = options.content.unwrap_or(&original.content).to_owned();
    let new_level = options.level.unwrap_or(&original.level).to_owned();
    let new_kind = options.kind.unwrap_or(&original.kind).to_owned();
    let new_confidence = options.confidence.unwrap_or(original.confidence);
    let new_provenance_uri = options
        .provenance_uri
        .map(str::to_owned)
        .or_else(|| original.provenance_uri.clone());

    let new_id = MemoryId::now().to_string();
    let audit_id = generate_audit_id();
    let revised_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let memory_input = CreateMemoryInput {
        workspace_id: original.workspace_id.clone(),
        level: new_level,
        kind: new_kind,
        content: new_content,
        workflow_id: original.workflow_id.clone(),
        confidence: new_confidence,
        utility: original.utility,
        importance: original.importance,
        provenance_uri: new_provenance_uri,
        trust_class: original.trust_class.clone(),
        trust_subclass: original.trust_subclass.clone(),
        tags: new_tags,
        valid_from: Some(revised_at.clone()),
        valid_to: None,
    };
    let audit_details = serde_json::json!({
        "from_id": options.original_memory_id,
        "to_id": new_id,
        "logical_id": logical_id,
        "revision_number": revision_number,
        "changed_fields": changed_fields,
        "reason": options.reason.as_str(),
        "actor": options.actor.unwrap_or("ee memory revise"),
        "revised_at": revised_at,
    });

    let result: Result<(), String> = conn
        .with_transaction(|| {
            conn.insert_memory_revision(&new_id, &logical_id, &memory_input)?;
            let prior_updated =
                conn.expire_memory_valid_to(options.original_memory_id, &revised_at)?;
            if !prior_updated {
                // The original row no longer has a NULL valid_to. This
                // shouldn't happen given the earlier validation, but we
                // bail out so the transaction rolls back rather than
                // landing an orphan revision.
                return Err(crate::db::DbError::MalformedRow {
                    operation: crate::db::DbOperation::Execute,
                    message: "Original memory's valid_to could not be set; revision aborted."
                        .to_owned(),
                });
            }
            conn.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(original.workspace_id.clone()),
                    actor: Some(options.actor.unwrap_or("ee memory revise").to_owned()),
                    action: crate::db::audit_actions::MEMORY_REVISE.to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(new_id.clone()),
                    details: Some(audit_details.to_string()),
                },
            )?;
            Ok(())
        })
        .map_err(|error| format!("Failed to commit revision: {error}"));

    if let Err(message) = result {
        return MemoryReviseReport::error(options.original_memory_id.to_owned(), message);
    }

    MemoryReviseReport::success(
        options.original_memory_id.to_owned(),
        new_id,
        logical_id,
        revision_number,
        options.reason.clone(),
        changed_fields,
        false,
    )
}

// =============================================================================
// Dedupe Detection (EE-069)
//
// Detects potential duplicate memories before creation to warn users about
// existing similar content. Uses both exact matching and similarity scoring.
// =============================================================================

/// Severity of a dedupe warning.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum DedupeSeverity {
    /// Exact content match - very likely a duplicate.
    Exact,
    /// High similarity (>90%) - probably a duplicate.
    High,
    /// Medium similarity (70-90%) - worth reviewing.
    Medium,
    /// Low similarity (50-70%) - possibly related.
    Low,
}

impl DedupeSeverity {
    /// Stable wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Determine severity from similarity score.
    #[must_use]
    pub fn from_score(score: f32) -> Self {
        if score >= 1.0 - f32::EPSILON {
            Self::Exact
        } else if score >= 0.9 {
            Self::High
        } else if score >= 0.7 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

/// A warning about a potential duplicate memory.
#[derive(Clone, Debug)]
pub struct DedupeWarning {
    /// ID of the similar existing memory.
    pub existing_memory_id: String,
    /// Similarity score (0.0-1.0).
    pub similarity_score: f32,
    /// Severity of the warning.
    pub severity: DedupeSeverity,
    /// Content preview of the existing memory.
    pub existing_preview: String,
    /// How the match was detected.
    pub match_type: DedupeMatchType,
    /// Suggested action.
    pub suggestion: String,
}

/// How a duplicate match was detected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DedupeMatchType {
    /// Exact content match.
    ExactContent,
    /// Normalized content match (ignoring whitespace/case).
    NormalizedContent,
    /// Semantic similarity (if available).
    Semantic,
    /// Lexical similarity (word overlap).
    Lexical,
}

impl DedupeMatchType {
    /// Stable wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactContent => "exact_content",
            Self::NormalizedContent => "normalized_content",
            Self::Semantic => "semantic",
            Self::Lexical => "lexical",
        }
    }
}

/// Options for dedupe detection.
#[derive(Clone, Debug)]
pub struct DedupeCheckOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Workspace path used to derive the workspace id.
    pub workspace_path: &'a Path,
    /// Content to check for duplicates.
    pub content: &'a str,
    /// Memory level (optional filter).
    pub level: Option<&'a str>,
    /// Memory kind (optional filter).
    pub kind: Option<&'a str>,
    /// Minimum similarity threshold (0.0-1.0).
    pub min_similarity: f32,
    /// Maximum warnings to return.
    pub max_warnings: usize,
}

impl<'a> DedupeCheckOptions<'a> {
    /// Create with defaults.
    #[must_use]
    pub fn new(database_path: &'a Path, workspace_path: &'a Path, content: &'a str) -> Self {
        Self {
            database_path,
            workspace_path,
            content,
            level: None,
            kind: None,
            min_similarity: 0.5,
            max_warnings: 5,
        }
    }
}

/// Result of a dedupe check.
#[derive(Clone, Debug)]
pub struct DedupeCheckReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Whether any duplicates were found.
    pub has_warnings: bool,
    /// Warnings ordered by severity (exact first, then by similarity).
    pub warnings: Vec<DedupeWarning>,
    /// Number of memories scanned.
    pub memories_scanned: u32,
    /// Error message if check failed.
    pub error: Option<String>,
}

impl DedupeCheckReport {
    /// Create a report with warnings.
    #[must_use]
    pub fn with_warnings(warnings: Vec<DedupeWarning>, memories_scanned: u32) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            has_warnings: !warnings.is_empty(),
            warnings,
            memories_scanned,
            error: None,
        }
    }

    /// Create a report with no warnings.
    #[must_use]
    pub fn no_duplicates(memories_scanned: u32) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            has_warnings: false,
            warnings: Vec::new(),
            memories_scanned,
            error: None,
        }
    }

    /// Create an error report.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            has_warnings: false,
            warnings: Vec::new(),
            memories_scanned: 0,
            error: Some(message),
        }
    }
}

/// Normalize content for comparison (lowercase, collapse whitespace).
fn normalize_content(content: &str) -> String {
    content
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Calculate simple word-based Jaccard similarity between two texts.
fn jaccard_similarity(a: &str, b: &str) -> f32 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    intersection as f32 / union as f32
}

/// Check for potential duplicate memories.
///
/// Scans existing memories and returns warnings for any that are similar
/// to the provided content. Uses exact matching and lexical similarity.
pub fn check_for_duplicates(options: &DedupeCheckOptions<'_>) -> DedupeCheckReport {
    let conn = match open_migrated_memory_database(options.database_path) {
        Ok(c) => c,
        Err(message) => return DedupeCheckReport::error(message),
    };

    let workspace_path = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&workspace_path);

    // List memories with optional level filter
    let memories = match conn.list_memories(&workspace_id, options.level, false) {
        Ok(m) => m,
        Err(e) => return DedupeCheckReport::error(format!("Failed to list memories: {e}")),
    };

    let memories_scanned = memories.len() as u32;
    let normalized_input = normalize_content(options.content);
    let mut warnings: Vec<DedupeWarning> = Vec::new();

    for memory in memories {
        // Skip if kind filter doesn't match
        if let Some(kind) = options.kind {
            if memory.kind != kind {
                continue;
            }
        }

        // Check exact match
        if memory.content == options.content {
            warnings.push(DedupeWarning {
                existing_memory_id: memory.id.clone(),
                similarity_score: 1.0,
                severity: DedupeSeverity::Exact,
                existing_preview: truncate_content(&memory.content).0,
                match_type: DedupeMatchType::ExactContent,
                suggestion: format!(
                    "Exact duplicate exists. Consider using `ee memory show {}` to review.",
                    memory.id
                ),
            });
            continue;
        }

        // Check normalized match
        let normalized_memory = normalize_content(&memory.content);
        if normalized_memory == normalized_input {
            warnings.push(DedupeWarning {
                existing_memory_id: memory.id.clone(),
                similarity_score: 0.99,
                severity: DedupeSeverity::Exact,
                existing_preview: truncate_content(&memory.content).0,
                match_type: DedupeMatchType::NormalizedContent,
                suggestion: format!(
                    "Near-exact match (whitespace/case differs). Review `ee memory show {}`.",
                    memory.id
                ),
            });
            continue;
        }

        // Check lexical similarity
        let similarity = jaccard_similarity(&normalized_input, &normalized_memory);
        if similarity >= options.min_similarity {
            let severity = DedupeSeverity::from_score(similarity);
            warnings.push(DedupeWarning {
                existing_memory_id: memory.id.clone(),
                similarity_score: similarity,
                severity,
                existing_preview: truncate_content(&memory.content).0,
                match_type: DedupeMatchType::Lexical,
                suggestion: format!(
                    "{:.0}% similar. Consider revising instead: `ee memory revise {}`.",
                    similarity * 100.0,
                    memory.id
                ),
            });
        }
    }

    // Sort by severity (exact first), then by similarity score (descending)
    warnings.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| {
            b.similarity_score
                .partial_cmp(&a.similarity_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    // Limit warnings
    warnings.truncate(options.max_warnings);

    if warnings.is_empty() {
        DedupeCheckReport::no_duplicates(memories_scanned)
    } else {
        DedupeCheckReport::with_warnings(warnings, memories_scanned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn freshness_memory(content: &str, provenance_uri: Option<String>) -> StoredMemory {
        StoredMemory {
            id: "mem_0000000000000000000000fresh".to_owned(),
            workspace_id: "wsp_01234567890123456789012345".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            provenance_uri,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "ee.memory.provenance_chain.v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-09T00:00:00Z".to_owned(),
            updated_at: "2026-05-09T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn assess_memory_evidence_freshness_covers_stable_states() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(
            temp.path().join("source.md"),
            "Freshness source release evidence line\nsecond line\n",
        )
        .map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join("source-dir")).map_err(|error| error.to_string())?;

        let fresh = assess_memory_evidence_freshness(
            &freshness_memory(
                "Freshness source release evidence line",
                Some("file://source.md#L1".to_owned()),
            ),
            Some(temp.path()),
        );
        ensure(fresh.status, EvidenceFreshnessStatus::Fresh, "fresh file")?;

        let changed = assess_memory_evidence_freshness(
            &freshness_memory(
                "Freshness source release evidence line",
                Some("file://source.md#L2".to_owned()),
            ),
            Some(temp.path()),
        );
        ensure(
            changed.status,
            EvidenceFreshnessStatus::ChangedSource,
            "changed file span",
        )?;

        let missing = assess_memory_evidence_freshness(
            &freshness_memory(
                "Freshness source release evidence line",
                Some("file://missing.md".to_owned()),
            ),
            Some(temp.path()),
        );
        ensure(
            missing.status,
            EvidenceFreshnessStatus::MissingSource,
            "missing file",
        )?;

        let unreachable = assess_memory_evidence_freshness(
            &freshness_memory(
                "Freshness source release evidence line",
                Some("file://source-dir".to_owned()),
            ),
            Some(temp.path()),
        );
        ensure(
            unreachable.status,
            EvidenceFreshnessStatus::UnreachableSource,
            "unreadable directory source",
        )?;

        let unsupported = assess_memory_evidence_freshness(
            &freshness_memory(
                "Freshness source release evidence line",
                Some("cass-session://session-a#L1".to_owned()),
            ),
            Some(temp.path()),
        );
        ensure(
            unsupported.status,
            EvidenceFreshnessStatus::UnsupportedSource,
            "unsupported source",
        )?;

        let unknown =
            assess_memory_evidence_freshness(&freshness_memory("No explicit source.", None), None);
        ensure(unknown.status, EvidenceFreshnessStatus::Unknown, "unknown")
    }

    fn remember_revisable_memory(
        content: &str,
    ) -> Result<(tempfile::TempDir, RememberMemoryReport), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let created = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("release,checks"),
            confidence: 0.9,
            source: Some("file://README.md#L74-77"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        Ok((temp, created))
    }

    #[test]
    fn expire_memory_dry_run_preserves_memory() -> TestResult {
        let (temp, created) = remember_revisable_memory("Expire dry-run target.")?;
        let report = expire_memory(&ExpireMemoryOptions {
            workspace_path: temp.path(),
            database_path: &created.database_path,
            memory_id: &created.memory_id.to_string(),
            reason: Some("not needed"),
            actor: Some("test"),
            dry_run: true,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;

        ensure(report.status, "would_expire".to_owned(), "dry-run status")?;
        ensure(report.persisted, false, "dry-run persisted")?;
        ensure(
            report.previous_valid_to.is_none(),
            true,
            "dry-run previous valid_to absent",
        )?;
        ensure(report.valid_to.is_some(), true, "dry-run valid_to preview")?;
        ensure(report.audit_id.is_none(), true, "dry-run audit absent")?;

        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(&created.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after dry-run".to_owned())?;
        ensure(
            memory.tombstoned_at.is_none(),
            true,
            "memory remains active",
        )?;
        ensure(memory.valid_to.is_none(), true, "memory valid_to unchanged")
    }

    #[test]
    fn expire_memory_persists_valid_to_audit_and_index_job() -> TestResult {
        let (temp, created) = remember_revisable_memory("Expire persisted target.")?;
        let report = expire_memory(&ExpireMemoryOptions {
            workspace_path: temp.path(),
            database_path: &created.database_path,
            memory_id: &created.memory_id.to_string(),
            reason: Some("obsolete"),
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;

        ensure(report.status, "expired".to_owned(), "expire status")?;
        ensure(report.persisted, true, "expire persisted")?;
        ensure(
            report.previous_valid_to.is_none(),
            true,
            "previous valid_to absent",
        )?;
        ensure(report.valid_to.is_some(), true, "valid_to is set")?;
        ensure(
            report.tombstoned_at.is_none(),
            true,
            "expire does not tombstone",
        )?;
        ensure(report.audit_id.is_some(), true, "audit ID present")?;
        ensure(report.index_job_id.is_some(), true, "index job ID present")?;

        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(&created.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory missing after expire".to_owned())?;
        ensure(
            memory.tombstoned_at.is_none(),
            true,
            "memory remains untombstoned",
        )?;
        ensure(memory.valid_to.is_some(), true, "memory valid_to persisted")?;

        let audit_id = report
            .audit_id
            .as_deref()
            .ok_or_else(|| "missing audit id".to_owned())?;
        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing expire audit".to_owned())?;
        ensure(
            audit.action,
            audit_actions::MEMORY_EXPIRE.to_owned(),
            "expire audit action",
        )
    }

    #[test]
    fn expire_memory_is_idempotent_after_valid_to_expiry() -> TestResult {
        let (temp, created) = remember_revisable_memory("Expire idempotent target.")?;
        let report = expire_memory(&ExpireMemoryOptions {
            workspace_path: temp.path(),
            database_path: &created.database_path,
            memory_id: &created.memory_id.to_string(),
            reason: Some("obsolete"),
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(report.status, "expired".to_owned(), "initial expire status")?;

        let already = expire_memory(&ExpireMemoryOptions {
            workspace_path: temp.path(),
            database_path: &created.database_path,
            memory_id: &created.memory_id.to_string(),
            reason: Some("again"),
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: true,
        })
        .map_err(|error| error.message())?;
        ensure(
            already.status,
            "already_expired".to_owned(),
            "idempotent status",
        )?;
        ensure(already.persisted, false, "idempotent persisted")?;
        ensure(
            already.previous_valid_to,
            report.valid_to.clone(),
            "idempotent previous valid_to",
        )?;
        ensure(already.valid_to, report.valid_to, "idempotent valid_to")
    }

    #[test]
    fn memory_tags_updates_are_sorted_audited_and_idempotent() -> TestResult {
        let (temp, created) = remember_revisable_memory("Tags mutation target.")?;
        let memory_id = created.memory_id.to_string();

        let dry_run = update_memory_tags(&MemoryTagsOptions {
            workspace_path: temp.path(),
            database_path: &created.database_path,
            memory_id: &memory_id,
            mode: MemoryTagsMode::Patch {
                add: vec!["zeta".to_owned(), "alpha".to_owned()],
                remove: vec!["checks".to_owned()],
            },
            actor: Some("test"),
            dry_run: true,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(dry_run.status, "would_update".to_owned(), "dry-run status")?;
        ensure(
            dry_run.tags,
            vec!["alpha".to_owned(), "release".to_owned(), "zeta".to_owned()],
            "dry-run sorted tags",
        )?;

        let applied = update_memory_tags(&MemoryTagsOptions {
            workspace_path: temp.path(),
            database_path: &created.database_path,
            memory_id: &memory_id,
            mode: MemoryTagsMode::Patch {
                add: vec!["zeta".to_owned(), "alpha".to_owned()],
                remove: vec!["checks".to_owned()],
            },
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(applied.status, "updated".to_owned(), "apply status")?;
        ensure(applied.audit_ids.len(), 1, "audit count")?;
        ensure(applied.index_job_id.is_some(), true, "index job present")?;
        let expected_tags = vec!["alpha".to_owned(), "release".to_owned(), "zeta".to_owned()];
        ensure(
            applied.tags.clone(),
            expected_tags.clone(),
            "applied sorted tags",
        )?;

        let unchanged = update_memory_tags(&MemoryTagsOptions {
            workspace_path: temp.path(),
            database_path: &created.database_path,
            memory_id: &memory_id,
            mode: MemoryTagsMode::Set(expected_tags),
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(
            unchanged.status,
            "unchanged".to_owned(),
            "idempotent status",
        )?;
        ensure(
            unchanged.audit_ids.is_empty(),
            true,
            "idempotent audit absent",
        )
    }

    #[test]
    fn memory_link_create_lists_and_reports_duplicate_idempotently() -> TestResult {
        let (temp, source) = remember_revisable_memory("Memory link source.")?;
        let target = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: Some(&source.database_path),
            content: "Memory link target.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: Some("links"),
            confidence: 0.8,
            source: Some("file://README.md#L78-80"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;
        let source_id = source.memory_id.to_string();
        let target_id = target.memory_id.to_string();

        let dry_run = update_memory_link(&MemoryLinkOptions {
            workspace_path: temp.path(),
            database_path: &source.database_path,
            memory_id: &source_id,
            mode: MemoryLinkMode::Create {
                target_memory_id: target_id.clone(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.75,
                confidence: 0.9,
                directed: true,
                evidence_count: 2,
                source: MemoryLinkSource::Human,
                metadata_json: Some(r#"{"reason":"explicit test"}"#.to_owned()),
            },
            actor: Some("test"),
            dry_run: true,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(dry_run.status, "would_create".to_owned(), "dry-run status")?;
        ensure(dry_run.link.is_some(), true, "dry-run link present")?;
        ensure(
            dry_run.link.and_then(|link| link.link_id),
            None,
            "dry-run has no link id",
        )?;

        let applied = update_memory_link(&MemoryLinkOptions {
            workspace_path: temp.path(),
            database_path: &source.database_path,
            memory_id: &source_id,
            mode: MemoryLinkMode::Create {
                target_memory_id: target_id.clone(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.75,
                confidence: 0.9,
                directed: true,
                evidence_count: 2,
                source: MemoryLinkSource::Human,
                metadata_json: Some(r#"{"reason":"explicit test"}"#.to_owned()),
            },
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(applied.status, "created".to_owned(), "apply status")?;
        ensure(applied.persisted, true, "link persisted")?;
        ensure(applied.audit_id.is_some(), true, "audit ID present")?;
        let applied_link_id = applied
            .link
            .as_ref()
            .and_then(|link| link.link_id.clone())
            .ok_or_else(|| "created link id missing".to_owned())?;

        let listed = update_memory_link(&MemoryLinkOptions {
            workspace_path: temp.path(),
            database_path: &source.database_path,
            memory_id: &source_id,
            mode: MemoryLinkMode::List {
                relation: Some(MemoryLinkRelation::Supports),
            },
            actor: None,
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(listed.status, "listed".to_owned(), "list status")?;
        ensure(listed.links.len(), 1, "listed link count")?;
        ensure(
            listed.links[0].link_id.clone(),
            Some(applied_link_id.clone()),
            "listed link id",
        )?;

        let duplicate = update_memory_link(&MemoryLinkOptions {
            workspace_path: temp.path(),
            database_path: &source.database_path,
            memory_id: &source_id,
            mode: MemoryLinkMode::Create {
                target_memory_id: target_id,
                relation: MemoryLinkRelation::Supports,
                weight: 0.75,
                confidence: 0.9,
                directed: true,
                evidence_count: 2,
                source: MemoryLinkSource::Human,
                metadata_json: Some(r#"{"reason":"explicit test"}"#.to_owned()),
            },
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        ensure(
            duplicate.status,
            "already_exists".to_owned(),
            "duplicate status",
        )?;
        ensure(duplicate.persisted, false, "duplicate persisted")?;
        ensure(duplicate.audit_id.is_none(), true, "duplicate audit absent")?;
        ensure(
            duplicate.link.and_then(|link| link.link_id),
            Some(applied_link_id),
            "duplicate reports existing link",
        )
    }

    #[test]
    fn truncate_content_handles_multibyte_boundary() -> TestResult {
        let content = "é".repeat(CONTENT_PREVIEW_LEN + 1);
        let expected = format!("{}...", "é".repeat(CONTENT_PREVIEW_LEN));

        ensure(
            truncate_content(&content),
            (expected, true),
            "multibyte preview truncates and reports content_truncated=true",
        )
    }

    #[test]
    fn truncate_content_below_limit_is_untruncated() -> TestResult {
        let content = "short body";
        ensure(
            truncate_content(content),
            (content.to_string(), false),
            "below-limit content is not truncated",
        )
    }

    #[test]
    fn truncate_content_at_exact_limit_is_untruncated() -> TestResult {
        let content = "a".repeat(CONTENT_PREVIEW_LEN);
        ensure(
            truncate_content(&content),
            (content.clone(), false),
            "at-limit content is not truncated",
        )
    }

    #[test]
    fn truncate_content_empty_is_untruncated() -> TestResult {
        ensure(
            truncate_content(""),
            (String::new(), false),
            "empty content is not truncated",
        )
    }

    #[test]
    fn memory_show_report_not_found_is_correct() -> TestResult {
        let report = MemoryShowReport::not_found();

        ensure(report.found, false, "found")?;
        ensure(report.memory.is_none(), true, "memory is none")?;
        ensure(report.is_tombstoned, false, "is_tombstoned")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn memory_show_report_error_captures_message() -> TestResult {
        let report = MemoryShowReport::error("test error".to_string());

        ensure(report.found, false, "found")?;
        ensure(
            report.error,
            Some("test error".to_string()),
            "error message",
        )
    }

    #[test]
    fn memory_show_report_version_matches_package() -> TestResult {
        let report = MemoryShowReport::not_found();
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    #[test]
    fn remember_memory_dry_run_does_not_create_database() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "  Run cargo fmt before release.  ",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("Release,cli,release"),
            confidence: 0.8,
            source: Some("file://AGENTS.md#L42"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: true,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.persisted, false, "persisted")?;
        ensure(report.revision_number, 1, "revision number")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "revision group absent",
        )?;
        ensure(report.audit_id.is_none(), true, "audit id absent")?;
        ensure(report.index_job_id.is_none(), true, "index job absent")?;
        ensure(
            report.index_status,
            "dry_run_not_queued".to_string(),
            "index status",
        )?;
        ensure(report.effect_ids.is_empty(), true, "effect ids empty")?;
        ensure(
            report.suggested_links.is_empty(),
            true,
            "suggested links empty",
        )?;
        ensure(
            report.suggested_link_status,
            "dry_run_not_evaluated".to_string(),
            "suggested link status",
        )?;
        ensure(
            report.suggested_link_degradations.is_empty(),
            true,
            "suggested link degradations",
        )?;
        ensure(
            report.redaction_status,
            "checked".to_string(),
            "redaction status",
        )?;
        ensure(
            report.database_path.exists(),
            false,
            "dry run must not create database",
        )?;
        ensure(
            report.tags,
            vec!["cli".to_string(), "release".to_string()],
            "canonical tags",
        )?;
        ensure(
            report.source,
            Some("file://AGENTS.md#L42".to_string()),
            "canonical source",
        )?;
        ensure(report.valid_from, None, "valid_from absent")?;
        ensure(report.valid_to, None, "valid_to absent")?;
        ensure(
            report.validity_status,
            "unknown".to_string(),
            "validity status",
        )?;
        ensure(
            report.validity_window_kind,
            "unbounded".to_string(),
            "validity window kind",
        )
    }

    #[test]
    fn remember_memory_persists_memory_audit_and_publishes_index_job() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Store release checks as durable memory.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("release,checks"),
            confidence: 0.9,
            source: Some("file://README.md#L74-77"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.persisted, true, "persisted")?;
        ensure(report.revision_number, 1, "revision number")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "revision group absent",
        )?;
        ensure(report.audit_id.is_some(), true, "audit id present")?;
        ensure(report.index_job_id.is_some(), true, "index job id present")?;
        ensure(report.index_status, "indexed".to_string(), "index status")?;
        ensure(report.effect_ids.is_empty(), true, "effect ids empty")?;
        ensure(
            report.suggested_links.is_empty(),
            true,
            "suggested links empty",
        )?;
        ensure(
            report.suggested_link_status,
            "no_candidates".to_string(),
            "suggested link status",
        )?;
        ensure(
            report.suggested_link_degradations.is_empty(),
            true,
            "suggested link degradations",
        )?;
        ensure(
            report.redaction_status,
            "checked".to_string(),
            "redaction status",
        )?;
        ensure(report.database_path.exists(), true, "database created")?;

        let connection = crate::db::DbConnection::open_file(&report.database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory should be persisted".to_string())?;
        ensure(
            memory.workspace_id,
            report.workspace_id.clone(),
            "workspace id",
        )?;
        ensure(
            memory.content,
            "Store release checks as durable memory.".to_string(),
            "content",
        )?;
        ensure(
            memory.trust_class,
            "human_explicit".to_string(),
            "trust class",
        )?;
        ensure(
            memory.provenance_uri,
            Some("file://README.md#L74-77".to_string()),
            "provenance uri",
        )?;
        ensure(
            memory.valid_from.is_some(),
            true,
            "stored valid_from assigned",
        )?;
        ensure(memory.valid_to, None, "stored valid_to")?;
        let tags = connection
            .get_memory_tags(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?;
        ensure(
            tags,
            vec!["checks".to_string(), "release".to_string()],
            "tags",
        )?;
        let audit_id = report
            .audit_id
            .as_ref()
            .ok_or_else(|| "audit id missing".to_string())?;
        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "audit should be persisted".to_string())?;
        ensure(audit.action, "memory.create".to_string(), "audit action")?;
        ensure(
            audit.target_id,
            Some(report.memory_id.to_string()),
            "audit target",
        )?;
        let job_id = report
            .index_job_id
            .as_ref()
            .ok_or_else(|| "index job id missing".to_string())?;
        let job = connection
            .get_search_index_job(job_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "index job should be persisted".to_string())?;
        ensure(job.status, "completed".to_string(), "index job status")?;
        ensure(
            job.document_id.clone(),
            Some(report.memory_id.to_string()),
            "index job document",
        )?;
        ensure(
            temp.path()
                .join(".ee")
                .join("index")
                .join("meta.json")
                .is_file(),
            true,
            "index metadata published",
        )
    }

    #[test]
    fn remember_memory_validates_and_stores_temporal_validity_window() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Temporal memories retain their explicit applicability window.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: Some("temporal,validity"),
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: Some("2020-01-01T00:00:00+00:00"),
            valid_to: Some("2099-01-01T00:00:00Z"),
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(
            report.valid_from,
            Some("2020-01-01T00:00:00Z".to_string()),
            "normalized valid_from",
        )?;
        ensure(
            report.valid_to,
            Some("2099-01-01T00:00:00Z".to_string()),
            "normalized valid_to",
        )?;
        ensure(
            report.validity_status,
            "current".to_string(),
            "validity status",
        )?;
        ensure(
            report.validity_window_kind,
            "bounded".to_string(),
            "validity window kind",
        )?;

        let connection = crate::db::DbConnection::open_file(&report.database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory should be persisted".to_string())?;
        ensure(
            memory.valid_from,
            Some("2020-01-01T00:00:00Z".to_string()),
            "stored valid_from",
        )?;
        ensure(
            memory.valid_to,
            Some("2099-01-01T00:00:00Z".to_string()),
            "stored valid_to",
        )
    }

    #[test]
    fn remember_memory_rejects_invalid_temporal_validity_windows() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;

        let malformed = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Temporal windows must parse.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: Some("not a timestamp"),
            valid_to: None,
            dry_run: true,
            auto_link: true,
            propose_candidates: true,
        });
        match malformed {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("valid_from"), true, "mentions valid_from")?;
            }
            Err(error) => return Err(format!("expected usage error, got {error:?}")),
            Ok(_) => return Err("malformed valid_from should fail".to_string()),
        }

        let reversed = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Temporal windows must be ordered.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: Some("2099-01-01T00:00:00Z"),
            valid_to: Some("2020-01-01T00:00:00Z"),
            dry_run: true,
            auto_link: true,
            propose_candidates: true,
        });
        match reversed {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("valid_from"), true, "mentions valid_from")?;
                ensure(message.contains("valid_to"), true, "mentions valid_to")?;
            }
            Err(error) => return Err(format!("expected usage error, got {error:?}")),
            Ok(_) => return Err("reversed validity window should fail".to_string()),
        }

        let boundary = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Instant validity windows are accepted at the boundary.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: Some("2050-01-01T00:00:00Z"),
            valid_to: Some("2050-01-01T00:00:00Z"),
            dry_run: true,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;
        ensure(
            boundary.validity_window_kind,
            "instant".to_string(),
            "boundary-equal window kind",
        )
    }

    #[test]
    fn remember_memory_returns_tag_cooccurrence_suggestions_without_links() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let first = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Release checks include cargo fmt.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("release,checks"),
            confidence: 0.9,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;
        let second = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Release docs mention supported targets.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: Some("release,docs"),
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;
        let third = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Before release, run checks and record evidence.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("checks,release"),
            confidence: 0.85,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(
            third.suggested_link_status,
            "ready".to_string(),
            "suggested link status",
        )?;
        ensure(
            third.suggested_link_degradations.is_empty(),
            true,
            "suggested link degradations",
        )?;
        ensure(third.suggested_links.len(), 2, "suggestion count")?;
        ensure(
            third.suggested_links[0].target_memory_id.clone(),
            first.memory_id.to_string(),
            "highest-overlap target first",
        )?;
        ensure(
            third.suggested_links[0].matched_tags.clone(),
            vec!["checks".to_string(), "release".to_string()],
            "highest-overlap tags",
        )?;
        ensure(
            third.suggested_links[0].relation.clone(),
            "co_tag".to_string(),
            "relation",
        )?;
        ensure(
            third.suggested_links[0].source.clone(),
            "tag_cooccurrence".to_string(),
            "source",
        )?;
        ensure(
            third.suggested_links[1].target_memory_id.clone(),
            second.memory_id.to_string(),
            "lower-overlap target second",
        )?;

        let connection = crate::db::DbConnection::open_file(&third.database_path)
            .map_err(|error| error.to_string())?;
        let links = connection
            .list_all_memory_links(None)
            .map_err(|error| error.to_string())?;
        ensure(
            links.is_empty(),
            true,
            "suggestions must not create durable memory links",
        )
    }

    #[test]
    fn remember_memory_auto_links_recent_workflow_memories() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
        let workflow_id = "wf-auto-link";

        let first = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "First working memory in the release workflow.",
            workflow_id: Some(workflow_id),
            level: "working",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;
        ensure(
            first.auto_link_status,
            "no_candidates".to_string(),
            "first auto-link status",
        )?;

        let second = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Second working memory should reinforce the same workflow.",
            workflow_id: Some(workflow_id),
            level: "working",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(
            second.auto_link_status,
            "linked".to_string(),
            "second auto-link status",
        )?;
        ensure(second.auto_links.len(), 1, "report auto-link count")?;
        let reported = second
            .auto_links
            .first()
            .ok_or_else(|| "report auto-link missing".to_string())?;
        ensure(
            reported.target_memory_id.clone(),
            first.memory_id.to_string(),
            "reported target",
        )?;
        ensure(reported.relation.clone(), "related".to_string(), "relation")?;
        ensure(reported.source.clone(), "auto".to_string(), "source")?;
        ensure(reported.weight, 0.5, "weight")?;

        let connection = crate::db::DbConnection::open_file(&second.database_path)
            .map_err(|error| error.to_string())?;
        let links = connection
            .list_all_memory_links(None)
            .map_err(|error| error.to_string())?;
        ensure(links.len(), 1, "memory_links row count")?;
        let link = links
            .first()
            .ok_or_else(|| "stored auto-link missing".to_string())?;
        ensure(link.id.clone(), reported.link_id.clone(), "stored link id")?;
        ensure(
            link.src_memory_id.clone(),
            second.memory_id.to_string(),
            "stored source memory",
        )?;
        ensure(
            link.dst_memory_id.clone(),
            first.memory_id.to_string(),
            "stored target memory",
        )?;
        ensure(
            link.relation.clone(),
            "related".to_string(),
            "stored relation",
        )?;
        ensure(link.source.clone(), "auto".to_string(), "stored source")?;
        ensure(link.weight, 0.5, "stored weight")?;
        let metadata: serde_json::Value = serde_json::from_str(
            link.metadata_json
                .as_deref()
                .ok_or_else(|| "link metadata missing".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            metadata["linkKind"].clone(),
            serde_json::json!("hebbian"),
            "link kind metadata",
        )?;
        ensure(
            metadata["workflowId"].clone(),
            serde_json::json!(workflow_id),
            "workflow metadata",
        )?;
        let audit = connection
            .get_audit(&reported.audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "auto-link audit missing".to_string())?;
        ensure(
            audit.action,
            "memory.link.create".to_string(),
            "audit action",
        )?;
        ensure(
            audit.target_id,
            Some(reported.link_id.clone()),
            "audit target",
        )
    }

    /// G7 (bd-17c65.7.6): when ee remember runs without a workflow_id,
    /// the auto-link path commits to honest-unimplemented: status is
    /// `"no_workflow_required"` (NOT `"no_workflow"`; the new name
    /// signals this is a non-failure state) AND an info-severity
    /// `auto_link_disabled` degraded entry surfaces with a pointer to
    /// the explicit `ee memory link` recovery path.
    #[test]
    fn remember_memory_without_workflow_emits_auto_link_disabled_degradation() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "A workflow-less memory; no auto-linking possible.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(
            report.auto_link_status.clone(),
            "no_workflow_required".to_string(),
            "workflow-less auto-link status is `no_workflow_required` (honest-unimplemented marker)",
        )?;
        ensure(
            report.auto_links.len(),
            0,
            "no auto-links created without workflow",
        )?;
        ensure(
            report.auto_link_degradations.len(),
            1,
            "exactly one auto_link_disabled degraded entry",
        )?;
        let degradation = report
            .auto_link_degradations
            .first()
            .ok_or_else(|| "auto_link_disabled entry missing".to_string())?;
        ensure(
            degradation.code.clone(),
            "auto_link_disabled".to_string(),
            "degraded entry code",
        )?;
        ensure(
            degradation.severity.clone(),
            "info".to_string(),
            "degraded entry severity",
        )?;
        ensure(
            degradation.message.contains("workflow context"),
            true,
            "message mentions workflow context",
        )?;
        ensure(
            degradation.message.contains("ee memory link"),
            true,
            "message points at `ee memory link`",
        )?;
        ensure(
            degradation.repair.contains("ee memory link"),
            true,
            "repair points at `ee memory link --help`",
        )
    }

    #[test]
    fn remember_memory_auto_link_can_be_disabled() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
        let workflow_id = "wf-no-auto-link";

        remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Existing working memory in a workflow.",
            workflow_id: Some(workflow_id),
            level: "working",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        let second = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "This memory opts out of workflow auto-linking.",
            workflow_id: Some(workflow_id),
            level: "working",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: false,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(
            second.auto_link_status,
            "disabled".to_string(),
            "auto-link disabled status",
        )?;
        ensure(
            second.auto_links.is_empty(),
            true,
            "report has no auto-links",
        )?;
        let connection = crate::db::DbConnection::open_file(&second.database_path)
            .map_err(|error| error.to_string())?;
        let links = connection
            .list_all_memory_links(None)
            .map_err(|error| error.to_string())?;
        ensure(links.is_empty(), true, "no durable links when disabled")
    }

    #[test]
    fn remember_memory_proposes_curation_candidate_after_repeated_tagged_rules() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let mut reports = Vec::new();
        for index in 0..3 {
            reports.push(
                remember_memory(&RememberMemoryOptions {
                    workspace_path: temp.path(),
                    database_path: None,
                    content: &format!(
                        "Cargo release rule {index}: run cargo fmt --check before release."
                    ),
                    workflow_id: None,
                    level: "procedural",
                    kind: "rule",
                    tags: Some("cargo,release"),
                    confidence: 0.8,
                    source: None,
                    allow_secret_mention: false,
                    valid_from: None,
                    valid_to: None,
                    dry_run: false,
                    auto_link: true,
                    propose_candidates: true,
                })
                .map_err(|error| error.message())?,
            );
        }

        let third = reports
            .last()
            .ok_or_else(|| "third remember report missing".to_owned())?;
        ensure(
            third.curation_candidate_status.clone(),
            "proposed".to_owned(),
            "third proposal status",
        )?;
        let proposal = third
            .curation_candidate
            .as_ref()
            .ok_or_else(|| "proposal missing".to_owned())?;
        ensure(proposal.member_memory_ids.len(), 3, "proposal member count")?;
        for report in &reports {
            ensure(
                proposal
                    .member_memory_ids
                    .contains(&report.memory_id.to_string()),
                true,
                "proposal includes seeded memory",
            )?;
        }
        ensure(
            proposal.audit_id.is_some(),
            true,
            "proposal audit id recorded",
        )?;

        let connection = crate::db::DbConnection::open_file(&third.database_path)
            .map_err(|error| error.to_string())?;
        let candidates = connection
            .list_curation_candidates(&third.workspace_id, Some("rule"), Some("pending"), None)
            .map_err(|error| error.to_string())?;
        ensure(candidates.len(), 1, "stored candidate count")?;
        let stored = candidates
            .first()
            .ok_or_else(|| "stored candidate missing".to_owned())?;
        ensure(
            stored.id.clone(),
            proposal.candidate_id.clone(),
            "stored candidate id",
        )?;
        ensure(
            stored.source_type.clone(),
            "agent_inference".to_owned(),
            "stored candidate source",
        )?;
        let source_id = stored
            .source_id
            .clone()
            .ok_or_else(|| "stored source ids missing".to_owned())?;
        for report in &reports {
            ensure(
                source_id.contains(&report.memory_id.to_string()),
                true,
                "stored source ids include seeded memory",
            )?;
        }
        let audit_id = proposal
            .audit_id
            .as_ref()
            .ok_or_else(|| "proposal audit missing".to_owned())?;
        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate audit row missing".to_owned())?;
        ensure(
            audit.action,
            "curation_candidate.create".to_owned(),
            "candidate audit action",
        )?;
        let audit_details = audit
            .details
            .as_ref()
            .ok_or_else(|| "candidate audit details missing".to_owned())?;
        let audit_details: serde_json::Value =
            serde_json::from_str(audit_details).map_err(|error| error.to_string())?;
        ensure(
            audit_details["cluster"]["algorithm"].as_str(),
            Some("average_linkage_agglomerative"),
            "cluster algorithm recorded",
        )?;
        ensure(
            audit_details["cluster"]["memberCount"].as_u64(),
            Some(3),
            "cluster member count recorded",
        )?;
        ensure(
            audit_details["cluster"]["silhouette"]
                .as_f64()
                .is_some_and(|score| score >= 0.4),
            true,
            "accepted cluster silhouette recorded",
        )?;
        ensure(
            audit_details["cluster"]["threshold"]
                .as_f64()
                .is_some_and(|threshold| (0.0..=1.0).contains(&threshold)),
            true,
            "cluster threshold recorded",
        )?;
        ensure(
            audit_details["cluster"]["embeddingSnapshotHash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("blake3:")),
            true,
            "embedding snapshot hash recorded",
        )
    }

    #[test]
    fn remember_memory_curation_candidate_proposal_can_be_disabled() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Remember candidate proposal opt-out.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("cargo,release"),
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            report.curation_candidate_status,
            "disabled".to_owned(),
            "proposal disabled status",
        )?;
        ensure(
            report.curation_candidate.is_none(),
            true,
            "proposal absent when disabled",
        )
    }

    #[test]
    fn remember_memory_skips_curation_candidate_when_existing_rule_covers_cluster() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let mut reports = Vec::new();
        for index in 0..2 {
            reports.push(
                remember_memory(&RememberMemoryOptions {
                    workspace_path: temp.path(),
                    database_path: None,
                    content: &format!(
                        "Cargo release rule {index}: run cargo fmt --check before release."
                    ),
                    workflow_id: None,
                    level: "procedural",
                    kind: "rule",
                    tags: Some("cargo,release"),
                    confidence: 0.8,
                    source: None,
                    allow_secret_mention: false,
                    valid_from: None,
                    valid_to: None,
                    dry_run: false,
                    auto_link: true,
                    propose_candidates: true,
                })
                .map_err(|error| error.message())?,
            );
        }

        let database_path = reports
            .first()
            .ok_or_else(|| "seed report missing".to_owned())?
            .database_path
            .clone();
        let workspace_id = reports
            .first()
            .ok_or_else(|| "seed report missing".to_owned())?
            .workspace_id
            .clone();
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection
            .insert_procedural_rule(
                "rule_00000000000000000000000000",
                &crate::db::CreateProceduralRuleInput {
                    workspace_id: workspace_id.clone(),
                    content: "Run cargo fmt --check before release work.".to_owned(),
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    scope: "workspace".to_owned(),
                    scope_pattern: None,
                    maturity: "candidate".to_owned(),
                    protected: false,
                    source_memory_ids: reports
                        .iter()
                        .map(|report| report.memory_id.to_string())
                        .collect(),
                    tags: vec!["cargo".to_owned(), "release".to_owned()],
                },
            )
            .map_err(|error| error.to_string())?;

        let third = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Cargo release rule 2: run cargo fmt --check before release.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("cargo,release"),
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(
            third.curation_candidate_status,
            "skipped_existing_rule_covers".to_owned(),
            "covering rule skip status",
        )?;
        ensure(
            third.curation_candidate.is_none(),
            true,
            "no proposal when rule covers cluster",
        )?;
        ensure(
            third
                .curation_candidate_degradations
                .iter()
                .any(|degradation| degradation.code == "auto_propose_skipped_existing_rule_covers"),
            true,
            "covering rule degradation emitted",
        )?;
        let candidates = connection
            .list_curation_candidates(&workspace_id, Some("rule"), Some("pending"), None)
            .map_err(|error| error.to_string())?;
        ensure(candidates.is_empty(), true, "no stored candidate")
    }

    #[test]
    fn staged_link_builder_suppresses_self_existing_and_limits_stably() -> TestResult {
        let mut matches = BTreeMap::new();
        matches.insert(
            "mem_new".to_string(),
            BTreeSet::from(["release".to_string(), "checks".to_string()]),
        );
        matches.insert(
            "mem_existing".to_string(),
            BTreeSet::from(["release".to_string(), "checks".to_string()]),
        );
        matches.insert(
            "mem_c".to_string(),
            BTreeSet::from(["release".to_string(), "checks".to_string()]),
        );
        matches.insert("mem_a".to_string(), BTreeSet::from(["release".to_string()]));
        matches.insert("mem_b".to_string(), BTreeSet::from(["release".to_string()]));

        let existing_targets = BTreeSet::from(["mem_existing".to_string()]);
        let suggestions =
            build_suggested_links_from_matches("mem_new", matches, &existing_targets, 2, 2);

        ensure(suggestions.len(), 2, "bounded suggestions")?;
        ensure(
            suggestions[0].target_memory_id.clone(),
            "mem_c".to_string(),
            "highest overlap first",
        )?;
        ensure(
            suggestions[1].target_memory_id.clone(),
            "mem_a".to_string(),
            "tie broken by target id",
        )?;
        ensure(
            suggestions
                .iter()
                .any(|suggestion| suggestion.target_memory_id == "mem_new"),
            false,
            "self-link suppressed",
        )?;
        ensure(
            suggestions
                .iter()
                .any(|suggestion| suggestion.target_memory_id == "mem_existing"),
            false,
            "existing link suppressed",
        )
    }

    #[test]
    fn remember_memory_rejects_secret_like_content_before_storage() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let secret_like_content = "Rotate API_KEY=sk-FAKEabc123def456ghi789jkl012 before release.";
        let result = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: secret_like_content,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        });

        match result {
            Err(
                DomainError::PolicyDenied { message, repair }
                | DomainError::PolicyDeniedWithDetails {
                    message, repair, ..
                },
            ) => {
                ensure(
                    message.contains("secret"),
                    true,
                    "policy error mentions secret",
                )?;
                ensure(repair.is_some(), true, "repair is present")?;
            }
            Err(error) => return Err(format!("expected policy denial, got {error:?}")),
            Ok(report) => {
                return Err(format!(
                    "secret-like content should not persist, got {report:?}"
                ));
            }
        }
        ensure(
            temp.path().join(".ee").join("ee.db").exists(),
            false,
            "policy denial must not create database",
        )
    }

    #[test]
    fn remember_invalid_tag_error_includes_programmatic_details() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let result = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Tag rejection should be recoverable by an agent.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: Some("bad tag"),
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: true,
            auto_link: true,
            propose_candidates: true,
        });

        let details_json = match result {
            Err(DomainError::UsageWithDetails { details_json, .. }) => details_json,
            Err(error) => return Err(format!("expected detailed usage error, got {error:?}")),
            Ok(report) => return Err(format!("invalid tag should fail, got {report:?}")),
        };
        let details: serde_json::Value =
            serde_json::from_str(&details_json).map_err(|error| error.to_string())?;
        ensure(
            details["acceptedPattern"]
                .as_str()
                .unwrap_or_default()
                .contains("._:-"),
            true,
            "accepted pattern names C3 punctuation",
        )?;
        ensure(
            details["acceptedExamples"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == "v0.1.0")),
            true,
            "accepted examples include dotted version",
        )?;
        ensure(
            details["matchedAt"][0]["reason"].as_str(),
            Some("space_disallowed"),
            "space rejection reason",
        )
    }

    #[test]
    fn remember_secret_policy_error_includes_offsets_without_secret_value() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
        let secret_like_content =
            "Document redacted sample API_KEY=sk-FAKEabc123def456ghi789jkl012.";
        let result = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: secret_like_content,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        });

        let details_json = match result {
            Err(DomainError::PolicyDeniedWithDetails { details_json, .. }) => details_json,
            Err(error) => return Err(format!("expected detailed policy error, got {error:?}")),
            Ok(report) => return Err(format!("secret-like content should fail, got {report:?}")),
        };
        if details_json.contains("sk-FAKEabc123def456ghi789jkl012") {
            return Err("policy details leaked the rejected secret value".to_owned());
        }
        let details: serde_json::Value =
            serde_json::from_str(&details_json).map_err(|error| error.to_string())?;
        ensure(
            details["bypassFlag"].as_str(),
            Some("--allow-secret-mention"),
            "bypass flag",
        )?;
        ensure(
            details["matchedAt"][0]["pattern_id"].as_str(),
            Some("api_key"),
            "pattern id",
        )?;
        ensure(
            details["matchedAt"][0]["start"].as_u64().is_some(),
            true,
            "match start present",
        )
    }

    #[test]
    fn remember_memory_allow_secret_mention_persists_with_policy_bypass_audit() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
        let secret_like_content =
            "Document redacted sample API_KEY=sk-FAKEabc123def456ghi789jkl012.";

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: secret_like_content,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: true,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(report.persisted, true, "bypass persisted")?;
        let bypass = report
            .policy_bypass
            .as_ref()
            .ok_or_else(|| "policy bypass missing".to_owned())?;
        ensure(bypass.code.clone(), "policy_bypass_used".to_owned(), "code")?;
        ensure(bypass.kind.clone(), "flag".to_owned(), "kind")?;
        let policy_audit_id = bypass
            .audit_id
            .as_deref()
            .ok_or_else(|| "policy bypass audit id missing".to_owned())?;

        let connection = crate::db::DbConnection::open_file(&report.database_path)
            .map_err(|error| error.to_string())?;
        let audit = connection
            .get_audit(policy_audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "policy bypass audit row missing".to_owned())?;
        ensure(
            audit.action,
            audit_actions::POLICY_BYPASS.to_owned(),
            "policy audit action",
        )?;
        ensure(
            audit.target_id,
            Some(report.memory_id.to_string()),
            "policy audit target",
        )
    }

    #[test]
    fn remember_memory_secret_detector_allow_phrase_masks_configured_sentence() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = temp.path().join(".ee");
        std::fs::create_dir(&config_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            config_dir.join("config.toml"),
            "[policy.secret_detector]\nallow_phrases = [\"OAuth refresh token\"]\n",
        )
        .map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "OAuth refresh token fixture uses API_KEY=sk-FAKEabc123def456ghi789jkl012 for documentation.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;

        ensure(report.persisted, true, "config bypass persisted")?;
        let bypass = report
            .policy_bypass
            .as_ref()
            .ok_or_else(|| "policy bypass missing".to_owned())?;
        ensure(
            bypass.kind.clone(),
            "config_phrase".to_owned(),
            "config phrase kind",
        )?;
        ensure(
            bypass
                .matches
                .iter()
                .any(|item| item.pattern == "OAuth refresh token"),
            true,
            "allow phrase recorded",
        )
    }

    #[test]
    fn memory_history_report_not_found_is_correct() -> TestResult {
        let report = MemoryHistoryReport::not_found("mem_test".to_string());

        ensure(report.memory_exists, false, "memory_exists")?;
        ensure(report.entries.is_empty(), true, "entries empty")?;
        ensure(report.is_tombstoned, false, "is_tombstoned")?;
        ensure(report.error.is_none(), true, "no error")?;
        ensure(report.memory_id, "mem_test".to_string(), "memory_id")
    }

    #[test]
    fn memory_history_report_error_captures_message() -> TestResult {
        let report = MemoryHistoryReport::error("mem_test".to_string(), "db error".to_string());

        ensure(report.memory_exists, false, "memory_exists")?;
        ensure(report.error, Some("db error".to_string()), "error message")
    }

    #[test]
    fn memory_history_report_found_with_entries() -> TestResult {
        let entries = vec![
            MemoryHistoryEntry {
                audit_id: "audit_001".to_string(),
                timestamp: "2026-04-29T12:00:00Z".to_string(),
                actor: Some("user@example.com".to_string()),
                action: "create".to_string(),
                details: None,
            },
            MemoryHistoryEntry {
                audit_id: "audit_002".to_string(),
                timestamp: "2026-04-29T13:00:00Z".to_string(),
                actor: Some("user@example.com".to_string()),
                action: "update".to_string(),
                details: Some("{\"field\":\"content\"}".to_string()),
            },
        ];

        let report = MemoryHistoryReport::found("mem_test".to_string(), false, entries, 2, false);

        ensure(report.memory_exists, true, "memory_exists")?;
        ensure(report.entries.len(), 2, "entry count")?;
        ensure(report.total_count, 2, "total_count")?;
        ensure(report.truncated, false, "truncated")?;
        ensure(report.is_tombstoned, false, "is_tombstoned")
    }

    #[test]
    fn memory_history_report_version_matches_package() -> TestResult {
        let report = MemoryHistoryReport::not_found("mem_test".to_string());
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    // =========================================================================
    // Memory Revise Tests (EE-066)
    // =========================================================================

    #[test]
    fn revise_reason_as_str_is_stable() -> TestResult {
        ensure(
            ReviseReason::Correction.as_str(),
            "correction",
            "correction",
        )?;
        ensure(ReviseReason::Update.as_str(), "update", "update")?;
        ensure(
            ReviseReason::Refinement.as_str(),
            "refinement",
            "refinement",
        )?;
        ensure(
            ReviseReason::Consolidation.as_str(),
            "consolidation",
            "consolidation",
        )?;
        ensure(
            ReviseReason::Custom("custom-reason".to_owned()).as_str(),
            "custom-reason",
            "custom",
        )
    }

    #[test]
    fn revise_reason_parse_roundtrips() -> TestResult {
        ensure(
            ReviseReason::parse("correction"),
            ReviseReason::Correction,
            "correction",
        )?;
        ensure(
            ReviseReason::parse("update"),
            ReviseReason::Update,
            "update",
        )?;
        ensure(
            ReviseReason::parse("refinement"),
            ReviseReason::Refinement,
            "refinement",
        )?;
        ensure(
            ReviseReason::parse("consolidation"),
            ReviseReason::Consolidation,
            "consolidation",
        )?;
        ensure(
            ReviseReason::parse("my-custom"),
            ReviseReason::Custom("my-custom".to_owned()),
            "custom",
        )
    }

    #[test]
    fn revise_reason_default_is_update() -> TestResult {
        ensure(ReviseReason::default(), ReviseReason::Update, "default")
    }

    #[test]
    fn memory_revise_report_not_found_is_correct() -> TestResult {
        let report = MemoryReviseReport::not_found("mem_missing".to_string());

        ensure(report.success, false, "success")?;
        ensure(report.original_id, "mem_missing".to_string(), "original_id")?;
        ensure(report.new_id.is_none(), true, "new_id is none")?;
        ensure(
            report.error,
            Some("Memory not found".to_owned()),
            "error message",
        )
    }

    #[test]
    fn memory_revise_report_tombstoned_is_correct() -> TestResult {
        let report = MemoryReviseReport::tombstoned("mem_old".to_string());

        ensure(report.success, false, "success")?;
        ensure(report.original_id, "mem_old".to_string(), "original_id")?;
        ensure(
            report.error,
            Some("Cannot revise tombstoned memory".to_owned()),
            "error message",
        )
    }

    #[test]
    fn memory_revise_report_no_changes_is_correct() -> TestResult {
        let report = MemoryReviseReport::no_changes("mem_same".to_string());

        ensure(report.success, false, "success")?;
        ensure(report.original_id, "mem_same".to_string(), "original_id")?;
        ensure(
            report.error,
            Some("No changes specified".to_owned()),
            "error message",
        )
    }

    #[test]
    fn memory_revise_report_success_captures_all_fields() -> TestResult {
        let report = MemoryReviseReport::success(
            "mem_old".to_string(),
            "mem_new".to_string(),
            "rev_group".to_string(),
            2,
            ReviseReason::Correction,
            vec!["content".to_string(), "confidence".to_string()],
            false,
        );

        ensure(report.success, true, "success")?;
        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.original_id, "mem_old".to_string(), "original_id")?;
        ensure(report.new_id, Some("mem_new".to_string()), "new_id")?;
        ensure(
            report.revision_group_id,
            Some("rev_group".to_string()),
            "revision_group_id",
        )?;
        ensure(report.revision_number, Some(2), "revision_number")?;
        ensure(report.reason, "correction".to_string(), "reason")?;
        ensure(report.changed_fields.len(), 2, "changed_fields count")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn memory_revise_report_dry_run_preview_is_correct() -> TestResult {
        let report = MemoryReviseReport::dry_run_preview(
            "mem_test".to_string(),
            ReviseReason::Update,
            vec!["level".to_string()],
        );

        ensure(report.success, true, "success")?;
        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.new_id.is_none(), true, "no new_id for dry run")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "no revision_group_id for dry run",
        )?;
        ensure(report.changed_fields.len(), 1, "changed_fields count")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn memory_revise_report_write_unavailable_preserves_preview_fields() -> TestResult {
        let report = MemoryReviseReport::write_unavailable(
            "mem_old".to_string(),
            ReviseReason::Correction,
            vec!["content".to_string(), "confidence".to_string()],
        );

        ensure(report.success, false, "success")?;
        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.original_id, "mem_old".to_string(), "original_id")?;
        ensure(report.new_id.is_none(), true, "new_id absent")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "revision group absent",
        )?;
        ensure(report.revision_number.is_none(), true, "revision absent")?;
        ensure(report.reason, "correction".to_string(), "reason")?;
        ensure(
            report.changed_fields,
            vec!["content".to_string(), "confidence".to_string()],
            "changed fields",
        )?;
        ensure(
            report
                .error
                .as_deref()
                .is_some_and(|message| message.contains("unavailable")),
            true,
            "unavailable error",
        )
    }

    #[test]
    fn revise_memory_non_dry_run_reports_unavailable_instead_of_stub_success() -> TestResult {
        let (_temp, created) =
            remember_revisable_memory("Store release checks as durable memory.")?;
        let memory_id = created.memory_id.to_string();

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &created.database_path,
            original_memory_id: &memory_id,
            content: Some("Store release checks and clippy gates as durable memory."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Correction,
            actor: Some("SapphireBeacon"),
            dry_run: false,
        });

        ensure(report.success, false, "success")?;
        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.original_id, memory_id.clone(), "original id")?;
        ensure(report.new_id.is_none(), true, "no generated new memory id")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "no generated revision group",
        )?;
        ensure(
            report.revision_number.is_none(),
            true,
            "no stub revision number",
        )?;
        ensure(
            report.changed_fields,
            vec!["content".to_string()],
            "changed fields",
        )?;
        ensure(
            report
                .error
                .as_deref()
                .is_some_and(|message| message.contains("--dry-run")),
            true,
            "repair hint mentions dry-run",
        )?;

        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let original = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "created memory should still exist".to_string())?;
        ensure(
            original.content,
            "Store release checks as durable memory.".to_string(),
            "original content unchanged",
        )
    }

    #[test]
    fn revise_memory_dry_run_preview_preserves_database() -> TestResult {
        let (_temp, created) =
            remember_revisable_memory("Store release checks as durable memory.")?;
        let memory_id = created.memory_id.to_string();

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &created.database_path,
            original_memory_id: &memory_id,
            content: Some("Store release checks and clippy gates as durable memory."),
            level: None,
            kind: None,
            confidence: Some(0.91),
            tags: None,
            provenance_uri: Some("file://README.md#L267"),
            reason: ReviseReason::Correction,
            actor: Some("ProudBasin"),
            dry_run: true,
        });

        ensure(report.success, true, "success")?;
        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.original_id, memory_id.clone(), "original id")?;
        ensure(report.new_id.is_none(), true, "no new id")?;
        ensure(report.revision_number.is_none(), true, "no revision")?;
        ensure(
            report.changed_fields,
            vec![
                "content".to_string(),
                "confidence".to_string(),
                "provenance_uri".to_string(),
            ],
            "changed fields",
        )?;

        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let original = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "created memory should still exist".to_string())?;
        ensure(
            original.content,
            "Store release checks as durable memory.".to_string(),
            "original content unchanged",
        )?;
        ensure(original.confidence, 0.9, "original confidence unchanged")
    }

    #[test]
    fn revise_memory_no_changes_reports_usage_error() -> TestResult {
        let (_temp, created) = remember_revisable_memory("Keep memory revisions honest.")?;
        let memory_id = created.memory_id.to_string();

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &created.database_path,
            original_memory_id: &memory_id,
            content: Some("Keep memory revisions honest."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Update,
            actor: Some("ProudBasin"),
            dry_run: true,
        });

        ensure(report.success, false, "success")?;
        ensure(report.original_id, memory_id, "original id")?;
        ensure(
            report.changed_fields,
            Vec::<String>::new(),
            "changed fields",
        )?;
        ensure(
            report.error,
            Some("No changes specified".to_string()),
            "no changes error",
        )
    }

    #[test]
    fn revise_memory_tombstoned_original_is_denied() -> TestResult {
        let (_temp, created) = remember_revisable_memory("Do not revise tombstoned memories.")?;
        let memory_id = created.memory_id.to_string();
        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let tombstoned = connection
            .tombstone_memory(&memory_id)
            .map_err(|error| error.to_string())?;
        ensure(tombstoned, true, "memory tombstoned")?;

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &created.database_path,
            original_memory_id: &memory_id,
            content: Some("This revision must not be accepted."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Correction,
            actor: Some("ProudBasin"),
            dry_run: true,
        });

        ensure(report.success, false, "success")?;
        ensure(report.original_id, memory_id, "original id")?;
        ensure(
            report.error,
            Some("Cannot revise tombstoned memory".to_string()),
            "tombstoned error",
        )
    }

    #[test]
    fn revise_memory_storage_error_is_reported_without_stub_success() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("missing-parent").join("ee.db");

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &database_path,
            original_memory_id: "mem_missing_storage",
            content: Some("No storage should mean no revision."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Correction,
            actor: Some("ProudBasin"),
            dry_run: true,
        });

        ensure(report.success, false, "success")?;
        ensure(report.new_id.is_none(), true, "no new id")?;
        ensure(report.revision_number.is_none(), true, "no revision")?;
        ensure(
            report
                .error
                .as_deref()
                .is_some_and(|message| message.starts_with("Failed to open database")),
            true,
            "storage error message",
        )
    }

    #[test]
    fn memory_revise_report_version_matches_package() -> TestResult {
        let report = MemoryReviseReport::not_found("mem_test".to_string());
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    // =========================================================================
    // Dedupe Warning Tests (EE-069)
    // =========================================================================

    #[test]
    fn dedupe_severity_as_str_is_stable() -> TestResult {
        ensure(DedupeSeverity::Exact.as_str(), "exact", "exact")?;
        ensure(DedupeSeverity::High.as_str(), "high", "high")?;
        ensure(DedupeSeverity::Medium.as_str(), "medium", "medium")?;
        ensure(DedupeSeverity::Low.as_str(), "low", "low")
    }

    #[test]
    fn dedupe_severity_from_score_thresholds() -> TestResult {
        ensure(
            DedupeSeverity::from_score(1.0),
            DedupeSeverity::Exact,
            "1.0",
        )?;
        ensure(
            DedupeSeverity::from_score(0.95),
            DedupeSeverity::High,
            "0.95",
        )?;
        ensure(
            DedupeSeverity::from_score(0.90),
            DedupeSeverity::High,
            "0.90",
        )?;
        ensure(
            DedupeSeverity::from_score(0.89),
            DedupeSeverity::Medium,
            "0.89",
        )?;
        ensure(
            DedupeSeverity::from_score(0.70),
            DedupeSeverity::Medium,
            "0.70",
        )?;
        ensure(
            DedupeSeverity::from_score(0.69),
            DedupeSeverity::Low,
            "0.69",
        )?;
        ensure(DedupeSeverity::from_score(0.5), DedupeSeverity::Low, "0.5")?;
        ensure(DedupeSeverity::from_score(0.0), DedupeSeverity::Low, "0.0")
    }

    #[test]
    fn dedupe_severity_ordering_is_correct() -> TestResult {
        let exact = DedupeSeverity::Exact;
        let high = DedupeSeverity::High;
        let medium = DedupeSeverity::Medium;
        let low = DedupeSeverity::Low;

        ensure(exact < high, true, "exact < high")?;
        ensure(high < medium, true, "high < medium")?;
        ensure(medium < low, true, "medium < low")
    }

    #[test]
    fn dedupe_match_type_as_str_is_stable() -> TestResult {
        ensure(
            DedupeMatchType::ExactContent.as_str(),
            "exact_content",
            "exact_content",
        )?;
        ensure(
            DedupeMatchType::NormalizedContent.as_str(),
            "normalized_content",
            "normalized_content",
        )?;
        ensure(DedupeMatchType::Semantic.as_str(), "semantic", "semantic")?;
        ensure(DedupeMatchType::Lexical.as_str(), "lexical", "lexical")
    }

    #[test]
    fn jaccard_similarity_identical_strings() -> TestResult {
        let sim = jaccard_similarity("hello world", "hello world");
        ensure((sim - 1.0).abs() < f32::EPSILON, true, "identical = 1.0")
    }

    #[test]
    fn jaccard_similarity_completely_different() -> TestResult {
        let sim = jaccard_similarity("alpha beta", "gamma delta");
        ensure((sim - 0.0).abs() < f32::EPSILON, true, "disjoint = 0.0")
    }

    #[test]
    fn jaccard_similarity_partial_overlap() -> TestResult {
        // "hello world" vs "hello there" -> intersection = {hello}, union = {hello, world, there}
        // Jaccard = 1/3 ≈ 0.333
        let sim = jaccard_similarity("hello world", "hello there");
        ensure(sim > 0.3 && sim < 0.4, true, "partial overlap ~0.33")
    }

    #[test]
    fn jaccard_similarity_empty_strings() -> TestResult {
        let both_empty = jaccard_similarity("", "");
        let one_empty = jaccard_similarity("hello", "");

        ensure(
            (both_empty - 1.0).abs() < f32::EPSILON,
            true,
            "both empty = 1.0",
        )?;
        ensure(
            (one_empty - 0.0).abs() < f32::EPSILON,
            true,
            "one empty = 0.0",
        )
    }

    #[test]
    fn dedupe_check_options_defaults() -> TestResult {
        let opts = DedupeCheckOptions::new(
            std::path::Path::new("/tmp/db"),
            std::path::Path::new("/tmp/workspace"),
            "test content",
        );

        ensure(
            opts.workspace_path,
            std::path::Path::new("/tmp/workspace"),
            "workspace path",
        )?;
        ensure(opts.content, "test content", "content")?;
        ensure(opts.level.is_none(), true, "level none")?;
        ensure(opts.kind.is_none(), true, "kind none")?;
        ensure(
            (opts.min_similarity - 0.5).abs() < f32::EPSILON,
            true,
            "min_similarity",
        )?;
        ensure(opts.max_warnings, 5, "max_warnings")
    }

    #[test]
    fn dedupe_check_scans_requested_workspace() -> TestResult {
        let (_temp, created) = remember_revisable_memory("Run cargo fmt before release checks.")?;
        let report = check_for_duplicates(&DedupeCheckOptions {
            database_path: &created.database_path,
            workspace_path: &created.workspace_path,
            content: "Run cargo fmt before release checks.",
            level: Some("procedural"),
            kind: Some("rule"),
            min_similarity: 0.9,
            max_warnings: 5,
        });

        ensure(report.error.is_none(), true, "no dedupe error")?;
        ensure(report.memories_scanned, 1, "scanned workspace memories")?;
        ensure(report.has_warnings, true, "has duplicate warning")?;
        ensure(report.warnings.len(), 1, "warning count")?;
        ensure(
            report.warnings[0].existing_memory_id.clone(),
            created.memory_id.to_string(),
            "matched non-default workspace memory",
        )?;
        ensure(
            report.warnings[0].match_type,
            DedupeMatchType::ExactContent,
            "exact match",
        )
    }

    #[test]
    fn dedupe_check_report_no_duplicates() -> TestResult {
        let report = DedupeCheckReport::no_duplicates(42);

        ensure(report.has_warnings, false, "has_warnings")?;
        ensure(report.warnings.is_empty(), true, "warnings empty")?;
        ensure(report.memories_scanned, 42, "memories_scanned")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn dedupe_check_report_with_warnings() -> TestResult {
        let warning = DedupeWarning {
            existing_memory_id: "mem_123".to_string(),
            similarity_score: 0.85,
            severity: DedupeSeverity::Medium,
            existing_preview: "preview text".to_string(),
            match_type: DedupeMatchType::Lexical,
            suggestion: "Consider reviewing".to_string(),
        };
        let report = DedupeCheckReport::with_warnings(vec![warning], 100);

        ensure(report.has_warnings, true, "has_warnings")?;
        ensure(report.warnings.len(), 1, "warnings count")?;
        ensure(report.memories_scanned, 100, "memories_scanned")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn dedupe_check_report_error() -> TestResult {
        let report = DedupeCheckReport::error("Database failure".to_string());

        ensure(report.has_warnings, false, "has_warnings")?;
        ensure(report.warnings.is_empty(), true, "warnings empty")?;
        ensure(report.memories_scanned, 0, "memories_scanned")?;
        ensure(
            report.error,
            Some("Database failure".to_string()),
            "error message",
        )
    }

    #[test]
    fn dedupe_check_report_version_matches_package() -> TestResult {
        let report = DedupeCheckReport::no_duplicates(0);
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    // ========================================================================
    // Bead bd-17c65.3.1 (C1) — Validate remember policy: value-shape, not keyword
    // ========================================================================

    /// Plain-English mentions of secret/token/credentials must persist.
    /// The 2026-05-10 walkthrough surfaced these as the worst false-
    /// positives in the old keyword detector. Lock them in as accepted
    /// post-C1.
    ///
    /// Note: we deliberately do NOT include phrases like "Bearer auth"
    /// or "Authorization header" here — those still trip the existing
    /// value-shape detector's key-value patterns (`bearer <value>`,
    /// `authorization: ...`). The value-shape detector's tuning is its
    /// own scope (potentially C2 bypass flag or C5 corpora calibration).
    /// C1's contract is narrower: free-text mentions of `secret`,
    /// `token`, `credentials` as nouns must pass.
    #[test]
    fn validate_remember_policy_accepts_meta_policy_phrases() {
        let temp = match tempfile::tempdir() {
            Ok(temp) => temp,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let acceptable = [
            // The four 2026-05-10 walkthrough cases that the keyword
            // detector blocked:
            "Context packs must never include secrets. Redaction is enforced.",
            "Never embed credentials in stored memories.",
            "Cancellation test for ee context hung once because Scope::spawn didn't propagate the cancel token; fixed via budget.",
            // Additional plain-English mentions that the keyword detector
            // would have caught but value-shape lets through:
            "PEM-encoded keys live in the keystore module.",
        ];
        for content in acceptable {
            match validate_remember_policy(content, temp.path(), false) {
                Ok(None) => {}
                Ok(Some(bypass)) => panic!("C1 false bypass: `{content}` accepted via {bypass:?}"),
                Err(error) => panic!("C1 false positive: `{content}` rejected: {error:?}"),
            }
        }
    }

    /// Real secret VALUES must still be rejected. These are synthetic
    /// look-alikes (never real keys) covering format-prefix patterns the
    /// existing value-shape detector definitively catches.
    #[test]
    fn validate_remember_policy_rejects_real_secret_values() {
        let temp = match tempfile::tempdir() {
            Ok(temp) => temp,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let must_reject = [
            // OpenAI-style — covered by raw_api_tokens regex
            "API_KEY=sk-FAKEabc123def456ghi789jkl012",
            // AWS access key — covered by key=value with AWS_ prefix
            "Set AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE for the build.",
            // PEM block — covered by redact_pem_blocks
            "-----BEGIN PRIVATE KEY-----\nMIIEvQIB...synthetic body...\n-----END PRIVATE KEY-----",
            // URL with embedded password — covered by redact_url_passwords
            "DATABASE_URL=postgres://admin:SuperSecretPass123!@db.example.com/prod",
        ];
        for content in must_reject {
            match validate_remember_policy(content, temp.path(), false) {
                Ok(_) => panic!("C1 false negative: `{content}` should reject"),
                Err(
                    DomainError::PolicyDenied { .. } | DomainError::PolicyDeniedWithDetails { .. },
                ) => {}
                Err(other) => panic!("wrong error variant for `{content}`: {other:?}"),
            }
        }
    }

    /// Structurally-fine content from the 2026-05-10 reference corpus
    /// must not trip the detector (regression guard).
    #[test]
    fn validate_remember_policy_accepts_benign_corpus_content() {
        let temp = match tempfile::tempdir() {
            Ok(temp) => temp,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        for content in [
            "Run cargo fmt --check before cutting any release tag; CI rejects unformatted code.",
            "Forbidden deps: tokio, rusqlite, petgraph, hyper, axum, tower, reqwest, sqlx, diesel, sea-orm — CI greps for them.",
            "ee's core jobs are Ingest, Retrieve, Pack, Learn, Maintain.",
            "JSON output goes to stdout; human diagnostics go to stderr.",
            "All work lands on main. No worktrees. No feature branches.",
        ] {
            match validate_remember_policy(content, temp.path(), false) {
                Ok(None) => {}
                Ok(Some(bypass)) => {
                    panic!("benign content `{content}` accepted via policy bypass: {bypass:?}")
                }
                Err(error) => panic!("benign content `{content}` rejected: {error:?}"),
            }
        }
    }
}
