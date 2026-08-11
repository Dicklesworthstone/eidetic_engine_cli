//! Memory retrieval and inspection operations (EE-063, EE-066).
//!
//! Provides the core use case functions for inspecting stored memories:
//! - `get_memory_details`: retrieve a single memory with its tags and metadata
//! - `revise_memory`: create an immutable revision of an existing memory

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use super::audit_lane::{
    AuditEvent as AuditLaneEvent, AuditLaneHandle, emit_with_direct_fallback, insert_audit_event,
};
use super::bayes::BetaPosterior;
use super::config_surface::{ConfigSurfaceOptions, get_config};
use super::index::{
    DEFAULT_INDEX_SUBDIR, IndexProcessingJobReport, IndexRebuildError,
    process_index_job_for_connection, process_pending_index_jobs_coalesced,
};
use super::memory_lifecycle::{
    LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE, LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE,
    LEVEL_TRANSITION_TOMBSTONED_REJECTED_CODE, MemoryLifecycleState, transition_for,
};
use super::search::{SearchOptions, SearchStatus, run_search};
use crate::config::{ConfigFile, GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY};
use crate::curate::cluster_coherence::{ClusterCoherenceConfig, EmbeddingPoint, agglomerate};
use crate::curate::{CandidateSource, CandidateStatus, CandidateType};
use crate::db::{
    AdvisoryLockId, ApplyMemoryLevelTransitionInput, CreateAuditInput,
    CreateCurationCandidateInput, CreateEvidenceSpanInput, CreateMemoryInput,
    CreateMemoryLinkInput, CreateRememberIdempotencyKeyInput, CreateSearchIndexJobInput,
    CreateSessionInput, CreateWorkspaceInput, DbConnection, DbOperation, EvidenceProducerKind,
    MemoryContentSimHash, MemoryLinkRelation, MemoryLinkSource, SearchIndexJobStatus,
    SearchIndexJobType, StoredMemory, StoredMemoryLink, audit_actions, generate_audit_id,
    generate_audit_id_seeded,
};
use crate::models::{
    DomainError, GLOBAL_MEMORY_SCOPE_TAG, KNOWN_MEMORY_KINDS, KNOWN_MEMORY_LEVELS, MAX_TAG_BYTES,
    MemoryContent, MemoryId, MemoryKind, MemoryLevel, MemoryValidationError, ProducerMetadata,
    ProducerSourceSystem, ProvenanceUri, Tag, TrustClass, UnitScore, WorkspaceId,
};
use crate::obs::{AuditEvent, AuditOutcome, now_rfc3339_nanos};
use crate::runtime::determinism::{Deterministic, Seed};
use crate::search::HashEmbedder;
use crate::search::simhash::{
    EmbedDedupConfig, SimHash128, cosine_similarity, first_confirmed_simhash_candidate,
    ranked_simhash_candidates,
};
use crate::util::radix_ulid_sort::sort_by_ulid_payload_or_lexical;

/// A memory with its associated tags for display.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDetails {
    /// The stored memory record.
    pub memory: StoredMemory,
    /// Tags associated with this memory.
    pub tags: Vec<String>,
    /// Canonical typed memory fields, when the memory kind supports them and
    /// this record has a sidecar.
    pub typed_fields: Option<serde_json::Value>,
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

/// Stable candidate schema for `ee remember --from-commit/--from-diff`.
pub const REMEMBER_GIT_CAPTURE_SCHEMA_V1: &str = "ee.remember.git_capture.v1";
const REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES: usize = 24 * 1024;
const REMEMBER_GIT_CAPTURE_DIFF_EXCERPT_LINES: usize = 96;
const REMEMBER_GIT_CAPTURE_MAX_SURFACES: usize = 24;
const REMEMBER_GIT_CAPTURE_MAX_SYMBOLS: usize = 16;

/// Git source mode for frictionless remember capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RememberGitCaptureMode {
    /// Capture from one commit object.
    Commit,
    /// Capture from `git diff <ref>`.
    Diff,
    /// Capture from the current working tree against `HEAD` when available.
    WorkingTree,
}

impl RememberGitCaptureMode {
    /// Stable wire/display form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Diff => "diff",
            Self::WorkingTree => "working-tree",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Commit => "from-commit",
            Self::Diff => "from-diff",
            Self::WorkingTree => "from-working-tree",
        }
    }
}

/// Raw git evidence used by the deterministic capture transform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RememberGitCaptureInput {
    pub mode: RememberGitCaptureMode,
    pub reference: Option<String>,
    pub commit_sha: Option<String>,
    pub commit_subject: Option<String>,
    pub commit_body: Option<String>,
    pub changed_files: Vec<String>,
    pub diff_text: String,
}

/// A dry-run-first memory candidate derived from git evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RememberGitCaptureCandidate {
    pub schema: &'static str,
    pub mode: RememberGitCaptureMode,
    pub reference: Option<String>,
    pub commit_sha: Option<String>,
    pub content: String,
    pub level: &'static str,
    pub kind: &'static str,
    pub tags: Vec<String>,
    pub source: String,
    pub changed_files: Vec<String>,
    pub changed_symbols: Vec<String>,
    pub diff_fingerprint: String,
    pub redacted: bool,
    pub redaction_reasons: Vec<String>,
}

impl RememberGitCaptureCandidate {
    /// Tags rendered for the existing `ee remember` comma-separated input.
    #[must_use]
    pub fn tags_csv(&self) -> String {
        self.tags.join(",")
    }
}

/// Repository-backed git capture request.
#[derive(Clone, Debug)]
pub struct RememberGitCaptureOptions<'a> {
    pub workspace_path: &'a Path,
    pub mode: RememberGitCaptureMode,
    pub reference: Option<&'a str>,
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
    /// Canonical typed sidecar fields selected for this write.
    pub typed_fields: Option<serde_json::Value>,
    /// Attempt-family multiplicity declaration persisted with this write
    /// (bd-multiplicity-aware-trust-p0u7g).
    pub attempt_family: Option<crate::db::MemoryAttemptFamily>,
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
    /// Near-duplicate memories surfaced for explicit agent review.
    pub near_duplicates: Vec<RememberNearDuplicate>,
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
const REMEMBER_EMBED_DEDUP_CANDIDATE_LIMIT: usize = 16;
const REMEMBER_EMBED_DEDUP_LINK_SCHEMA_V1: &str = "ee.embed_dedup.link.v1";

#[derive(Clone, Debug, PartialEq)]
struct RememberEmbedDedupDecision {
    content_simhash: Option<MemoryContentSimHash>,
    link: Option<RememberEmbedDedupLink>,
    decision: &'static str,
    reason: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
enum RememberEmbedDedupProbe {
    Disabled,
    Enabled {
        hamming_k: u32,
        cosine_floor: f32,
        query_fingerprint: SimHash128,
        content_simhash: MemoryContentSimHash,
        query_embedding: Vec<f32>,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct RememberEmbedDedupLink {
    target_memory_id: String,
    hamming_distance: u32,
    cosine_similarity: f32,
    cosine_floor: f32,
}

impl RememberEmbedDedupDecision {
    const fn disabled() -> Self {
        Self {
            content_simhash: None,
            link: None,
            decision: "new_embed",
            reason: "dedup_disabled",
        }
    }

    const fn fresh(content_simhash: MemoryContentSimHash, reason: &'static str) -> Self {
        Self {
            content_simhash: Some(content_simhash),
            link: None,
            decision: "new_embed",
            reason,
        }
    }

    fn reused(content_simhash: MemoryContentSimHash, link: RememberEmbedDedupLink) -> Self {
        Self {
            content_simhash: Some(content_simhash),
            link: Some(link),
            decision: "reuse",
            reason: "simhash_within_threshold_and_cosine_confirmed",
        }
    }

    fn link_metadata_json(&self) -> Option<String> {
        let link = self.link.as_ref()?;
        Some(
            serde_json::json!({
                "schema": REMEMBER_EMBED_DEDUP_LINK_SCHEMA_V1,
                "relationship": "embedding_reuse",
                "targetMemoryId": link.target_memory_id,
                "hammingDistance": link.hamming_distance,
                "cosineSimilarity": link.cosine_similarity,
                "cosineFloor": link.cosine_floor,
                "decision": self.decision,
                "reason": self.reason,
            })
            .to_string(),
        )
    }
}

fn remember_near_duplicates_from_embed_dedup_decision(
    decision: &RememberEmbedDedupDecision,
) -> Vec<RememberNearDuplicate> {
    decision.link.as_ref().map_or_else(Vec::new, |link| {
        vec![RememberNearDuplicate {
            memory_id: link.target_memory_id.clone(),
            similarity: link.cosine_similarity,
            threshold: link.cosine_floor,
            hamming_distance: link.hamming_distance,
            source: "embedding_reuse".to_owned(),
            next_actions: vec![
                "ee remember --reinforce <same-content>".to_owned(),
                format!("ee memory link <new-memory-id> {}", link.target_memory_id),
                "ee curate candidates --type paraphrase_dedup_proposal --json".to_owned(),
            ],
        }]
    })
}

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

/// A remember-time near-duplicate surfaced without blocking or mutating the
/// existing memory. The new memory may still be stored; agents decide whether
/// to reinforce, supersede, or keep both.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberNearDuplicate {
    /// Existing memory that appears near-duplicate to the new content.
    pub memory_id: String,
    /// Similarity score in 0.0..=1.0.
    pub similarity: f32,
    /// Threshold that admitted this candidate.
    pub threshold: f32,
    /// SimHash Hamming distance used as the cheap deterministic gate.
    pub hamming_distance: u32,
    /// Ranking/source lane that found the candidate.
    pub source: String,
    /// Stable next actions; no silent mutation is performed.
    pub next_actions: Vec<String>,
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
    let mut id_source = RememberIdSource::Ambient;
    remember_memory_inner(options, &mut id_source, None, false, &[], None)
}

/// [`remember_memory`] with the search-index publish optionally deferred
/// (bd-2efx1): the index job is enqueued transactionally but left
/// pending for a later coalesced drain. Batch-lane internal.
fn remember_memory_with_index_mode(
    options: &RememberMemoryOptions<'_>,
    defer_index_processing: bool,
    typed_field_assignments: &[String],
    attempt_family: Option<&RememberAttemptFamily<'_>>,
) -> Result<RememberMemoryReport, DomainError> {
    let mut id_source = RememberIdSource::Ambient;
    remember_memory_inner(
        options,
        &mut id_source,
        None,
        defer_index_processing,
        typed_field_assignments,
        attempt_family,
    )
}

pub fn remember_memory_seeded(
    options: &RememberMemoryOptions<'_>,
    determinism: &mut Deterministic<Seed>,
) -> Result<RememberMemoryReport, DomainError> {
    let mut id_source = RememberIdSource::Seeded(determinism);
    remember_memory_inner(options, &mut id_source, None, false, &[], None)
}

/// Build a dry-run-first memory candidate from a git commit or diff.
///
/// This function is pure and deterministic: callers provide all git text, and
/// the same input produces byte-identical content, kind, tags, source, and
/// fingerprints. The returned content is safe to feed into the existing
/// audited [`remember_memory_with_controls`] path.
#[must_use]
pub fn build_remember_git_capture_candidate(
    input: &RememberGitCaptureInput,
) -> RememberGitCaptureCandidate {
    let changed_files = normalized_git_changed_files(&input.changed_files);
    let redacted_message = redact_git_capture_text(&git_capture_message(
        input.commit_subject.as_deref(),
        input.commit_body.as_deref(),
    ));
    let redacted_diff = redact_git_capture_text(&truncate_utf8_lossless(
        &input.diff_text,
        REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES,
    ));
    let mut redaction_reasons = redacted_message
        .redaction_reasons
        .iter()
        .chain(redacted_diff.redaction_reasons.iter())
        .cloned()
        .collect::<Vec<_>>();
    redaction_reasons.sort_unstable();
    redaction_reasons.dedup();
    let changed_symbols = extract_git_capture_symbols(&redacted_diff.content);
    let kind = suggest_git_capture_kind(&redacted_message.content, &redacted_diff.content);
    let tags = git_capture_tags(input.mode, kind, &changed_files);
    let diff_fingerprint = format!(
        "blake3:{}",
        blake3::hash(redacted_diff.content.as_bytes()).to_hex()
    );
    let source = git_capture_source(
        input.mode,
        input.reference.as_deref(),
        input.commit_sha.as_deref(),
        &diff_fingerprint,
    );
    let content = render_git_capture_content(
        input,
        &changed_files,
        &changed_symbols,
        kind,
        &source,
        &diff_fingerprint,
        &redacted_message.content,
        &redacted_diff.content,
        !redaction_reasons.is_empty(),
        &redaction_reasons,
    );

    RememberGitCaptureCandidate {
        schema: REMEMBER_GIT_CAPTURE_SCHEMA_V1,
        mode: input.mode,
        reference: input.reference.clone(),
        commit_sha: input.commit_sha.clone(),
        content,
        level: "episodic",
        kind,
        tags,
        source,
        changed_files,
        changed_symbols,
        diff_fingerprint,
        redacted: !redaction_reasons.is_empty(),
        redaction_reasons,
    }
}

/// Collect git evidence from a live repository and build a capture candidate.
pub fn remember_git_capture_candidate_from_repo(
    options: &RememberGitCaptureOptions<'_>,
) -> Result<RememberGitCaptureCandidate, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, false)?;
    let git_root = remember_git_root(&workspace_path)?;
    let input = match options.mode {
        RememberGitCaptureMode::Commit => {
            let reference = options.reference.ok_or_else(|| {
                remember_usage_error("--from-commit requires a commit ref such as HEAD".to_owned())
            })?;
            remember_git_capture_commit_input(&git_root, reference)?
        }
        RememberGitCaptureMode::Diff => {
            let reference = options.reference.ok_or_else(|| {
                remember_usage_error(
                    "--from-diff requires a ref; use --from-worktree for the working tree"
                        .to_owned(),
                )
            })?;
            remember_git_capture_diff_input(&git_root, Some(reference))?
        }
        RememberGitCaptureMode::WorkingTree => remember_git_capture_diff_input(&git_root, None)?,
    };
    Ok(build_remember_git_capture_candidate(&input))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GitCaptureRedactedText {
    content: String,
    redaction_reasons: Vec<String>,
}

fn remember_git_capture_commit_input(
    git_root: &Path,
    reference: &str,
) -> Result<RememberGitCaptureInput, DomainError> {
    let reference = validate_git_capture_ref(reference)?;
    let commit_arg = format!("{reference}^{{commit}}");
    let commit_sha = git_command_text(
        git_root,
        &["rev-parse", "--verify", commit_arg.as_str()],
        "resolve commit ref",
    )?
    .lines()
    .next()
    .unwrap_or_default()
    .trim()
    .to_owned();
    if commit_sha.is_empty() {
        return Err(remember_usage_error(format!(
            "git did not resolve commit ref `{reference}`"
        )));
    }

    let message = git_command_text(
        git_root,
        &["log", "-1", "--format=%s%x00%b", commit_sha.as_str()],
        "read commit message",
    )?;
    let (subject, body) = message
        .split_once('\0')
        .map_or((message.trim(), ""), |(subject, body)| {
            (subject.trim(), body.trim())
        });
    let changed_files = git_command_text(
        git_root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--root",
            commit_sha.as_str(),
            "--",
        ],
        "read commit changed files",
    )?
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let diff_text = git_command_text(
        git_root,
        &[
            "show",
            "--format=",
            "--no-ext-diff",
            "--find-renames",
            "--unified=80",
            commit_sha.as_str(),
            "--",
        ],
        "read commit diff",
    )?;

    Ok(RememberGitCaptureInput {
        mode: RememberGitCaptureMode::Commit,
        reference: Some(reference),
        commit_sha: Some(commit_sha),
        commit_subject: (!subject.is_empty()).then(|| subject.to_owned()),
        commit_body: (!body.is_empty()).then(|| body.to_owned()),
        changed_files,
        diff_text,
    })
}

fn remember_git_capture_diff_input(
    git_root: &Path,
    reference: Option<&str>,
) -> Result<RememberGitCaptureInput, DomainError> {
    let reference = reference.map(validate_git_capture_ref).transpose()?;
    let mut diff_args = vec![
        "diff".to_owned(),
        "--no-ext-diff".to_owned(),
        "--find-renames".to_owned(),
        "--unified=80".to_owned(),
    ];
    let mut name_args = vec!["diff".to_owned(), "--name-only".to_owned()];
    let mode = if let Some(reference) = reference.as_deref() {
        diff_args.push(reference.to_owned());
        name_args.push(reference.to_owned());
        RememberGitCaptureMode::Diff
    } else {
        if git_head_exists(git_root) {
            diff_args.push("HEAD".to_owned());
            name_args.push("HEAD".to_owned());
        }
        RememberGitCaptureMode::WorkingTree
    };
    diff_args.push("--".to_owned());
    name_args.push("--".to_owned());

    let diff_arg_refs = diff_args.iter().map(String::as_str).collect::<Vec<_>>();
    let name_arg_refs = name_args.iter().map(String::as_str).collect::<Vec<_>>();
    let changed_files = git_command_text(git_root, &name_arg_refs, "read diff changed files")?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let diff_text = git_command_text(git_root, &diff_arg_refs, "read diff text")?;

    Ok(RememberGitCaptureInput {
        mode,
        reference,
        commit_sha: None,
        commit_subject: None,
        commit_body: None,
        changed_files,
        diff_text,
    })
}

fn remember_git_root(workspace_path: &Path) -> Result<PathBuf, DomainError> {
    let root = git_command_text(
        workspace_path,
        &["rev-parse", "--show-toplevel"],
        "resolve git root",
    )?;
    let root = root.lines().next().unwrap_or_default().trim();
    if root.is_empty() {
        return Err(remember_usage_error(format!(
            "{} is not inside a git repository",
            workspace_path.display()
        )));
    }
    Ok(PathBuf::from(root))
}

fn git_head_exists(git_root: &Path) -> bool {
    git_command_text(git_root, &["rev-parse", "--verify", "HEAD"], "check HEAD").is_ok()
}

fn git_command_text(
    git_root: &Path,
    args: &[&str],
    phase: &'static str,
) -> Result<String, DomainError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(args)
        .output()
        .map_err(|error| DomainError::Configuration {
            message: format!("Failed to run git while trying to {phase}: {error}"),
            repair: Some("Install git and run this command inside a git workspace.".to_owned()),
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(remember_usage_error(format!(
        "git {} failed while trying to {phase}: {}",
        args.join(" "),
        stderr.trim()
    )))
}

fn validate_git_capture_ref(reference: &str) -> Result<String, DomainError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err(remember_usage_error("git ref cannot be empty".to_owned()));
    }
    if trimmed.starts_with('-') {
        return Err(remember_usage_error(
            "git ref for remember capture must not start with '-'".to_owned(),
        ));
    }
    if trimmed
        .chars()
        .any(|character| character.is_ascii_control() || character.is_whitespace())
    {
        return Err(remember_usage_error(
            "git ref for remember capture must not contain whitespace or control characters"
                .to_owned(),
        ));
    }
    Ok(trimmed.to_owned())
}

fn git_capture_message(subject: Option<&str>, body: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(subject) = subject.map(str::trim).filter(|value| !value.is_empty()) {
        parts.push(subject.to_owned());
    }
    if let Some(body) = body.map(str::trim).filter(|value| !value.is_empty()) {
        parts.push(body.to_owned());
    }
    parts.join("\n\n")
}

fn redact_git_capture_text(content: &str) -> GitCaptureRedactedText {
    let report = crate::policy::redact_secret_like_content(content);
    let mut redaction_reasons = report
        .redacted_reasons
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    redaction_reasons.sort_unstable();
    redaction_reasons.dedup();
    GitCaptureRedactedText {
        content: report.content,
        redaction_reasons,
    }
}

fn normalized_git_changed_files(changed_files: &[String]) -> Vec<String> {
    let mut unique = BTreeSet::new();
    for raw in changed_files {
        let path = raw.trim().trim_start_matches("./").replace('\\', "/");
        if path.is_empty()
            || path.starts_with('/')
            || path.contains("://")
            || path.split('/').any(|component| component == "..")
            || path.chars().any(char::is_control)
        {
            continue;
        }
        unique.insert(path);
    }
    unique
        .into_iter()
        .take(REMEMBER_GIT_CAPTURE_MAX_SURFACES)
        .collect()
}

fn suggest_git_capture_kind(message: &str, diff: &str) -> &'static str {
    let lower = format!("{message}\n{diff}").to_ascii_lowercase();
    if [
        "anti-pattern",
        "antipattern",
        "avoid ",
        "never ",
        "unsafe habit",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "anti-pattern";
    }
    if [
        "fix",
        "bug",
        "regression",
        "failure",
        "failed",
        "panic",
        "error",
        "broken",
        "repair",
        "revert",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "failure";
    }
    if [
        "decision:",
        "decide",
        "decided",
        "chosen:",
        "rationale:",
        "adr",
        "choose ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "decision";
    }
    "fact"
}

fn git_capture_tags(
    mode: RememberGitCaptureMode,
    kind: &str,
    changed_files: &[String],
) -> Vec<String> {
    let mut tags = BTreeSet::from([
        "git".to_owned(),
        "capture".to_owned(),
        mode.tag().to_owned(),
        kind.replace('-', "_"),
    ]);
    for path in changed_files {
        if let Some(first) = path.split('/').next() {
            if let Some(tag) = sanitize_git_capture_tag(first) {
                tags.insert(tag);
            }
        }
        if let Some(extension) = Path::new(path).extension().and_then(|value| value.to_str()) {
            let tag = match extension {
                "rs" => Some("rust".to_owned()),
                "md" => Some("docs".to_owned()),
                "sh" => Some("shell".to_owned()),
                other => sanitize_git_capture_tag(other),
            };
            if let Some(tag) = tag {
                tags.insert(tag);
            }
        }
    }
    tags.into_iter().collect()
}

fn sanitize_git_capture_tag(raw: &str) -> Option<String> {
    let mut tag = raw
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if matches!(character, '.' | '_' | ':' | '-') {
                Some(character)
            } else {
                None
            }
        })
        .collect::<String>();
    while tag
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '.' | ':' | '-' | '_'))
    {
        tag.remove(0);
    }
    while tag
        .chars()
        .last()
        .is_some_and(|character| matches!(character, '.' | ':' | '-' | '_'))
    {
        tag.pop();
    }
    (!tag.is_empty()).then_some(tag)
}

fn git_capture_source(
    mode: RememberGitCaptureMode,
    reference: Option<&str>,
    commit_sha: Option<&str>,
    diff_fingerprint: &str,
) -> String {
    match mode {
        RememberGitCaptureMode::Commit => {
            format!("git-sha://{}", commit_sha.unwrap_or("unknown"))
        }
        RememberGitCaptureMode::Diff | RememberGitCaptureMode::WorkingTree => {
            let reference = reference
                .map(sanitize_git_capture_ref_for_source)
                .unwrap_or_else(|| "working-tree".to_owned());
            let short_hash = diff_fingerprint
                .strip_prefix("blake3:")
                .unwrap_or(diff_fingerprint)
                .chars()
                .take(16)
                .collect::<String>();
            format!("git-sha://diff/{reference}/{short_hash}")
        }
    }
}

fn sanitize_git_capture_ref_for_source(reference: &str) -> String {
    let sanitized = reference
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '~') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized
    }
}

fn render_git_capture_content(
    input: &RememberGitCaptureInput,
    changed_files: &[String],
    changed_symbols: &[String],
    kind: &str,
    source: &str,
    diff_fingerprint: &str,
    redacted_message: &str,
    redacted_diff: &str,
    redacted: bool,
    redaction_reasons: &[String],
) -> String {
    let mut lines = Vec::new();
    let reference = input.reference.as_deref().unwrap_or("working tree");
    let headline = match input.mode {
        RememberGitCaptureMode::Commit => {
            let sha = input.commit_sha.as_deref().unwrap_or("unknown");
            format!("Git commit `{sha}` captured a durable {kind} memory from `{reference}`.")
        }
        RememberGitCaptureMode::Diff => {
            format!("Git diff `{reference}` captured a durable {kind} memory candidate.")
        }
        RememberGitCaptureMode::WorkingTree => {
            format!("Git working tree diff captured a durable {kind} memory candidate.")
        }
    };
    lines.push(headline);
    lines.push(format!("Source: {source}."));
    lines.push(format!("Diff fingerprint: {diff_fingerprint}."));
    lines.push(format!("Mode: {}.", input.mode.as_str()));
    if redacted {
        let reasons = if redaction_reasons.is_empty() {
            "unknown".to_owned()
        } else {
            redaction_reasons.join(",")
        };
        lines.push(format!(
            "Redaction: secret-like diff or message content was redacted before memory capture ({reasons})."
        ));
    } else {
        lines.push("Redaction: no secret-like diff or message content detected.".to_owned());
    }
    if changed_files.is_empty() {
        lines.push("Changed surfaces: none reported by git.".to_owned());
    } else {
        let rendered = changed_files
            .iter()
            .map(|path| format!("`{path}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Changed surfaces: {rendered}."));
        lines.push(format!(
            "Anchor tokens: {}.",
            changed_files
                .iter()
                .map(|path| format!("ee-anchor:path:{path}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !changed_symbols.is_empty() {
        let rendered = changed_symbols
            .iter()
            .map(|symbol| format!("`{symbol}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Changed symbols: {rendered}."));
        lines.push(format!(
            "Symbol anchors: {}.",
            changed_symbols
                .iter()
                .map(|symbol| format!("ee-anchor:symbol:{symbol}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !redacted_message.trim().is_empty() {
        lines.push("Message evidence:".to_owned());
        lines.push(truncate_utf8_lossless(redacted_message.trim(), 4096));
    }
    let excerpt = git_capture_diff_excerpt(redacted_diff);
    if !excerpt.is_empty() {
        lines.push("Redacted diff excerpt:".to_owned());
        lines.push("```diff".to_owned());
        lines.push(excerpt);
        lines.push("```".to_owned());
    }
    lines.join("\n")
}

fn git_capture_diff_excerpt(diff: &str) -> String {
    diff.lines()
        .take(REMEMBER_GIT_CAPTURE_DIFF_EXCERPT_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_utf8_lossless(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = input[..end].to_owned();
    output.push_str("\n... [truncated]");
    output
}

fn extract_git_capture_symbols(diff: &str) -> Vec<String> {
    let mut symbols = BTreeSet::new();
    for line in diff.lines() {
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        if added.starts_with("+++") {
            continue;
        }
        if let Some(symbol) = extract_symbol_from_added_line(added.trim_start()) {
            symbols.insert(symbol);
        }
        if symbols.len() >= REMEMBER_GIT_CAPTURE_MAX_SYMBOLS {
            break;
        }
    }
    symbols.into_iter().collect()
}

fn extract_symbol_from_added_line(line: &str) -> Option<String> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        match *token {
            "fn" => {
                let raw = tokens.get(index + 1)?;
                return sanitize_git_capture_symbol(raw);
            }
            "struct" | "enum" | "trait" | "mod" | "type" => {
                let raw = tokens.get(index + 1)?;
                return sanitize_git_capture_symbol(raw);
            }
            "impl" => {
                let raw = tokens.get(index + 1)?;
                if !raw.starts_with('<') {
                    return sanitize_git_capture_symbol(raw);
                }
            }
            _ => {}
        }
    }
    None
}

fn sanitize_git_capture_symbol(raw: &str) -> Option<String> {
    let symbol = raw
        .trim_matches(|character: char| {
            matches!(
                character,
                '(' | ')' | '{' | '}' | '[' | ']' | '<' | '>' | ',' | ';' | ':'
            )
        })
        .split(['(', '<', '{', ':', '='])
        .next()
        .unwrap_or_default()
        .trim();
    if symbol.is_empty() {
        return None;
    }
    let cleaned = symbol
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect::<String>();
    (!cleaned.is_empty()
        && cleaned
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic() || character == '_'))
    .then_some(cleaned)
}

enum RememberIdSource<'a> {
    Ambient,
    Seeded(&'a mut Deterministic<Seed>),
}

impl RememberIdSource<'_> {
    fn next_memory_id(&mut self) -> MemoryId {
        match self {
            Self::Ambient => MemoryId::now(),
            Self::Seeded(determinism) => MemoryId::now_seeded(determinism),
        }
    }

    fn next_audit_id(&mut self) -> String {
        match self {
            Self::Ambient => generate_audit_id(),
            Self::Seeded(determinism) => generate_audit_id_seeded(determinism),
        }
    }

    fn next_search_index_job_id(&mut self) -> String {
        match self {
            Self::Ambient => generate_search_index_job_id(),
            Self::Seeded(determinism) => generate_search_index_job_id_seeded(determinism),
        }
    }
}

fn remember_memory_inner(
    options: &RememberMemoryOptions<'_>,
    id_source: &mut RememberIdSource<'_>,
    audit_lane: Option<&AuditLaneHandle>,
    defer_index_processing: bool,
    typed_field_assignments: &[String],
    attempt_family: Option<&RememberAttemptFamily<'_>>,
) -> Result<RememberMemoryReport, DomainError> {
    validate_remember_level_kind_cross_wire(options.level, options.kind)?;
    remember_memory_inner_with_store(
        options,
        id_source,
        audit_lane,
        defer_index_processing,
        None,
        typed_field_assignments,
        attempt_family,
    )
}

fn remember_memory_inner_with_store(
    options: &RememberMemoryOptions<'_>,
    id_source: &mut RememberIdSource<'_>,
    audit_lane: Option<&AuditLaneHandle>,
    defer_index_processing: bool,
    store_override: Option<&RememberStoreOverride>,
    typed_field_assignments: &[String],
    attempt_family: Option<&RememberAttemptFamily<'_>>,
) -> Result<RememberMemoryReport, DomainError> {
    let prepared = prepare_remember_memory_with_store(
        options,
        id_source.next_memory_id(),
        store_override,
        typed_field_assignments,
        attempt_family,
    )?;
    if options.dry_run {
        let typed_fields =
            remember_typed_fields_value(&prepared.kind, prepared.typed_fields_json.as_deref())?;
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
            typed_fields,
            attempt_family: prepared.attempt_family,
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
            near_duplicates: Vec::new(),
        });
    }

    ensure_database_parent_exists(&prepared.database_path)?;
    let connection = open_remember_database_with_retry(&prepared.database_path)?;
    migrate_remember_database_with_retry(&connection)?;
    ensure_workspace(
        &connection,
        &prepared.workspace_id,
        &prepared.workspace_path,
    )?;

    let memory_id = prepared.memory_id.to_string();
    let audit_id = id_source.next_audit_id();
    let policy_bypass_audit_id = prepared
        .policy_bypass
        .as_ref()
        .map(|_| id_source.next_audit_id());
    let index_job_id = id_source.next_search_index_job_id();
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
        trust_class: if prepared.attempt_family.is_some()
            && super::memory_scope::current_agent_name().is_some()
        {
            // bd-multiplicity-aware-trust-p0u7g: an attempt-family write from
            // a registered agent identity (the same actor signal
            // remember_trust_subclass records) is an agent fan-out record and
            // enters at agent_assertion — the class the promotion gate holds
            // it at until every declared sibling slot is recorded. Human
            // --family writes keep ADR 0009's human_explicit posture; their
            // multiplicity still surfaces through reporting and ranking
            // discounts. The actor signal, never the flag alone, decides the
            // class.
            TrustClass::AgentAssertion.as_str().to_owned()
        } else {
            TrustClass::HumanExplicit.as_str().to_owned()
        },
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
    let index_input = CreateSearchIndexJobInput {
        workspace_id: prepared.workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(memory_id.clone()),
        documents_total: 1,
    };
    // Compute the immutable query fingerprint/vector before entering the
    // serialized writer lane. Candidate lookup remains inside the lane so a
    // writer that waited behind an identical remember observes and links the
    // row committed by its predecessor.
    let embed_dedup_probe = remember_embed_dedup_probe_from_env(&memory_input)?;
    let audit_details = remember_audit_details(
        &memory_id,
        &memory_input,
        policy_bypass.as_ref(),
        prepared.attempt_family.as_ref(),
    );
    let typed_fields_json = prepared.typed_fields_json.clone();

    let write_operation = crate::core::write_owner::WriteOperation::MemoryCreate {
        workspace_id: prepared.workspace_id.clone(),
        content: prepared.content.clone(),
        level: prepared.level.as_str().to_owned(),
        kind: prepared.kind.as_str().to_owned(),
        tags: prepared.tags.clone(),
        source_id: None,
        trust_class: if prepared.attempt_family.is_some()
            && super::memory_scope::current_agent_name().is_some()
        {
            // bd-multiplicity-aware-trust-p0u7g: an attempt-family write from
            // a registered agent identity (the same actor signal
            // remember_trust_subclass records) is an agent fan-out record and
            // enters at agent_assertion — the class the promotion gate holds
            // it at until every declared sibling slot is recorded. Human
            // --family writes keep ADR 0009's human_explicit posture; their
            // multiplicity still surfaces through reporting and ranking
            // discounts. The actor signal, never the flag alone, decides the
            // class.
            TrustClass::AgentAssertion.as_str().to_owned()
        } else {
            TrustClass::HumanExplicit.as_str().to_owned()
        },
        provenance_uri: prepared.provenance_uri.clone(),
        observed_at_ms: u64::try_from(Utc::now().timestamp_millis()).unwrap_or(0),
    };
    // Serialize only source-of-truth mutations. Expensive embedding
    // preparation above and derived index publication below have their own
    // consistency boundaries and must not consume another writer's
    // workspace-lock budget.
    let workspace_write_lock =
        acquire_remember_workspace_lock(&connection, &prepared.workspace_id, &memory_id)?;
    let embed_dedup_decision =
        remember_embed_dedup_decision_from_probe(&connection, &memory_input, &embed_dedup_probe)?;
    let near_duplicates = remember_near_duplicates_from_embed_dedup_decision(&embed_dedup_decision);
    let embed_dedup_link_id = embed_dedup_decision
        .link
        .as_ref()
        .map(|_| generate_memory_link_id());
    let mut write_replay_guard = RememberWriteReplayGuard::arm(&prepared.workspace_path)?;
    crate::core::write_owner::run_one_shot_write_intake(
        &prepared.workspace_path,
        &write_operation,
        || {
            store_remembered_memory_with_retry(
                &connection,
                &memory_id,
                &audit_id,
                &index_job_id,
                &memory_input,
                typed_fields_json.as_deref(),
                prepared.attempt_family.as_ref(),
                &embed_dedup_decision,
                embed_dedup_link_id.as_deref(),
                &audit_details,
                &index_input,
                policy_bypass.as_ref(),
                audit_lane,
            )
        },
    )?;

    append_remember_audit_jsonl(&prepared, &audit_id, &memory_id, &memory_input)?;

    let (mut auto_links, mut auto_link_status, mut auto_link_degradations) =
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

    let (mut suggested_links, mut suggested_link_status, suggested_link_degradations) =
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

    // bd-pp1fk: auto-persist the strongest co-tag neighbors as audited links so
    // ordinary tagged remembers populate the graph (not only workflow-scoped
    // ones). Gated on the same `--auto-link` toggle (default on). Persisted
    // targets are removed from the advisory `suggested_links` set so a memory is
    // never both auto-linked and re-suggested.
    {
        let existing_auto_link_targets: BTreeSet<String> = auto_links
            .iter()
            .map(|link| link.target_memory_id.clone())
            .collect();
        match persist_high_confidence_cotag_links(
            &connection,
            &prepared.workspace_id,
            &memory_id,
            options.auto_link,
            &existing_auto_link_targets,
            &suggested_links,
        ) {
            Ok(cotag_links) if !cotag_links.is_empty() => {
                let persisted: BTreeSet<String> = cotag_links
                    .iter()
                    .map(|link| link.target_memory_id.clone())
                    .collect();
                suggested_links.retain(|link| !persisted.contains(&link.target_memory_id));
                if suggested_links.is_empty() && suggested_link_status == "ready" {
                    suggested_link_status = "no_candidates".to_owned();
                }
                auto_links.extend(cotag_links);
                // We linked, so drop the workflow-less "use ee memory link"
                // advisory and report the honest "linked" status.
                auto_link_degradations
                    .retain(|degradation| degradation.code != "auto_link_disabled");
                if auto_link_status == "no_workflow_required" || auto_link_status == "no_candidates"
                {
                    auto_link_status = "linked".to_owned();
                }
            }
            Ok(_) => {}
            Err(error) => {
                auto_link_degradations.push(RememberSuggestedLinkDegradation {
                    code: "remember_cotag_auto_link_failed".to_owned(),
                    severity: "low".to_owned(),
                    message: format!(
                        "Remembered the memory, but co-tag auto-linking failed: {}",
                        error.message()
                    ),
                    repair: "Run `ee doctor --json` and inspect memory link indexes.".to_owned(),
                });
            }
        }
    }

    write_replay_guard.mark_clean()?;
    drop(workspace_write_lock);

    let index_dir = prepared.index_dir.clone();
    let index_report = if defer_index_processing {
        // bd-2efx1: leave the job pending; the batch lane drains every
        // pending job with one coalesced rebuild after its last line.
        IndexProcessingJobReport {
            job_id: index_job_id.clone(),
            job_type: SearchIndexJobType::SingleDocument.as_str().to_owned(),
            document_source: Some("memory".to_owned()),
            document_id: None,
            outcome: "skipped".to_owned(),
            processing_mode: "deferred_to_coalesced_batch_rebuild".to_owned(),
            documents_total: 1,
            documents_indexed: 0,
            error: None,
            fallback_to_full: None,
        }
    } else {
        match remember_inline_index_publish_route(
            &connection,
            &prepared.workspace_id,
            &index_job_id,
        ) {
            RememberIndexPublishRoute::Defer => {
                remember_index_job_queued_for_coalescing(&index_job_id)
            }
            RememberIndexPublishRoute::LeadCoalescedDrain => remember_lead_coalesced_index_drain(
                &connection,
                &prepared.workspace_id,
                &index_job_id,
                &index_dir,
            ),
            RememberIndexPublishRoute::Inline => {
                let report =
                    process_remember_index_job_with_retry(&connection, &index_job_id, &index_dir)?;
                remember_drain_peer_tail_after_publish(
                    &connection,
                    &prepared.workspace_id,
                    &index_dir,
                );
                report
            }
        }
    };
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

    let typed_fields =
        remember_typed_fields_value(&prepared.kind, prepared.typed_fields_json.as_deref())?;
    let report = RememberMemoryReport {
        version: env!("CARGO_PKG_VERSION"),
        memory_id: prepared.memory_id,
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path,
        database_path: prepared.database_path,
        content: prepared.content,
        workflow_id: prepared.workflow_id,
        level: prepared.level,
        kind: prepared.kind,
        typed_fields,
        attempt_family: prepared.attempt_family,
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
        near_duplicates,
    };
    if let Err(error) = connection.close() {
        tracing::warn!(
            target: "ee::memory",
            event = "remember_connection_close_failed",
            database_path = %report.database_path.display(),
            error = %error,
        );
    }
    Ok(report)
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
            repair: Some(crate::core::storeless_workspace_repair(&database_path)),
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
            repair: Some(crate::core::storeless_workspace_repair(&database_path)),
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

const REMEMBER_CONTENTION_MAX_ATTEMPTS: usize = 64;
const REMEMBER_WORKSPACE_LOCK_TTL_SECS: u64 = 300;
const REMEMBER_WORKSPACE_LOCK_MAX_WAIT: Duration = Duration::from_secs(300);
const REMEMBER_ADVISORY_LOCK_REPAIR_COMMAND: &str =
    "ee diag advisory-lock --workspace . --resource-type workspace --release --json";

struct RememberWorkspaceWriteLock<'a> {
    connection: &'a DbConnection,
    lock_id: AdvisoryLockId,
    holder_id: String,
}

impl Drop for RememberWorkspaceWriteLock<'_> {
    fn drop(&mut self) {
        if let Err(error) = self
            .connection
            .release_advisory_lock(&self.lock_id, &self.holder_id)
        {
            tracing::warn!(
                target: "ee::memory",
                resource_type = self.lock_id.resource_type(),
                workspace_id = self.lock_id.resource_id(),
                holder_id = self.holder_id.as_str(),
                error = %error,
                "remember workspace advisory lock release failed"
            );
        }
    }
}

fn acquire_remember_workspace_lock<'a>(
    connection: &'a DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> Result<RememberWorkspaceWriteLock<'a>, DomainError> {
    acquire_remember_workspace_lock_with_retry(
        connection,
        workspace_id,
        memory_id,
        REMEMBER_CONTENTION_MAX_ATTEMPTS,
        REMEMBER_WORKSPACE_LOCK_MAX_WAIT,
        remember_write_retry_delay,
    )
}

fn acquire_remember_workspace_lock_with_retry<'a>(
    connection: &'a DbConnection,
    workspace_id: &str,
    memory_id: &str,
    attempts: usize,
    max_wait: Duration,
    retry_delay: impl Fn(usize) -> Duration,
) -> Result<RememberWorkspaceWriteLock<'a>, DomainError> {
    let started = Instant::now();
    acquire_remember_workspace_lock_with_retry_and_elapsed(
        connection,
        workspace_id,
        memory_id,
        attempts,
        max_wait,
        retry_delay,
        || started.elapsed(),
    )
}

fn acquire_remember_workspace_lock_with_retry_and_elapsed<'a>(
    connection: &'a DbConnection,
    workspace_id: &str,
    memory_id: &str,
    attempts: usize,
    max_wait: Duration,
    retry_delay: impl Fn(usize) -> Duration,
    mut elapsed: impl FnMut() -> Duration,
) -> Result<RememberWorkspaceWriteLock<'a>, DomainError> {
    let lock_id = AdvisoryLockId::workspace(workspace_id);
    let holder_id = format!("remember:{}:{memory_id}", std::process::id());
    let attempts = attempts.max(1);
    let mut progress_token: Option<(String, String)> = None;

    // Progress-aware waiting (bd-rs4cm): `attempts` bounds CONSECUTIVE polls
    // that observe the SAME holder — a stagnant lock (wedged or leaked
    // holder) still fails within the old fixed budget. A holder that
    // CHANGES between polls is a queue making progress, so the wait
    // continues rather than dropping the write with a hard storage error
    // (the old fixed cliff starved N-writer queues whose total service time
    // exceeded ~38s). The ambient Cx remains the overall cancellation/deadline
    // bound when present; the elapsed ceiling is the fail-safe for synchronous
    // callers that do not install one. Unlike the removed total-poll ceiling,
    // elapsed time does not punish a rapidly progressing deep queue.
    let mut no_progress_polls = 0usize;

    loop {
        if elapsed() >= max_wait {
            return Err(DomainError::Storage {
                message: format!(
                    "advisory lock timeout after {}ms while waiting for workspace write lock",
                    max_wait.as_millis()
                ),
                repair: Some(REMEMBER_ADVISORY_LOCK_REPAIR_COMMAND.to_owned()),
            });
        }
        remember_retry_sleep(Duration::ZERO, "acquire advisory lock")?;
        match connection.acquire_advisory_lock(
            &lock_id,
            &holder_id,
            Some(REMEMBER_WORKSPACE_LOCK_TTL_SECS),
            Some("remember workspace write"),
        ) {
            Ok(crate::db::AcquireLockResult::Acquired(_))
            | Ok(crate::db::AcquireLockResult::Expired { .. }) => {
                return Ok(RememberWorkspaceWriteLock {
                    connection,
                    lock_id,
                    holder_id,
                });
            }
            Ok(crate::db::AcquireLockResult::AlreadyHeld {
                holder_id,
                acquired_at,
            }) => {
                let current = (holder_id, acquired_at);
                if progress_token.as_ref() == Some(&current) {
                    no_progress_polls += 1;
                } else {
                    progress_token = Some(current);
                    no_progress_polls = 0;
                }
                if no_progress_polls >= attempts {
                    break;
                }
                remember_retry_sleep(retry_delay(no_progress_polls), "acquire advisory lock")?;
            }
            Err(error) if remember_write_contention_is_retryable(&error) => {
                no_progress_polls += 1;
                if no_progress_polls >= attempts {
                    return Err(DomainError::Storage {
                        message: format!(
                            "advisory lock timeout after {no_progress_polls} no-progress polls because the workspace write lock remained unavailable"
                        ),
                        repair: Some(REMEMBER_ADVISORY_LOCK_REPAIR_COMMAND.to_owned()),
                    });
                }
                remember_retry_sleep(retry_delay(no_progress_polls - 1), "acquire advisory lock")?;
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!("Failed to acquire workspace advisory lock: {error}"),
                    repair: Some(REMEMBER_ADVISORY_LOCK_REPAIR_COMMAND.to_owned()),
                });
            }
        }
    }

    Err(DomainError::Storage {
        message: format!(
            "advisory lock timeout after {no_progress_polls} no-progress polls while waiting for workspace write lock"
        ),
        repair: Some(REMEMBER_ADVISORY_LOCK_REPAIR_COMMAND.to_owned()),
    })
}

fn open_remember_database_with_retry(database_path: &Path) -> Result<DbConnection, DomainError> {
    for attempt in 0..REMEMBER_CONTENTION_MAX_ATTEMPTS {
        match DbConnection::open_file(database_path) {
            Ok(connection) => return Ok(connection),
            Err(error) if remember_write_contention_is_retryable(&error) => {
                if attempt + 1 < REMEMBER_CONTENTION_MAX_ATTEMPTS {
                    remember_retry_sleep(remember_write_retry_delay(attempt), "open database")?;
                } else {
                    return Err(DomainError::Storage {
                        message: format!(
                            "Failed to open database after contention retries: {error}"
                        ),
                        repair: Some("ee doctor".to_string()),
                    });
                }
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!("Failed to open database: {error}"),
                    repair: Some("ee doctor".to_string()),
                });
            }
        }
    }

    Err(DomainError::Storage {
        message: "Failed to open database after contention retries.".to_owned(),
        repair: Some("ee doctor".to_string()),
    })
}

fn migrate_remember_database_with_retry(connection: &DbConnection) -> Result<(), DomainError> {
    for attempt in 0..REMEMBER_CONTENTION_MAX_ATTEMPTS {
        match connection.migrate() {
            Ok(_) => return Ok(()),
            Err(error) if remember_write_contention_is_retryable(&error) => {
                if attempt + 1 < REMEMBER_CONTENTION_MAX_ATTEMPTS {
                    remember_retry_sleep(remember_write_retry_delay(attempt), "migrate database")?;
                } else {
                    return Err(DomainError::Storage {
                        message: format!(
                            "Failed to migrate database after contention retries: {error}"
                        ),
                        repair: Some("ee doctor".to_string()),
                    });
                }
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!("Failed to migrate database: {error}"),
                    repair: Some("ee doctor".to_string()),
                });
            }
        }
    }

    Err(DomainError::Storage {
        message: "Failed to migrate database after contention retries.".to_owned(),
        repair: Some("ee doctor".to_string()),
    })
}

fn process_remember_index_job_with_retry(
    connection: &DbConnection,
    index_job_id: &str,
    index_dir: &Path,
) -> Result<IndexProcessingJobReport, DomainError> {
    for attempt in 0..REMEMBER_CONTENTION_MAX_ATTEMPTS {
        match process_index_job_for_connection(connection, index_job_id, index_dir) {
            Ok(report) => return Ok(report),
            Err(error) if remember_write_contention_is_retryable(&error) => {
                if attempt + 1 < REMEMBER_CONTENTION_MAX_ATTEMPTS {
                    remember_retry_sleep(remember_write_retry_delay(attempt), "publish index job")?;
                } else {
                    return Ok(remember_index_job_queued_after_transient_failure(
                        index_job_id,
                        error,
                    ));
                }
            }
            // The memory, audit row, and index job are already committed before
            // this best-effort inline publish begins. Under a deep writer queue,
            // a process can exhaust the index runtime deadline while waiting for
            // or rebuilding the derived index. Reporting the whole remember as
            // failed would lie about that durable mutation and invites duplicate
            // retries. Leave the cancelled job for the normal public requeue /
            // coalesced-reconcile path and report the truthful queued posture.
            Err(error) if remember_index_failure_is_deferable(&error) => {
                return Ok(remember_index_job_queued_after_transient_failure(
                    index_job_id,
                    error,
                ));
            }
            Err(error) => return Err(remember_search_index_error(error)),
        }
    }

    Err(DomainError::SearchIndex {
        message: "Remembered memory but failed to publish search index after contention retries."
            .to_owned(),
        repair: Some("ee index rebuild --workspace .".to_owned()),
    })
}

/// How an inline remember should treat its freshly committed index job
/// (bd-index-auto-freshness-m5kwf).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RememberIndexPublishRoute {
    /// No active publisher and no peer pending jobs: publish the single
    /// document inline (the common singleton fast path).
    Inline,
    /// Another process holds the index publish lock: defer — that
    /// publisher's coalesced drain owns every pending job including ours.
    Defer,
    /// No active publisher, but peer jobs are pending. The burst's initial
    /// publisher has already finished, so with no elected drainer these
    /// jobs would sit pending until an unrelated rebuild — the liveness
    /// hole behind 30/30-durable-but-1/30-searchable. One writer must
    /// attempt bounded, non-blocking leadership of a coalesced drain.
    LeadCoalescedDrain,
}

fn remember_inline_index_publish_route(
    connection: &DbConnection,
    workspace_id: &str,
    index_job_id: &str,
) -> RememberIndexPublishRoute {
    let active_publish = match connection.is_lock_held(&AdvisoryLockId::index(workspace_id)) {
        Ok(lock) => lock.is_some(),
        Err(error) => {
            tracing::warn!(
                target: "ee::memory",
                workspace_id,
                index_job_id,
                error = %error,
                "deferring inline remember indexing because publish-lock posture is unavailable"
            );
            return RememberIndexPublishRoute::Defer;
        }
    };
    let pending_job_ids = match connection.list_pending_search_index_jobs(workspace_id, Some(2)) {
        Ok(jobs) => jobs.into_iter().map(|job| job.id).collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(
                target: "ee::memory",
                workspace_id,
                index_job_id,
                error = %error,
                "deferring inline remember indexing because pending-job posture is unavailable"
            );
            return RememberIndexPublishRoute::Defer;
        }
    };
    remember_index_publish_route(active_publish, &pending_job_ids, index_job_id)
}

fn remember_index_publish_route(
    active_publish: bool,
    pending_job_ids: &[String],
    index_job_id: &str,
) -> RememberIndexPublishRoute {
    if active_publish {
        return RememberIndexPublishRoute::Defer;
    }
    if pending_job_ids.iter().any(|job_id| job_id != index_job_id) {
        return RememberIndexPublishRoute::LeadCoalescedDrain;
    }
    RememberIndexPublishRoute::Inline
}

/// Election lock for the burst-drain leader. Distinct from
/// `AdvisoryLockId::index` on purpose: the coalesced drain acquires the
/// publish lock internally, so the leader must not pre-hold it.
fn remember_index_drain_leader_lock(workspace_id: &str) -> AdvisoryLockId {
    AdvisoryLockId::new("index_drain_leader", workspace_id)
}

/// Leadership TTL covers both bounded drain rounds (each capped by the
/// 300s index runtime budget) so a killed leader cannot wedge elections.
const REMEMBER_INDEX_DRAIN_LEADER_TTL_SECS: u64 = 660;
const REMEMBER_INDEX_DRAIN_MAX_ROUNDS: usize = 2;

/// Attempt bounded, non-blocking leadership of a coalesced drain for a
/// burst whose initial publisher already finished. Exactly one racing
/// writer wins the election lock; losers defer immediately (their jobs are
/// owned by the winner's drain). The winner runs at most
/// [`REMEMBER_INDEX_DRAIN_MAX_ROUNDS`] coalesced rounds — the second pass
/// catches stragglers enqueued during the first publish — and reports its
/// own job's real outcome from the drain reports.
fn remember_lead_coalesced_index_drain(
    connection: &DbConnection,
    workspace_id: &str,
    index_job_id: &str,
    index_dir: &Path,
) -> IndexProcessingJobReport {
    let lock_id = remember_index_drain_leader_lock(workspace_id);
    let holder_id = format!("remember-drain:{}:{index_job_id}", std::process::id());
    match connection.acquire_advisory_lock(
        &lock_id,
        &holder_id,
        Some(REMEMBER_INDEX_DRAIN_LEADER_TTL_SECS),
        Some("remember coalesced index drain leadership"),
    ) {
        Ok(crate::db::AcquireLockResult::Acquired(_))
        | Ok(crate::db::AcquireLockResult::Expired { .. }) => {}
        Ok(crate::db::AcquireLockResult::AlreadyHeld { .. }) => {
            // Election lost: the winner's drain owns this pending job.
            return remember_index_job_queued_for_coalescing(index_job_id);
        }
        Err(error) => {
            tracing::warn!(
                target: "ee::memory",
                workspace_id,
                index_job_id,
                error = %error,
                "deferring coalesced drain leadership because the election lock is unavailable"
            );
            return remember_index_job_queued_for_coalescing(index_job_id);
        }
    }
    let _leadership = RememberWorkspaceWriteLock {
        connection,
        lock_id,
        holder_id,
    };
    remember_drain_pending_rounds(
        index_job_id,
        REMEMBER_INDEX_DRAIN_MAX_ROUNDS,
        || process_pending_index_jobs_coalesced(connection, workspace_id, index_dir, None),
        || match connection.list_pending_search_index_jobs(workspace_id, Some(1)) {
            Ok(pending) => Some(!pending.is_empty()),
            Err(_) => None,
        },
        || remember_index_job_report_from_durable_state(connection, index_job_id),
    )
}

/// Pure round driver for the elected leader: drain, pick out our own
/// job's report, and run at most one bounded straggler pass when jobs
/// remain pending after a round. Never claims success for work that did
/// not happen: on drain failure with our job still pending we report the
/// queued posture (the next writer's election owns it) while the failed
/// attempt stays in the log, and when the rounds never listed our job the
/// caller-supplied resolver derives the report from durable truth instead
/// of assuming a concurrent rebuild completed it.
fn remember_drain_pending_rounds<D, P, A>(
    index_job_id: &str,
    max_rounds: usize,
    mut drain: D,
    mut pending_remaining: P,
    resolve_absent: A,
) -> IndexProcessingJobReport
where
    D: FnMut() -> Result<Vec<IndexProcessingJobReport>, IndexRebuildError>,
    P: FnMut() -> Option<bool>,
    A: FnOnce() -> IndexProcessingJobReport,
{
    let mut own_report: Option<IndexProcessingJobReport> = None;
    for _round in 0..max_rounds.max(1) {
        match drain() {
            Ok(reports) => {
                if own_report.is_none() {
                    own_report = reports
                        .into_iter()
                        .find(|report| report.job_id == index_job_id);
                }
            }
            Err(error) if remember_index_failure_is_deferable(&error) => {
                return own_report.unwrap_or_else(|| {
                    remember_index_job_queued_after_transient_failure(index_job_id, &error)
                });
            }
            Err(error) => {
                tracing::warn!(
                    target: "ee::memory",
                    index_job_id,
                    error = %error,
                    "coalesced drain leadership round failed; leaving remaining jobs for the next election"
                );
                return own_report
                    .unwrap_or_else(|| remember_index_job_queued_for_coalescing(index_job_id));
            }
        }
        match pending_remaining() {
            Some(true) => {}
            Some(false) | None => break,
        }
    }
    own_report.unwrap_or_else(resolve_absent)
}

/// Resolve this writer's report when the leader's own drain rounds never
/// listed its job. Absence is NOT proof of success — the job may have been
/// claimed by a concurrent drain, failed, cancelled, or left pending — so
/// the report derives from the durable job row: only an actually-Completed
/// row reports the indexed posture; everything else stays truthful.
fn remember_index_job_report_from_durable_state(
    connection: &DbConnection,
    index_job_id: &str,
) -> IndexProcessingJobReport {
    match connection.get_search_index_job(index_job_id) {
        Ok(Some(job)) if job.status == SearchIndexJobStatus::Completed.as_str() => {
            IndexProcessingJobReport {
                job_id: index_job_id.to_owned(),
                job_type: job.job_type,
                document_source: job.document_source,
                document_id: job.document_id,
                outcome: "completed".to_owned(),
                processing_mode: "drained_by_concurrent_coalesced_rebuild".to_owned(),
                documents_total: job.documents_total,
                documents_indexed: job.documents_indexed,
                error: None,
                fallback_to_full: None,
            }
        }
        Ok(Some(job)) if job.status == SearchIndexJobStatus::Failed.as_str() => {
            IndexProcessingJobReport {
                job_id: index_job_id.to_owned(),
                job_type: job.job_type,
                document_source: job.document_source,
                document_id: job.document_id,
                outcome: "failed".to_owned(),
                processing_mode: "concurrent_drain_reported_failure".to_owned(),
                documents_total: job.documents_total,
                documents_indexed: job.documents_indexed,
                error: job.error_message,
                fallback_to_full: None,
            }
        }
        // Pending, running, cancelled, missing, or unreadable: the publish
        // is unproven, so report the queued posture — a later election or
        // explicit rebuild owns the job. Never hard-code success.
        _ => remember_index_job_queued_for_coalescing(index_job_id),
    }
}

/// Post-publish tail sweep (bd-index-auto-freshness-m5kwf reachability):
/// when every concurrent writer deferred against this writer's publish
/// lock, this writer is the burst's LAST completed writer and nobody later
/// will observe the orphaned tail. The finishing publisher therefore makes
/// one bounded, non-blocking election attempt over any peer jobs that
/// landed while it published. Peers that land after this sweep saw a free
/// publish lock at their own route decision and elect for themselves.
fn remember_drain_peer_tail_after_publish(
    connection: &DbConnection,
    workspace_id: &str,
    index_dir: &Path,
) {
    let pending = match connection.list_pending_search_index_jobs(workspace_id, Some(1)) {
        Ok(pending) => pending,
        Err(error) => {
            tracing::warn!(
                target: "ee::memory",
                workspace_id,
                error = %error,
                "skipping post-publish tail sweep because pending-job posture is unavailable"
            );
            return;
        }
    };
    let Some(first) = pending.first() else {
        return;
    };
    let report =
        remember_lead_coalesced_index_drain(connection, workspace_id, &first.id, index_dir);
    tracing::debug!(
        target: "ee::memory",
        workspace_id,
        tail_job_id = first.id.as_str(),
        outcome = report.outcome.as_str(),
        processing_mode = report.processing_mode.as_str(),
        "post-publish tail sweep finished"
    );
}

fn remember_index_job_queued_for_coalescing(index_job_id: &str) -> IndexProcessingJobReport {
    IndexProcessingJobReport {
        job_id: index_job_id.to_owned(),
        job_type: SearchIndexJobType::SingleDocument.as_str().to_owned(),
        document_source: Some("memory".to_owned()),
        document_id: None,
        outcome: "skipped".to_owned(),
        processing_mode: "deferred_to_coalesced_contention_rebuild".to_owned(),
        documents_total: 1,
        documents_indexed: 0,
        error: None,
        fallback_to_full: None,
    }
}

fn remember_index_failure_is_deferable(error: &IndexRebuildError) -> bool {
    matches!(
        error,
        IndexRebuildError::Cancelled(reason)
            if matches!(
                reason.kind,
                asupersync::CancelKind::Deadline | asupersync::CancelKind::Timeout
            )
    )
}

fn remember_index_job_queued_after_transient_failure(
    index_job_id: &str,
    error: impl ToString,
) -> IndexProcessingJobReport {
    IndexProcessingJobReport {
        job_id: index_job_id.to_owned(),
        job_type: SearchIndexJobType::SingleDocument.as_str().to_owned(),
        document_source: Some("memory".to_owned()),
        document_id: None,
        outcome: "skipped".to_owned(),
        processing_mode: "single_document_as_full_rebuild".to_owned(),
        documents_total: 1,
        documents_indexed: 0,
        error: Some(format!(
            "search index publish deferred after a transient failure: {}",
            error.to_string()
        )),
        fallback_to_full: None,
    }
}

fn remember_search_index_error(error: impl ToString) -> DomainError {
    DomainError::SearchIndex {
        message: format!(
            "Remembered memory but failed to publish search index after contention retries: {}",
            error.to_string()
        ),
        repair: Some("ee index rebuild --workspace .".to_owned()),
    }
}

#[derive(Clone, Debug)]
struct PreparedRememberMemory {
    memory_id: MemoryId,
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
    index_dir: PathBuf,
    content: String,
    workflow_id: Option<String>,
    level: MemoryLevel,
    kind: MemoryKind,
    typed_fields_json: Option<String>,
    attempt_family: Option<crate::db::MemoryAttemptFamily>,
    confidence: f32,
    tags: Vec<String>,
    provenance_uri: Option<String>,
    policy_bypass: Option<RememberPolicyBypassReport>,
    valid_from: Option<String>,
    valid_to: Option<String>,
    validity_status: String,
    validity_window_kind: String,
}

#[derive(Clone, Debug)]
struct RememberStoreOverride {
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
    index_dir: PathBuf,
}

struct RememberFinishInput {
    prepared: PreparedRememberMemory,
    memory_id: String,
    audit_id: String,
    index_job_id: String,
    memory_input: CreateMemoryInput,
    policy_bypass: Option<RememberPolicyBypassReport>,
    near_duplicates: Vec<RememberNearDuplicate>,
    defer_index_processing: bool,
    auto_link: bool,
    propose_candidates: bool,
    write_replay_guard: RememberWriteReplayGuard,
}

pub(crate) struct PreparedRememberTxnWrite {
    finish: RememberFinishInput,
    typed_fields_json: Option<String>,
    embed_dedup_decision: RememberEmbedDedupDecision,
    embed_dedup_link_id: Option<String>,
    audit_details: String,
    index_input: CreateSearchIndexJobInput,
}

impl PreparedRememberTxnWrite {
    pub(crate) fn memory_id(&self) -> &str {
        &self.finish.memory_id
    }

    // Accessor kept for parity with memory_id()/index_dir(); not yet consumed.
    #[allow(dead_code)]
    pub(crate) fn workspace_id(&self) -> &str {
        &self.finish.prepared.workspace_id
    }

    pub(crate) fn index_dir(&self) -> &Path {
        &self.finish.prepared.index_dir
    }
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

// Thin convenience wrapper retained for callers that do not supply a store;
// currently unused (callers go through prepare_remember_memory_with_store).
#[allow(dead_code)]
fn prepare_remember_memory(
    options: &RememberMemoryOptions<'_>,
    memory_id: MemoryId,
) -> Result<PreparedRememberMemory, DomainError> {
    prepare_remember_memory_with_store(options, memory_id, None, &[], None)
}

fn prepare_remember_memory_with_store(
    options: &RememberMemoryOptions<'_>,
    memory_id: MemoryId,
    store_override: Option<&RememberStoreOverride>,
    typed_field_assignments: &[String],
    attempt_family: Option<&RememberAttemptFamily<'_>>,
) -> Result<PreparedRememberMemory, DomainError> {
    let caller_workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
    let default_database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| caller_workspace_path.join(".ee").join("ee.db"));
    let workspace_path = store_override
        .map(|store| store.workspace_path.clone())
        .unwrap_or_else(|| caller_workspace_path.clone());
    let database_path = store_override
        .map(|store| store.database_path.clone())
        .unwrap_or(default_database_path);
    let workspace_id = store_override
        .map(|store| store.workspace_id.clone())
        .unwrap_or_else(|| stable_workspace_id(&workspace_path));
    let index_dir = store_override
        .map(|store| store.index_dir.clone())
        .unwrap_or_else(|| workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR));
    let content = MemoryContent::parse(options.content)
        .map_err(|error| remember_usage_error(error.to_string()))?
        .as_str()
        .to_owned();
    let workflow_id = parse_workflow_id(options.workflow_id)?;
    if let Some(error) = remember_level_kind_cross_wire_error(options.level, options.kind) {
        return Err(error);
    }
    let level = MemoryLevel::from_str(options.level)
        .map_err(|error| remember_usage_error(error.to_string()))?;
    let kind = MemoryKind::from_str(options.kind)
        .map_err(|error| remember_usage_error(error.to_string()))?;
    let explicit_field_hint = typed_assignment_field_hint(&kind, typed_field_assignments);
    let explicit_unredacted =
        crate::models::memory::canonicalize_typed_memory_field_assignments_json_with_redactor(
            &kind,
            typed_field_assignments,
            str::to_owned,
        )
        .map_err(|error| {
            typed_field_validation_error(&kind, explicit_field_hint.as_deref(), error)
        })?;
    let policy_input = explicit_unredacted
        .as_ref()
        .map(|typed_fields| format!("{content}\n{typed_fields}"));
    let policy_bypass = validate_remember_policy(
        policy_input.as_deref().unwrap_or(&content),
        &caller_workspace_path,
        options.allow_secret_mention,
    )?;
    let extracted_typed_fields =
        crate::models::memory::extract_typed_memory_fields_json_with_redactor(
            &kind,
            &content,
            |value| crate::policy::redact_secret_like_content(value).content,
        )
        .map_err(|error| remember_usage_error(format!("typed field extraction failed: {error}")))?;
    let explicit_typed_fields =
        crate::models::memory::canonicalize_typed_memory_field_assignments_json_with_redactor(
            &kind,
            typed_field_assignments,
            |value| crate::policy::redact_secret_like_content(value).content,
        )
        .map_err(|error| {
            typed_field_validation_error(&kind, explicit_field_hint.as_deref(), error)
        })?;
    let typed_fields_json = crate::models::memory::merge_typed_memory_fields_json(
        &kind,
        extracted_typed_fields.as_deref(),
        explicit_typed_fields.as_deref(),
    )
    .map_err(|error| remember_usage_error(format!("typed field merge failed: {error}")))?;
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
    let attempt_family = attempt_family
        .map(validate_remember_attempt_family)
        .transpose()?;

    Ok(PreparedRememberMemory {
        memory_id,
        workspace_id,
        workspace_path,
        database_path,
        index_dir,
        content,
        workflow_id,
        level,
        kind,
        typed_fields_json,
        attempt_family,
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

/// Per-command cache for provenance file contents used by freshness checks.
#[derive(Debug, Default)]
pub struct EvidenceFreshnessFileCache {
    files: BTreeMap<PathBuf, Result<Option<String>, String>>,
}

impl EvidenceFreshnessFileCache {
    #[must_use]
    pub fn cached_file_count(&self) -> usize {
        self.files.len()
    }

    fn read_file_text(&mut self, source_path: &Path) -> Result<Option<String>, String> {
        if let Some(cached) = self.files.get(source_path) {
            return cached.clone();
        }
        let result = read_provenance_file_text(source_path);
        self.files.insert(source_path.to_path_buf(), result.clone());
        result
    }
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
    assess_memory_evidence_freshness_inner(memory, workspace_path, None)
}

/// Check evidence freshness while reusing file reads within one command.
#[must_use]
pub fn assess_memory_evidence_freshness_with_cache(
    memory: &StoredMemory,
    workspace_path: Option<&Path>,
    file_cache: &mut EvidenceFreshnessFileCache,
) -> EvidenceFreshness {
    assess_memory_evidence_freshness_inner(memory, workspace_path, Some(file_cache))
}

fn assess_memory_evidence_freshness_inner(
    memory: &StoredMemory,
    workspace_path: Option<&Path>,
    mut file_cache: Option<&mut EvidenceFreshnessFileCache>,
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
            let read_result = if let Some(cache) = file_cache.as_deref_mut() {
                cache.read_file_text(&source_path)
            } else {
                read_provenance_file_text(&source_path)
            };
            let source_text = match read_result {
                Ok(Some(contents)) => match span {
                    Some(_) => extract_line_span(&contents, *span).unwrap_or_default(),
                    None => contents,
                },
                Ok(None) => {
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
                Err(message) => {
                    return EvidenceFreshness {
                        status: EvidenceFreshnessStatus::UnreachableSource,
                        provenance_uri: Some(canonical_uri),
                        detail: message,
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
        | ProvenanceUri::AgentMail { .. }
        | ProvenanceUri::External { .. } => EvidenceFreshness {
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

const MAX_PROVENANCE_FILE_BYTES: u64 = 10 * 1024 * 1024;

fn read_provenance_file_text(source_path: &Path) -> Result<Option<String>, String> {
    if let Some(symlink_path) = first_existing_symlink_component(source_path).map_err(|error| {
        format!(
            "Referenced provenance file {} could not be inspected at {}: {}.",
            source_path.display(),
            error.path.display(),
            error.source
        )
    })? {
        return Err(format!(
            "Referenced provenance file {} traverses symlinked path component {}.",
            source_path.display(),
            symlink_path.display()
        ));
    }

    match fs::symlink_metadata(source_path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.len() > MAX_PROVENANCE_FILE_BYTES {
                return Err(format!(
                    "Referenced provenance file {} is too large ({} bytes). Maximum supported size is {} bytes.",
                    source_path.display(),
                    metadata.len(),
                    MAX_PROVENANCE_FILE_BYTES
                ));
            }
        }
        Ok(_) => {
            return Err(format!(
                "Referenced provenance file {} is not a regular file.",
                source_path.display()
            ));
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "Referenced provenance file {} could not be inspected: {error}.",
                source_path.display()
            ));
        }
    }

    // Bounded read with `take(CAP + 1)`. The metadata pre-check above is
    // TOCTOU-racy: a peer can grow the provenance file between the
    // `symlink_metadata().len()` check and the read, so the underlying
    // `fs::read_to_string` would still pre-size its destination `String`
    // from the (now-grown) metadata length on every supported platform
    // and OOM the evidence-freshness path before the body could be
    // inspected. `take(CAP + 1)` pins peak allocation to CAP + 1 bytes
    // regardless of on-disk file size; the post-read length check then
    // distinguishes "exactly at cap" (accepted) from "above cap"
    // (rejected as TOCTOU). Same defensive pattern as the Round-2 caps
    // at `src/core/handoff.rs::read_regular_file_no_symlinks`
    // (6d8d00e5), `src/core/preflight_guard.rs::read_preflight_rules_file_no_follow`
    // (7f56d89b), `src/core/claims.rs::read_claim_file_bytes_no_follow`,
    // and the workspace-side Agent Mail snapshot guard added by
    // ed0f69f8.
    //
    // After the read, re-check that no path component became a symlink
    // during the read window. The opening `first_existing_symlink_component`
    // scan above plus this trailing re-check together close the
    // symlink-swap TOCTOU window without requiring O_NOFOLLOW on the
    // file handle itself — same shape as
    // `handoff::read_regular_file_no_symlinks`.
    use std::io::Read as _;
    let mut bytes = Vec::new();
    let file = match fs::File::open(source_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Referenced provenance file {} could not be read: {error}.",
                source_path.display()
            ));
        }
    };
    let read_limit = MAX_PROVENANCE_FILE_BYTES.saturating_add(1);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "Referenced provenance file {} could not be read: {error}.",
                source_path.display()
            )
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PROVENANCE_FILE_BYTES {
        return Err(format!(
            "Referenced provenance file {} grew past the {} byte cap after the metadata check (TOCTOU).",
            source_path.display(),
            MAX_PROVENANCE_FILE_BYTES
        ));
    }
    // Symlink swap detection. If the opening scan saw no symlinks but
    // the path acquired a symlink component during the read, the bytes
    // we just hashed came from the swapped target rather than the
    // regular file the caller asked us to verify. Fail closed.
    if let Some(symlink_path) = first_existing_symlink_component(source_path).map_err(|error| {
        format!(
            "Referenced provenance file {} could not be inspected at {}: {}.",
            source_path.display(),
            error.path.display(),
            error.source
        )
    })? {
        return Err(format!(
            "Referenced provenance file {} acquired symlinked path component {} during read.",
            source_path.display(),
            symlink_path.display()
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        format!(
            "Referenced provenance file {} could not be read as UTF-8: {error}.",
            source_path.display()
        )
    })
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
    let Some((path, contents)) =
        read_workspace_config_if_present(workspace_path, "workspace config")?
    else {
        return Ok(SecretDetectorAllowConfig::default());
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

/// Maximum bytes inspected when reading `<workspace>/.ee/config.toml`.
/// Realistic workspace configs are kilobytes to low tens of KiB; 4 MiB
/// is a very generous ceiling. Same magnitude as `PREFLIGHT_RULES_MAX_BYTES`
/// (7f56d89b) and `PREFLIGHT_RUN_STORE_MAX_BYTES` (aac04adb) for the
/// parallel workspace-local `.ee/preflight_rules.toml` and
/// `.ee/preflight_runs.json` paths. The cap bounds the per-call
/// allocation on the `ee remember` hot path so a peer-planted oversized
/// config cannot OOM the CLI through repeated invocation.
const WORKSPACE_CONFIG_MAX_BYTES: u64 = 4 * 1024 * 1024;

fn read_workspace_config_if_present(
    workspace_path: &Path,
    purpose: &str,
) -> Result<Option<(PathBuf, String)>, DomainError> {
    use std::io::Read as _;

    let path = workspace_path.join(".ee").join("config.toml");
    ensure_workspace_config_path_is_not_symlink(&path, purpose)?;
    ensure_workspace_config_path_is_regular_file(&path, purpose)?;

    // Bound the read so a peer-planted multi-GB `.ee/config.toml`
    // (accidental — `cat /dev/urandom > .ee/config.toml` — or hostile
    // in a shared multi-agent checkout) cannot pin a matching
    // allocation on every `ee remember` invocation. Both
    // `load_secret_detector_allow_config` (line 1981) and
    // `remember_cluster_coherence_config` (line 3447) call this
    // helper on the `ee remember` hot path; without the cap, one bad
    // config silently disables every other agent's memory writes.
    // Same self-DoS amplification pattern that 7f56d89b
    // (`PREFLIGHT_RULES_MAX_BYTES`) and aac04adb
    // (`PREFLIGHT_RUN_STORE_MAX_BYTES`) just closed for the parallel
    // workspace-local `.ee/` files.
    //
    // Three layers of defense, matching the peer's
    // `read_preflight_rules_file_no_follow` shape:
    //  1. `metadata.len() > LIMIT` pre-check at stat time, before any
    //     open or allocation.
    //  2. No-follow open plus opened-metadata checks close the
    //     leaf-symlink and race-grown-file windows between stat and read.
    //  3. `file.take(LIMIT + 1).read_to_end` bounds allocation if the
    //     opened file grows while it is being read.
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.len() > WORKSPACE_CONFIG_MAX_BYTES => {
            return Err(DomainError::Configuration {
                message: format!(
                    "Refusing to read {purpose} {}: file is {} bytes, exceeding the {WORKSPACE_CONFIG_MAX_BYTES}-byte ceiling.",
                    path.display(),
                    metadata.len(),
                ),
                repair: Some(format!(
                    "Trim or remove {} so it is under {WORKSPACE_CONFIG_MAX_BYTES} bytes.",
                    path.display()
                )),
            });
        }
        Ok(_) => {}
        // Missing or stat-failed paths fall through to `File::open`,
        // which classifies NotFound as `Ok(None)` per the original
        // semantics. Other stat errors will resurface there too.
        Err(_) => {}
    }

    let file = match open_workspace_config_file_for_read_no_follow(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(DomainError::Configuration {
                message: format!("Failed to read {purpose} {}: {error}", path.display()),
                repair: Some("Fix or remove .ee/config.toml.".to_owned()),
            });
        }
    };
    let opened_metadata = file
        .metadata()
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to inspect opened {purpose} {}: {error}",
                path.display()
            ),
            repair: Some("Fix or remove .ee/config.toml.".to_owned()),
        })?;
    if !opened_metadata.file_type().is_file() {
        return Err(DomainError::Configuration {
            message: format!(
                "Refusing to read {purpose} {} because it is not a regular file after open.",
                path.display()
            ),
            repair: Some("Replace .ee/config.toml with a regular TOML file.".to_owned()),
        });
    }
    if opened_metadata.len() > WORKSPACE_CONFIG_MAX_BYTES {
        return Err(DomainError::Configuration {
            message: format!(
                "Refusing to read {purpose} {}: file grew past the {WORKSPACE_CONFIG_MAX_BYTES}-byte cap after open.",
                path.display()
            ),
            repair: Some(format!(
                "Trim or remove {} so it is under {WORKSPACE_CONFIG_MAX_BYTES} bytes.",
                path.display()
            )),
        });
    }
    let mut bytes = Vec::new();
    if let Err(error) = file
        .take(WORKSPACE_CONFIG_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        return Err(DomainError::Configuration {
            message: format!("Failed to read {purpose} {}: {error}", path.display()),
            repair: Some("Fix or remove .ee/config.toml.".to_owned()),
        });
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > WORKSPACE_CONFIG_MAX_BYTES {
        return Err(DomainError::Configuration {
            message: format!(
                "Refusing to read {purpose} {}: file grew past the {WORKSPACE_CONFIG_MAX_BYTES}-byte cap after the metadata check (TOCTOU)",
                path.display()
            ),
            repair: Some(format!(
                "Trim or remove {} so it is under {WORKSPACE_CONFIG_MAX_BYTES} bytes.",
                path.display()
            )),
        });
    }
    let contents = String::from_utf8(bytes).map_err(|error| DomainError::Configuration {
        message: format!(
            "Failed to read {purpose} {}: contents are not valid UTF-8: {error}",
            path.display()
        ),
        repair: Some("Fix or remove .ee/config.toml.".to_owned()),
    })?;
    Ok(Some((path, contents)))
}

fn open_workspace_config_file_for_read_no_follow(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_workspace_config_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_workspace_config_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_workspace_config_open_no_follow(_options: &mut fs::OpenOptions) {}

fn ensure_workspace_config_path_is_regular_file(
    path: &Path,
    purpose: &str,
) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(DomainError::Configuration {
            message: format!(
                "Refusing to read {purpose} {} because it is not a regular file.",
                path.display()
            ),
            repair: Some("Replace .ee/config.toml with a regular TOML file.".to_owned()),
        }),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(DomainError::Configuration {
            message: format!("Failed to inspect {purpose} {}: {error}", path.display()),
            repair: Some("Fix or remove .ee/config.toml.".to_owned()),
        }),
    }
}

fn ensure_workspace_config_path_is_not_symlink(
    path: &Path,
    purpose: &str,
) -> Result<(), DomainError> {
    if let Some(symlink_path) =
        first_existing_symlink_component(path).map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to inspect {purpose} path component {}: {}",
                error.path.display(),
                error.source
            ),
            repair: Some("Fix or remove .ee/config.toml.".to_owned()),
        })?
    {
        return Err(DomainError::Configuration {
            message: format!(
                "Refusing to read {purpose} {} through symlinked path component {}.",
                path.display(),
                symlink_path.display()
            ),
            repair: Some(
                "Replace .ee/config.toml with a regular file inside the workspace.".to_owned(),
            ),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct SymlinkComponentInspectionError {
    path: PathBuf,
    source: std::io::Error,
}

fn first_existing_symlink_component(
    path: &Path,
) -> Result<Option<PathBuf>, SymlinkComponentInspectionError> {
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
            Err(source) => {
                return Err(SymlinkComponentInspectionError {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(None)
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
            offset = end;
        } else {
            // Handle edge case where a match occurs across character boundaries
            // by advancing to the next valid character boundary.
            offset += 1;
            while offset < content.len() && !content.is_char_boundary(offset) {
                offset += 1;
            }
        }
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
        repair: Some(crate::core::storeless_workspace_repair(database_path)),
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

fn build_prepared_remember_txn_write(
    connection: &DbConnection,
    prepared: PreparedRememberMemory,
    id_source: &mut RememberIdSource<'_>,
    defer_index_processing: bool,
    auto_link: bool,
    propose_candidates: bool,
    write_replay_guard: RememberWriteReplayGuard,
) -> Result<PreparedRememberTxnWrite, DomainError> {
    let memory_id = prepared.memory_id.to_string();
    let audit_id = id_source.next_audit_id();
    let policy_bypass_audit_id = prepared
        .policy_bypass
        .as_ref()
        .map(|_| id_source.next_audit_id());
    let index_job_id = id_source.next_search_index_job_id();
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
        trust_class: if prepared.attempt_family.is_some()
            && super::memory_scope::current_agent_name().is_some()
        {
            // bd-multiplicity-aware-trust-p0u7g: an attempt-family write from
            // a registered agent identity (the same actor signal
            // remember_trust_subclass records) is an agent fan-out record and
            // enters at agent_assertion — the class the promotion gate holds
            // it at until every declared sibling slot is recorded. Human
            // --family writes keep ADR 0009's human_explicit posture; their
            // multiplicity still surfaces through reporting and ranking
            // discounts. The actor signal, never the flag alone, decides the
            // class.
            TrustClass::AgentAssertion.as_str().to_owned()
        } else {
            TrustClass::HumanExplicit.as_str().to_owned()
        },
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
    let index_input = CreateSearchIndexJobInput {
        workspace_id: prepared.workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(memory_id.clone()),
        documents_total: 1,
    };
    let embed_dedup_decision = remember_embed_dedup_decision_from_env(connection, &memory_input)?;
    let near_duplicates = remember_near_duplicates_from_embed_dedup_decision(&embed_dedup_decision);
    let embed_dedup_link_id = embed_dedup_decision
        .link
        .as_ref()
        .map(|_| generate_memory_link_id());
    let audit_details = remember_audit_details(
        &memory_id,
        &memory_input,
        policy_bypass.as_ref(),
        prepared.attempt_family.as_ref(),
    );
    let typed_fields_json = prepared.typed_fields_json.clone();

    Ok(PreparedRememberTxnWrite {
        finish: RememberFinishInput {
            prepared,
            memory_id,
            audit_id,
            index_job_id,
            memory_input,
            policy_bypass,
            near_duplicates,
            defer_index_processing,
            auto_link,
            propose_candidates,
            write_replay_guard,
        },
        typed_fields_json,
        embed_dedup_decision,
        embed_dedup_link_id,
        audit_details,
        index_input,
    })
}

pub(crate) fn prepare_remember_txn_write_for_connection(
    connection: &DbConnection,
    options: &RememberMemoryOptions<'_>,
    defer_index_processing: bool,
) -> Result<PreparedRememberTxnWrite, DomainError> {
    validate_remember_level_kind_cross_wire(options.level, options.kind)?;
    let mut id_source = RememberIdSource::Ambient;
    let prepared =
        prepare_remember_memory_with_store(options, id_source.next_memory_id(), None, &[], None)?;
    if options.dry_run {
        return Err(DomainError::Usage {
            message: "daemon remember batching cannot persist a dry-run request".to_owned(),
            repair: Some("submit dry-run remember requests through the direct CLI path".to_owned()),
        });
    }
    let write_replay_guard = RememberWriteReplayGuard::arm(&prepared.workspace_path)?;
    ensure_database_parent_exists(&prepared.database_path)?;
    migrate_remember_database_with_retry(connection)?;
    ensure_workspace(connection, &prepared.workspace_id, &prepared.workspace_path)?;
    // The daemon write-owner is already the per-process serializer for this
    // batch. We keep the direct CLI advisory lock on the direct path, but do
    // not acquire one per memory here; multiple same-workspace remembers in one
    // daemon batch would otherwise contend with their own first lock holder.
    build_prepared_remember_txn_write(
        connection,
        prepared,
        &mut id_source,
        defer_index_processing,
        options.auto_link,
        options.propose_candidates,
        write_replay_guard,
    )
}

pub(crate) fn record_prepared_remember_txn_write_in_txn(
    connection: &DbConnection,
    write: &PreparedRememberTxnWrite,
) -> crate::db::Result<()> {
    record_remembered_memory_in_txn(
        connection,
        &write.finish.memory_id,
        &write.finish.audit_id,
        &write.finish.index_job_id,
        &write.finish.memory_input,
        write.typed_fields_json.as_deref(),
        write.finish.prepared.attempt_family.as_ref(),
        &write.embed_dedup_decision,
        write.embed_dedup_link_id.as_deref(),
        &write.audit_details,
        &write.index_input,
        write.finish.policy_bypass.as_ref(),
        None,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "transaction-local primitive mirrors the storage/audit/index inputs"
)]
fn record_remembered_memory_in_txn(
    connection: &DbConnection,
    memory_id: &str,
    audit_id: &str,
    index_job_id: &str,
    memory_input: &CreateMemoryInput,
    typed_fields_json: Option<&str>,
    attempt_family: Option<&crate::db::MemoryAttemptFamily>,
    embed_dedup_decision: &RememberEmbedDedupDecision,
    embed_dedup_link_id: Option<&str>,
    audit_details: &str,
    index_input: &CreateSearchIndexJobInput,
    policy_bypass: Option<&RememberPolicyBypassReport>,
    audit_lane: Option<&AuditLaneHandle>,
) -> crate::db::Result<()> {
    match embed_dedup_decision.content_simhash {
        Some(content_simhash) => connection.insert_memory_with_content_simhash(
            memory_id,
            memory_input,
            content_simhash,
        )?,
        None => connection.insert_memory(memory_id, memory_input)?,
    }
    if let Some(typed_fields_json) = typed_fields_json {
        connection.set_memory_typed_fields_json(memory_id, Some(typed_fields_json))?;
    }
    if let Some(family) = attempt_family {
        connection.set_memory_attempt_family(memory_id, family)?;
    }
    if let (Some(link), Some(link_id)) = (embed_dedup_decision.link.as_ref(), embed_dedup_link_id) {
        connection.insert_memory_link(
            link_id,
            &CreateMemoryLinkInput {
                src_memory_id: memory_id.to_owned(),
                dst_memory_id: link.target_memory_id.clone(),
                relation: MemoryLinkRelation::Related,
                weight: 1.0,
                confidence: link.cosine_similarity.clamp(0.0, 1.0),
                directed: true,
                evidence_count: 2,
                last_reinforced_at: None,
                source: MemoryLinkSource::Auto,
                created_by: Some("ee remember".to_owned()),
                metadata_json: embed_dedup_decision.link_metadata_json(),
            },
        )?;
    }
    if audit_lane.is_none() {
        emit_remember_audit_events(
            connection,
            None,
            memory_id,
            audit_id,
            memory_input,
            audit_details,
            policy_bypass,
        )?;
    }
    connection.insert_search_index_job(index_job_id, index_input)
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
    typed_fields_json: Option<&str>,
    attempt_family: Option<&crate::db::MemoryAttemptFamily>,
    embed_dedup_decision: &RememberEmbedDedupDecision,
    embed_dedup_link_id: Option<&str>,
    audit_details: &str,
    index_input: &CreateSearchIndexJobInput,
    policy_bypass: Option<&RememberPolicyBypassReport>,
    audit_lane: Option<&AuditLaneHandle>,
) -> Result<(), DomainError> {
    for attempt in 0..REMEMBER_CONTENTION_MAX_ATTEMPTS {
        match connection.with_transaction(|| {
            record_remembered_memory_in_txn(
                connection,
                memory_id,
                audit_id,
                index_job_id,
                memory_input,
                typed_fields_json,
                attempt_family,
                embed_dedup_decision,
                embed_dedup_link_id,
                audit_details,
                index_input,
                policy_bypass,
                audit_lane,
            )
        }) {
            Ok(()) => {
                if let Some(audit_lane) = audit_lane {
                    emit_remember_audit_events(
                        connection,
                        Some(audit_lane),
                        memory_id,
                        audit_id,
                        memory_input,
                        audit_details,
                        policy_bypass,
                    )
                    .map_err(|error| DomainError::Storage {
                        message: format!("Failed to emit remember audit event: {error}"),
                        repair: Some("ee doctor".to_owned()),
                    })?;
                }
                return Ok(());
            }
            Err(error) if remember_write_contention_is_retryable(&error) => {
                if let Err(rollback_error) = connection.rollback() {
                    tracing::error!(
                        phase = "remember_retryable_write",
                        error = %error,
                        rollback_error = %rollback_error,
                        "failed to rollback transaction after write contention"
                    );
                }
                if memory_exists_after_commit_ambiguity(connection, memory_id)? {
                    return Ok(());
                }
                if attempt + 1 < REMEMBER_CONTENTION_MAX_ATTEMPTS {
                    remember_retry_sleep(remember_write_retry_delay(attempt), "store memory")?;
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

pub(crate) fn finish_prepared_remember_txn_write(
    connection: &DbConnection,
    write: PreparedRememberTxnWrite,
) -> Result<RememberMemoryReport, DomainError> {
    finish_remember_memory_after_primary_commit(connection, write.finish)
}

fn finish_remember_memory_after_primary_commit(
    connection: &DbConnection,
    finish: RememberFinishInput,
) -> Result<RememberMemoryReport, DomainError> {
    let RememberFinishInput {
        prepared,
        memory_id,
        audit_id,
        index_job_id,
        memory_input,
        policy_bypass,
        near_duplicates,
        defer_index_processing,
        auto_link,
        propose_candidates,
        mut write_replay_guard,
    } = finish;

    append_remember_audit_jsonl(&prepared, &audit_id, &memory_id, &memory_input)?;

    let (mut auto_links, mut auto_link_status, mut auto_link_degradations) =
        match create_auto_links_for_remember(
            connection,
            &prepared.workspace_id,
            &memory_id,
            prepared.workflow_id.as_deref(),
            auto_link,
        ) {
            Ok(auto_links) => {
                let status =
                    auto_link_status(prepared.workflow_id.as_deref(), auto_link, &auto_links);
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

    let (mut suggested_links, mut suggested_link_status, suggested_link_degradations) =
        match suggest_links_for_remember(
            connection,
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

    {
        let existing_auto_link_targets: BTreeSet<String> = auto_links
            .iter()
            .map(|link| link.target_memory_id.clone())
            .collect();
        match persist_high_confidence_cotag_links(
            connection,
            &prepared.workspace_id,
            &memory_id,
            auto_link,
            &existing_auto_link_targets,
            &suggested_links,
        ) {
            Ok(cotag_links) if !cotag_links.is_empty() => {
                let persisted: BTreeSet<String> = cotag_links
                    .iter()
                    .map(|link| link.target_memory_id.clone())
                    .collect();
                suggested_links.retain(|link| !persisted.contains(&link.target_memory_id));
                if suggested_links.is_empty() && suggested_link_status == "ready" {
                    suggested_link_status = "no_candidates".to_owned();
                }
                auto_links.extend(cotag_links);
                auto_link_degradations
                    .retain(|degradation| degradation.code != "auto_link_disabled");
                if auto_link_status == "no_workflow_required" || auto_link_status == "no_candidates"
                {
                    auto_link_status = "linked".to_owned();
                }
            }
            Ok(_) => {}
            Err(error) => {
                auto_link_degradations.push(RememberSuggestedLinkDegradation {
                    code: "remember_cotag_auto_link_failed".to_owned(),
                    severity: "low".to_owned(),
                    message: format!(
                        "Remembered the memory, but co-tag auto-linking failed: {}",
                        error.message()
                    ),
                    repair: "Run `ee doctor --json` and inspect memory link indexes.".to_owned(),
                });
            }
        }
    }

    let index_dir = prepared.index_dir.clone();
    let index_report = if defer_index_processing {
        IndexProcessingJobReport {
            job_id: index_job_id.clone(),
            job_type: SearchIndexJobType::SingleDocument.as_str().to_owned(),
            document_source: Some("memory".to_owned()),
            document_id: None,
            outcome: "skipped".to_owned(),
            processing_mode: "deferred_to_coalesced_batch_rebuild".to_owned(),
            documents_total: 1,
            documents_indexed: 0,
            error: None,
            fallback_to_full: None,
        }
    } else {
        match remember_inline_index_publish_route(connection, &prepared.workspace_id, &index_job_id)
        {
            RememberIndexPublishRoute::Defer => {
                remember_index_job_queued_for_coalescing(&index_job_id)
            }
            RememberIndexPublishRoute::LeadCoalescedDrain => remember_lead_coalesced_index_drain(
                connection,
                &prepared.workspace_id,
                &index_job_id,
                &index_dir,
            ),
            RememberIndexPublishRoute::Inline => {
                let report =
                    process_remember_index_job_with_retry(connection, &index_job_id, &index_dir)?;
                remember_drain_peer_tail_after_publish(
                    connection,
                    &prepared.workspace_id,
                    &index_dir,
                );
                report
            }
        }
    };
    let index_status = remember_index_status(&index_report);

    let (curation_candidate, curation_candidate_status, curation_candidate_degradations) =
        match propose_curation_candidate_for_remember(
            connection,
            &prepared,
            &memory_id,
            &memory_input,
            propose_candidates,
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

    let typed_fields =
        remember_typed_fields_value(&prepared.kind, prepared.typed_fields_json.as_deref())?;
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
        typed_fields,
        attempt_family: prepared.attempt_family,
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
        near_duplicates,
    })
}

fn remember_embed_dedup_decision_from_env(
    connection: &DbConnection,
    memory_input: &CreateMemoryInput,
) -> Result<RememberEmbedDedupDecision, DomainError> {
    let probe = remember_embed_dedup_probe_from_env(memory_input)?;
    remember_embed_dedup_decision_from_probe(connection, memory_input, &probe)
}

fn remember_embed_dedup_probe_from_env(
    memory_input: &CreateMemoryInput,
) -> Result<RememberEmbedDedupProbe, DomainError> {
    let config = EmbedDedupConfig::from_env().map_err(|error| DomainError::Configuration {
        message: error.to_string(),
        repair: Some(error.repair.to_owned()),
    })?;
    Ok(remember_embed_dedup_probe(memory_input, config))
}

#[cfg(test)]
fn remember_embed_dedup_decision(
    connection: &DbConnection,
    memory_input: &CreateMemoryInput,
    config: EmbedDedupConfig,
) -> Result<RememberEmbedDedupDecision, DomainError> {
    let probe = remember_embed_dedup_probe(memory_input, config);
    remember_embed_dedup_decision_from_probe(connection, memory_input, &probe)
}

fn remember_embed_dedup_probe(
    memory_input: &CreateMemoryInput,
    config: EmbedDedupConfig,
) -> RememberEmbedDedupProbe {
    if !config.enabled {
        return RememberEmbedDedupProbe::Disabled;
    }

    let query_fingerprint = crate::search::simhash::simhash_128(&memory_input.content);
    RememberEmbedDedupProbe::Enabled {
        hamming_k: config.hamming_k,
        cosine_floor: config.cosine_floor as f32,
        query_fingerprint,
        content_simhash: query_fingerprint.to_be_bytes(),
        query_embedding: HashEmbedder::default_256().embed_sync(&memory_input.content),
    }
}

fn remember_embed_dedup_decision_from_probe(
    connection: &DbConnection,
    memory_input: &CreateMemoryInput,
    probe: &RememberEmbedDedupProbe,
) -> Result<RememberEmbedDedupDecision, DomainError> {
    let RememberEmbedDedupProbe::Enabled {
        hamming_k,
        cosine_floor,
        query_fingerprint,
        content_simhash,
        query_embedding,
    } = probe
    else {
        return Ok(RememberEmbedDedupDecision::disabled());
    };

    let started = Instant::now();
    let candidates = connection
        .list_memory_simhash_candidates(
            &memory_input.workspace_id,
            *content_simhash,
            *hamming_k,
            REMEMBER_EMBED_DEDUP_CANDIDATE_LIMIT,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query embed-dedup SimHash candidates: {error}"),
            repair: Some("Run `ee doctor --json` and inspect the memories table.".to_owned()),
        })?;
    if candidates.is_empty() {
        trace_remember_embed_dedup_decision(
            &memory_input.workspace_id,
            None,
            None,
            None,
            "new_embed",
            "no_prior_workspace_simhash_candidate",
            started.elapsed(),
        );
        return Ok(RememberEmbedDedupDecision::fresh(
            *content_simhash,
            "no_prior_workspace_simhash_candidate",
        ));
    }

    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id.as_str())
        .collect::<Vec<_>>();
    let candidate_memories = connection
        .get_memories_batch(&candidate_ids)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load embed-dedup candidate memories: {error}"),
            repair: Some("Run `ee doctor --json` and inspect memory rows.".to_owned()),
        })?;

    let embedder = HashEmbedder::default_256();
    let candidate_embeddings = candidates
        .iter()
        .filter_map(|candidate| {
            let memory = candidate_memories.get(&candidate.memory_id)?;
            Some((
                candidate.memory_id.clone(),
                SimHash128::from_be_bytes(candidate.content_simhash),
                candidate.hamming_distance,
                embedder.embed_sync(&memory.content),
            ))
        })
        .collect::<Vec<_>>();
    if candidate_embeddings.is_empty() {
        trace_remember_embed_dedup_decision(
            &memory_input.workspace_id,
            None,
            None,
            None,
            "new_embed",
            "simhash_candidate_rows_missing",
            started.elapsed(),
        );
        return Ok(RememberEmbedDedupDecision::fresh(
            *content_simhash,
            "simhash_candidate_rows_missing",
        ));
    }

    let confirmed = first_confirmed_simhash_candidate(
        *query_fingerprint,
        query_embedding,
        candidate_embeddings
            .iter()
            .map(|(memory_id, fingerprint, _, embedding)| {
                (memory_id.as_str(), *fingerprint, embedding.as_slice())
            }),
        *hamming_k,
        *cosine_floor,
    );

    match confirmed {
        Some(candidate) => {
            trace_remember_embed_dedup_decision(
                &memory_input.workspace_id,
                Some(candidate.candidate_id),
                Some(candidate.hamming_distance),
                Some(candidate.cosine.similarity),
                "reuse",
                "simhash_within_threshold_and_cosine_confirmed",
                started.elapsed(),
            );
            Ok(RememberEmbedDedupDecision::reused(
                *content_simhash,
                RememberEmbedDedupLink {
                    target_memory_id: candidate.candidate_id.to_owned(),
                    hamming_distance: candidate.hamming_distance,
                    cosine_similarity: candidate.cosine.similarity,
                    cosine_floor: *cosine_floor,
                },
            ))
        }
        None => {
            let nearest = candidate_embeddings
                .iter()
                .min_by(|left, right| left.2.cmp(&right.2).then_with(|| left.0.cmp(&right.0)));
            trace_remember_embed_dedup_decision(
                &memory_input.workspace_id,
                nearest.map(|candidate| candidate.0.as_str()),
                nearest.map(|candidate| candidate.2),
                None,
                "new_embed",
                "cosine_under_floor",
                started.elapsed(),
            );
            Ok(RememberEmbedDedupDecision::fresh(
                *content_simhash,
                "cosine_under_floor",
            ))
        }
    }
}

fn trace_remember_embed_dedup_decision(
    workspace_id: &str,
    candidate_memory_id: Option<&str>,
    hamming_distance: Option<u32>,
    cosine_similarity: Option<f32>,
    decision: &'static str,
    reason: &'static str,
    elapsed: Duration,
) {
    tracing::info!(
        workspace_id,
        request_id = "ee_remember",
        bead_id = "bd-1iltv",
        surface = "embed_dedup",
        phase = "decision",
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        candidate_memory_id,
        hamming_distance,
        cosine_similarity,
        decision,
        reason,
        "remember embed-dedup decision"
    );
}

fn emit_remember_audit_events(
    connection: &DbConnection,
    audit_lane: Option<&AuditLaneHandle>,
    memory_id: &str,
    audit_id: &str,
    memory_input: &CreateMemoryInput,
    audit_details: &str,
    policy_bypass: Option<&RememberPolicyBypassReport>,
) -> crate::db::Result<()> {
    let memory_audit = CreateAuditInput {
        workspace_id: Some(memory_input.workspace_id.clone()),
        actor: Some("ee remember".to_owned()),
        action: audit_actions::MEMORY_CREATE.to_owned(),
        target_type: Some("memory".to_owned()),
        target_id: Some(memory_id.to_owned()),
        details: Some(audit_details.to_owned()),
    };
    emit_with_direct_fallback(
        audit_lane,
        AuditLaneEvent::from_audit_input(audit_id, 1, &memory_audit),
        |event| insert_audit_event(connection, event),
    )?;
    if let Some(policy_bypass) = policy_bypass {
        if let Some(policy_audit_id) = policy_bypass.audit_id.as_deref() {
            let policy_audit = CreateAuditInput {
                workspace_id: Some(memory_input.workspace_id.clone()),
                actor: Some("ee remember".to_owned()),
                action: audit_actions::POLICY_BYPASS.to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(memory_id.to_owned()),
                details: Some(policy_bypass_audit_details(policy_bypass)),
            };
            emit_with_direct_fallback(
                audit_lane,
                AuditLaneEvent::from_audit_input(policy_audit_id, 2, &policy_audit),
                |event| insert_audit_event(connection, event),
            )?;
        }
    }
    Ok(())
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
        || message.contains("could not acquire database write lock")
        || message.contains("index publish lock contention")
        || message.contains("resource temporarily unavailable")
}

fn remember_write_retry_delay(attempt: usize) -> Duration {
    let capped = attempt.min(6) as u64;
    Duration::from_millis(10 * (1 << capped))
}

fn remember_retry_sleep(delay: Duration, phase: &'static str) -> Result<(), DomainError> {
    crate::db::sleep_retry_delay_or_cancel(DbOperation::Execute, delay).map_err(|error| {
        DomainError::Storage {
            message: format!("Remember retry cancelled while waiting to {phase}: {error}"),
            repair: Some(
                "Retry after storage contention clears or with a larger runtime budget.".to_owned(),
            ),
        }
    })
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

fn generate_search_index_job_id_seeded(determinism: &mut Deterministic<Seed>) -> String {
    let memory_id = MemoryId::now_seeded(determinism).to_string();
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
    attempt_family: Option<&crate::db::MemoryAttemptFamily>,
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
        "attemptFamily": attempt_family.map(|family| serde_json::json!({
            "familyAlias": crate::models::public_attempt_family_alias(&family.family_id),
            "declaredSize": family.declared_size,
            "attemptIndex": family.attempt_index,
            "disposition": family.disposition,
        })),
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

// bd-pp1fk: high-confidence co-tag auto-linking. Plain `ee remember` (no
// workflow) historically produced zero links, leaving the entire graph layer
// (PageRank/HITS/bridges/proximity/Pack DNA/skyline) dormant by default. We now
// persist a bounded, deterministic set of the strongest co-tag neighbors as
// audited `related` links so the graph wakes up from ordinary tagged remembers.
// See docs/adr/0051-remember-cotag-auto-linking.md.
//
// MIN_SCORE 0.75 maps (via `co_tag_score`) to "at least half of the new
// memory's tags overlap the neighbor", so a single incidental shared tag never
// triggers a durable link. LIMIT caps fan-out; WEIGHT sits just below the
// workflow-recency weight because co-tag is a weaker structural signal.
const REMEMBER_AUTO_COTAG_LINK_LIMIT: usize = 3;
const REMEMBER_AUTO_COTAG_LINK_MIN_SCORE: f32 = 0.75;
const REMEMBER_AUTO_COTAG_LINK_WEIGHT: f32 = 0.4;

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

/// bd-pp1fk: persist the strongest co-tag neighbors as durable, audited
/// `related` links. Returns the links it created; callers drop the persisted
/// targets from the advisory `suggested_links` set so a memory is never both
/// auto-linked and re-suggested.
///
/// Deterministic: `suggested` arrives already ordered by co-tag score then
/// ULID payload, so the bounded prefix we persist is stable for a given DB +
/// input. Each write is wrapped in a transaction with a `memory_link.create`
/// audit entry — this honors the "no silent memory mutation" principle: the
/// links are automatic but never silent.
fn persist_high_confidence_cotag_links(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    enabled: bool,
    existing_auto_link_targets: &BTreeSet<String>,
    suggested: &[RememberSuggestedLink],
) -> Result<Vec<RememberAutoLink>, DomainError> {
    if !enabled {
        return Ok(Vec::new());
    }

    let mut created = Vec::new();
    for link in suggested {
        if created.len() >= REMEMBER_AUTO_COTAG_LINK_LIMIT {
            break;
        }
        if link.score < REMEMBER_AUTO_COTAG_LINK_MIN_SCORE
            || existing_auto_link_targets.contains(&link.target_memory_id)
        {
            continue;
        }

        // `suggest_links_for_remember` already excludes existing links, but
        // re-check here so a workflow-recency link created earlier in the same
        // remember (or a concurrent writer) is never duplicated.
        let exists = connection
            .memory_link_exists_between(memory_id, &link.target_memory_id)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to query existing memory links for co-tag linking: {error}"
                ),
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
            dst_memory_id: link.target_memory_id.clone(),
            relation: MemoryLinkRelation::Related,
            weight: REMEMBER_AUTO_COTAG_LINK_WEIGHT,
            confidence: link.confidence,
            directed: false,
            evidence_count: link.evidence_count,
            last_reinforced_at: Some(reinforced_at),
            source: MemoryLinkSource::Auto,
            created_by: Some("ee remember".to_owned()),
            metadata_json: Some(
                serde_json::json!({
                    "schema": "ee.memory_link.cotag_auto.v1",
                    "linkKind": "cotag",
                    "reason": "high_confidence_cotag_overlap",
                    "matchedTags": link.matched_tags,
                    "cotagScore": link.score,
                })
                .to_string(),
            ),
        };
        let audit_details = serde_json::json!({
            "schema": "ee.audit.memory_link_auto_create.v1",
            "command": "ee remember",
            "linkId": &link_id,
            "srcMemoryId": memory_id,
            "dstMemoryId": &link.target_memory_id,
            "relation": input.relation.as_str(),
            "source": input.source.as_str(),
            "weight": input.weight,
            "linkKind": "cotag",
            "matchedTags": &link.matched_tags,
            "cotagScore": link.score,
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
                message: format!("Failed to create co-tag auto-link: {error}"),
                repair: Some("ee doctor".to_owned()),
            })?;

        created.push(RememberAutoLink {
            link_id,
            target_memory_id: link.target_memory_id.clone(),
            relation: input.relation.as_str().to_owned(),
            weight: input.weight,
            source: input.source.as_str().to_owned(),
            audit_id,
        });
    }

    Ok(created)
}

fn auto_link_status(
    workflow_id: Option<&str>,
    enabled: bool,
    auto_links: &[RememberAutoLink],
) -> &'static str {
    if !enabled {
        "disabled"
    } else if !auto_links.is_empty() {
        // bd-pp1fk: any durable link (workflow-recency OR high-confidence
        // co-tag) means we linked. Checked before the workflow branch so a
        // workflow-less remember that produced co-tag links reports "linked",
        // not the misleading "no_workflow_required".
        "linked"
    } else if workflow_id.is_none() {
        // G7 (bd-17c65.7.6): honest-unimplemented. Without a workflow
        // context we cannot meaningfully auto-link. The status name
        // explicitly says "required" so an agent reading it knows this
        // is NOT a failure — it's an expected state outside a workflow.
        // The caller emits an `auto_link_disabled` info-severity
        // degraded entry pointing at the explicit `ee memory link`
        // surface as the recovery path.
        "no_workflow_required"
    } else {
        // In a workflow but no links survived (all candidates already linked,
        // or none cleared the co-tag threshold).
        "no_candidates"
    }
}

const REMEMBER_CURATION_NEIGHBOR_LIMIT: usize = 10;
const REMEMBER_CURATION_CLUSTER_THRESHOLD: usize =
    crate::curate::cluster_coherence::DEFAULT_MIN_CLUSTER_SIZE;
#[cfg(not(test))]
const REMEMBER_CURATION_SYNC_BUDGET_MS: u128 = 50;
#[cfg(test)]
const REMEMBER_CURATION_TEST_SYNC_BUDGET_MS: u128 = 60_000;

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
    sort_and_dedup_memory_ids_by_ulid_payload(&mut member_memory_ids);

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
                    target_memory_id: Some(target_memory_id.clone()),
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
                    derivation_source_refs_json: None,
                    derivation_metadata_json: None,
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
        dedup_mode: crate::core::search::SearchDedupMode::DocId,
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
    sort_by_ulid_payload_or_lexical(&mut ranked, |(memory_id, _)| memory_id.as_str());
    ranked.sort_by(|(_, left_count), (_, right_count)| right_count.cmp(left_count));
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
    let candidate_ids: Vec<String> = std::iter::once(memory_id.to_owned())
        .chain(member_ids)
        .filter(|id| seen.insert(id.clone()))
        .collect();

    let batch_ids = candidate_ids.iter().map(|s| s.as_str()).collect::<Vec<_>>();
    let batch_result =
        connection
            .get_memories_batch(&batch_ids)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to load curation neighbor memory: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;

    for candidate_id in candidate_ids {
        let Some(memory) = batch_result.get(&candidate_id).cloned() else {
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
    sort_by_ulid_payload_or_lexical(&mut cluster, |memory| memory.id.as_str());
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
            repair: Some("ee memory tags --help".to_owned()),
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
    let threshold =
        match read_workspace_config_if_present(workspace_path, "workspace learn config")? {
            Some((config_path, contents)) => {
                let config =
                    ConfigFile::parse(&contents).map_err(|error| DomainError::Configuration {
                        message: format!(
                            "Failed to parse workspace learn config {}: {error}",
                            config_path.display()
                        ),
                        repair: Some("Fix [learn] in .ee/config.toml.".to_owned()),
                    })?;
                config.learn.cluster_coherence_threshold.unwrap_or(
                    crate::curate::cluster_coherence::DEFAULT_CLUSTER_COHERENCE_THRESHOLD,
                )
            }
            None => crate::curate::cluster_coherence::DEFAULT_CLUSTER_COHERENCE_THRESHOLD,
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
    sort_by_ulid_payload_or_lexical(&mut sorted, |point| point.memory_id.as_str());
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

fn sort_and_dedup_memory_ids_by_ulid_payload(memory_ids: &mut Vec<String>) {
    sort_by_ulid_payload_or_lexical(memory_ids, |memory_id| memory_id.as_str());
    memory_ids.dedup();
}

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
    let mut ordered = cluster.iter().collect::<Vec<_>>();
    sort_by_ulid_payload_or_lexical(&mut ordered, |memory| memory.id.as_str());
    let exemplar = ordered
        .first()
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

    sort_by_ulid_payload_or_lexical(&mut candidates, |(memory_id, _)| memory_id.as_str());
    candidates.sort_by_key(|(_, tags)| Reverse(tags.len()));

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

fn typed_assignment_field_hint(kind: &MemoryKind, assignments: &[String]) -> Option<String> {
    let assignment = assignments.first()?.trim();
    let separator = assignment
        .char_indices()
        .find_map(|(index, character)| matches!(character, '=' | '~' | '^').then_some(index))
        .unwrap_or(assignment.len());
    let raw_field = assignment[..separator].trim();
    if raw_field.is_empty() {
        return None;
    }
    let field = crate::models::memory::normalize_typed_memory_field_name(raw_field).ok()?;
    crate::models::memory::typed_memory_field_names(kind)
        .contains(&field)
        .then_some(field)
}

fn typed_field_validation_error(
    kind: &MemoryKind,
    field_hint: Option<&str>,
    error: MemoryValidationError,
) -> DomainError {
    let message = error.to_string();
    let mut valid_fields = crate::models::memory::typed_memory_field_names(kind);
    let (code, field, reason) = match &error {
        MemoryValidationError::TypedFieldsUnsupportedKind { .. } => (
            TYPED_FIELD_UNKNOWN_CODE,
            field_hint.unwrap_or("fields").to_owned(),
            message.clone(),
        ),
        MemoryValidationError::TypedFieldNotAllowed {
            field,
            valid_fields: error_valid_fields,
            ..
        } => {
            valid_fields.clone_from(error_valid_fields);
            (
                TYPED_FIELD_UNKNOWN_CODE,
                field.clone(),
                "field is not declared for the selected memory kind".to_owned(),
            )
        }
        MemoryValidationError::TypedFieldWrongType { field, expected } => (
            TYPED_FIELD_INVALID_CODE,
            field.clone(),
            format!("expected {expected}"),
        ),
        MemoryValidationError::TypedFieldInvalid { field, reason } => {
            (TYPED_FIELD_INVALID_CODE, field.clone(), reason.clone())
        }
        MemoryValidationError::TypedFieldTooLong {
            field,
            bytes,
            limit,
        } => (
            TYPED_FIELD_INVALID_CODE,
            field.clone(),
            format!("value is {bytes} UTF-8 bytes; limit is {limit}"),
        ),
        MemoryValidationError::TypedFieldListTooLong {
            field,
            count,
            limit,
        } => (
            TYPED_FIELD_INVALID_CODE,
            field.clone(),
            format!("list has {count} items; limit is {limit}"),
        ),
        MemoryValidationError::TypedFieldsJsonTooLarge { bytes, limit } => (
            TYPED_FIELD_INVALID_CODE,
            "fields".to_owned(),
            format!("sidecar JSON is {bytes} UTF-8 bytes; limit is {limit}"),
        ),
        MemoryValidationError::InvalidTypedFieldsJson { message } => (
            TYPED_FIELD_INVALID_CODE,
            field_hint.unwrap_or("fields").to_owned(),
            message.clone(),
        ),
        MemoryValidationError::TypedFieldsTooMany { count, limit } => (
            TYPED_FIELD_INVALID_CODE,
            "fields".to_owned(),
            format!("sidecar has {count} populated fields; limit is {limit}"),
        ),
        MemoryValidationError::TypedFieldsKindMismatch { expected, actual } => (
            TYPED_FIELD_INVALID_CODE,
            "kind".to_owned(),
            format!("expected kind `{expected}`, got `{actual}`"),
        ),
        _ => (
            TYPED_FIELD_INVALID_CODE,
            field_hint.unwrap_or("fields").to_owned(),
            message.clone(),
        ),
    };
    let repair = if code == TYPED_FIELD_UNKNOWN_CODE {
        "Choose a field from error.details.validFields for this kind. Inspect the registry with `ee schema export ee.memory.typed_fields.v2 --json`."
    } else {
        "Correct the named field using error.details.reason and validFields. Inspect the registry with `ee schema export ee.memory.typed_fields.v2 --json`."
    };
    DomainError::UsageCodeWithDetails {
        code,
        message,
        repair: Some(repair.to_owned()),
        details_json: serde_json::json!({
            "failureModeCode": code,
            "schema": crate::models::memory::TYPED_MEMORY_FIELDS_SCHEMA_V2,
            "kind": kind.as_str(),
            "field": field,
            "reason": reason,
            "validFields": valid_fields,
        })
        .to_string(),
    }
}

// =============================================================================
// Remember ergonomic upgrades (bd-1pi9m.4)
//
// 1. `ee remember --batch --stdin`: JSONL batch input with per-line
//    INDEPENDENT validation + persistence — one poisoned line cannot drop
//    the rest of a harness flush (mirrors the journal batch surface,
//    ADR 0062 §4).
// 2. `ee remember --reinforce`: when the top near-duplicate neighbor's
//    cosine similarity is at or above `[curation] duplicate_similarity`
//    (default 0.92), strengthen the existing memory (evidence span +
//    bounded Bayesian confidence bump + `memory.reinforce` audit row)
//    instead of inserting a new row. Below threshold falls through to the
//    normal create path.
// 3. Idempotency keys: replaying the same key + canonical request hash
//    (content plus explicit typed fields, when present) returns the original
//    memory id with `status=already_recorded` (mirrors the `ee outcome
//    --event-id` idempotency pattern).
// =============================================================================

/// Max JSONL lines accepted by `ee remember --batch --stdin` per invocation
/// (mirrors the journal `--stdin` bound, ADR 0062 §4).
pub const REMEMBER_BATCH_MAX_LINES: usize = 512;

/// Default `[curation] duplicate_similarity` threshold used by
/// `ee remember --reinforce` when the workspace config does not override it.
pub const REMEMBER_DEFAULT_DUPLICATE_SIMILARITY: f32 = 0.92;

/// Audit details schema for `memory.reinforce` rows.
pub const REMEMBER_REINFORCE_AUDIT_SCHEMA_V1: &str = "ee.audit.memory_reinforce.v1";

/// Per-line error code for an idempotency key replayed with a different request.
pub const REMEMBER_IDEMPOTENCY_CONFLICT_CODE: &str = "remember_idempotency_conflict";

/// A typed-field name is not declared for the selected memory kind.
pub const TYPED_FIELD_UNKNOWN_CODE: &str = "typed_field_unknown";

/// A declared typed-field assignment has an invalid name, value, or shape.
pub const TYPED_FIELD_INVALID_CODE: &str = "typed_field_invalid";

/// A known memory LEVEL token was passed as the memory kind.
pub const REMEMBER_KIND_IS_LEVEL_CODE: &str = "remember_kind_is_level";

/// A canonical memory KIND token was passed as the memory level.
pub const REMEMBER_LEVEL_IS_KIND_CODE: &str = "remember_level_is_kind";

/// Maximum caller-controlled bytes echoed by a cross-wire usage error.
const REMEMBER_CROSS_WIRE_ECHO_MAX_BYTES: usize = 128;

fn validate_remember_level_kind_cross_wire(level: &str, kind: &str) -> Result<(), DomainError> {
    remember_level_kind_cross_wire_error(level, kind).map_or(Ok(()), Err)
}

fn bounded_remember_cross_wire_echo(raw: &str) -> (String, bool) {
    if raw.len() <= REMEMBER_CROSS_WIRE_ECHO_MAX_BYTES {
        return (raw.to_owned(), false);
    }

    const OMISSION: &str = "…";
    let remaining = REMEMBER_CROSS_WIRE_ECHO_MAX_BYTES - OMISSION.len();
    let mut prefix_end = remaining / 2;
    while prefix_end > 0 && !raw.is_char_boundary(prefix_end) {
        prefix_end -= 1;
    }
    let mut suffix_start = raw.len() - (remaining - prefix_end);
    while suffix_start < raw.len() && !raw.is_char_boundary(suffix_start) {
        suffix_start += 1;
    }

    (
        format!("{}{}{}", &raw[..prefix_end], OMISSION, &raw[suffix_start..]),
        true,
    )
}

/// Detect level/kind cross-wiring on the raw `--level` / `--kind` tokens
/// (bd-remember-level-kind-validation-zau2l).
///
/// Matching is exact on the normalized token: the four level names are
/// reserved as kinds and the nine canonical kind names are rejected as
/// levels, each with did-you-mean guidance toward the sibling flag. Custom
/// kinds that merely share a prefix with a level name (for example
/// `episodic-note`) stay accepted and continue through the existing
/// [`MemoryKind`] canonicalization contract.
#[must_use]
pub fn remember_level_kind_cross_wire_error(level: &str, kind: &str) -> Option<DomainError> {
    if let Ok(level_token) = MemoryLevel::from_str(kind) {
        let level_value = level_token.as_str();
        let (provided, provided_truncated) = bounded_remember_cross_wire_echo(kind);
        return Some(DomainError::UsageCodeWithDetails {
            code: REMEMBER_KIND_IS_LEVEL_CODE,
            message: format!(
                "`{provided}` is a memory level, not a kind — did you mean `--level {level_value}`? \
                 Canonical kinds: {}. Free-form kinds stay accepted; only the four level names \
                 (working, episodic, semantic, procedural) are reserved.",
                KNOWN_MEMORY_KINDS.join(", ")
            ),
            repair: Some(format!(
                "ee remember \"<content>\" --level {level_value} --kind <kind> --json"
            )),
            details_json: serde_json::json!({
                "failureModeCode": REMEMBER_KIND_IS_LEVEL_CODE,
                "argument": "--kind",
                "provided": provided,
                "providedTruncated": provided_truncated,
                "didYouMean": {"argument": "--level", "value": level_value},
                "memoryLevels": KNOWN_MEMORY_LEVELS,
                "canonicalKinds": KNOWN_MEMORY_KINDS,
                "recovery": [{
                    "priority": 1,
                    "kind": "flag",
                    "rationale": "Move the recognized level token to --level and choose the intended memory kind.",
                    "riskClass": "mutating_local_repair",
                    "requiresHumanApproval": false,
                    "mutatesExternalState": false,
                    "mutatesTrackerState": false,
                    "privacyClass": "bounded_command_no_raw_state",
                    "flagName": "--level",
                    "valueHint": level_value,
                    "example": format!("ee remember \"<content>\" --level {level_value} --kind <kind> --json"),
                    "resultsIn": "The request is validated with separate level and kind taxonomies."
                }],
            })
            .to_string(),
        });
    }
    if MemoryLevel::from_str(level).is_err()
        && let Ok(kind_token) = MemoryKind::from_str(level)
        && !matches!(kind_token, MemoryKind::Custom(_))
    {
        let kind_value = kind_token.as_str().to_owned();
        let (provided, provided_truncated) = bounded_remember_cross_wire_echo(level);
        return Some(DomainError::UsageCodeWithDetails {
            code: REMEMBER_LEVEL_IS_KIND_CODE,
            message: format!(
                "`{provided}` is a memory kind, not a level — did you mean `--kind {kind_value}`? \
                 Levels are: working, episodic, semantic, procedural."
            ),
            repair: Some(format!(
                "ee remember \"<content>\" --level <level> --kind {kind_value} --json"
            )),
            details_json: serde_json::json!({
                "failureModeCode": REMEMBER_LEVEL_IS_KIND_CODE,
                "argument": "--level",
                "provided": provided,
                "providedTruncated": provided_truncated,
                "didYouMean": {"argument": "--kind", "value": kind_value},
                "memoryLevels": KNOWN_MEMORY_LEVELS,
                "canonicalKinds": KNOWN_MEMORY_KINDS,
                "recovery": [{
                    "priority": 1,
                    "kind": "flag",
                    "rationale": "Move the recognized kind token to --kind and choose the intended memory level.",
                    "riskClass": "mutating_local_repair",
                    "requiresHumanApproval": false,
                    "mutatesExternalState": false,
                    "mutatesTrackerState": false,
                    "privacyClass": "bounded_command_no_raw_state",
                    "flagName": "--kind",
                    "valueHint": kind_value,
                    "example": format!("ee remember \"<content>\" --level <level> --kind {kind_value} --json"),
                    "resultsIn": "The request is validated with separate level and kind taxonomies."
                }],
            })
            .to_string(),
        });
    }
    None
}

/// Metadata schema for evidence spans attached by `ee remember --reinforce`.
const REMEMBER_REINFORCE_EVIDENCE_SCHEMA_V1: &str = "ee.remember.reinforce_evidence.v1";
const REMEMBER_REINFORCE_CANDIDATE_LIMIT: usize = 16;
/// Most recent live memories scanned for reinforce neighbor discovery.
/// Bounds in-process SimHash fingerprinting on large workspaces.
const REMEMBER_REINFORCE_SCAN_LIMIT: usize = 256;
/// SimHash candidate gate for reinforce neighbor discovery. Wider than the
/// embed-dedup gate (12) because the reinforce cosine threshold (0.92
/// default) admits softer near-duplicates than dedup's 0.97 floor.
const REMEMBER_REINFORCE_HAMMING_K: u32 = 32;
/// Synthetic per-workspace session that owns `ee remember --reinforce`
/// evidence spans (`evidence_spans.session_id` is NOT NULL).
const REMEMBER_REINFORCE_SESSION_KEY: &str = "ee-remember-reinforce";
const REMEMBER_IDEMPOTENCY_KEY_MAX_BYTES: usize = 128;

/// Write-control toggles layered over [`RememberMemoryOptions`] (bd-1pi9m.4).
#[derive(Clone, Copy, Debug, Default)]
pub struct RememberWriteControls<'a> {
    /// Strengthen the top near-duplicate neighbor instead of creating a new
    /// row when its similarity clears the configured threshold.
    pub reinforce: bool,
    /// Optional idempotency key for replay-safe writes.
    pub idempotency_key: Option<&'a str>,
    /// bd-2efx1: leave this write's search-index job pending instead of
    /// publishing it synchronously. The batch lane sets this and drains
    /// every pending job with ONE coalesced rebuild after the last line —
    /// without it each line pays a full index rebuild (O(n²) ingest).
    pub defer_index_processing: bool,
}

/// Attempt-family multiplicity declaration supplied at write time
/// (bd-multiplicity-aware-trust-p0u7g): the stable family identity this
/// finding was selected from and, optionally, the declared number of sibling
/// attempts the family was drawn from.
#[derive(Clone, Copy, Debug)]
pub struct RememberAttemptFamily<'a> {
    /// Stable pre-registered family identity (1..=64 bytes, `[A-Za-z0-9._:-]`).
    pub family_id: &'a str,
    /// Declared sibling attempt count (`--of-n`), 1..=1_000_000.
    pub declared_size: Option<u32>,
    /// Unique 1-based attempt slot this write occupies (`--attempt`). Family
    /// completion is measured in distinct slots, so a member without a slot
    /// never advances completion.
    pub attempt_index: Option<u32>,
    /// Member role for the slot (`--attempt-outcome`): `selected` winner or
    /// `rejected` sibling. Required exactly when a slot is declared.
    pub disposition: Option<&'a str>,
}

/// Validate a write-time attempt-family declaration into its canonical
/// persisted form.
fn validate_remember_attempt_family(
    family: &RememberAttemptFamily<'_>,
) -> Result<crate::db::MemoryAttemptFamily, DomainError> {
    let trimmed = family.family_id.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(remember_usage_error(
            "--family must be 1..=64 bytes after trimming".to_owned(),
        ));
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        let family_alias = crate::models::public_attempt_family_alias(trimmed);
        return Err(remember_usage_error(format!(
            "--family `{family_alias}` may only contain ASCII letters, digits, `.`, `_`, `:`, and `-`"
        )));
    }
    if let Some(size) = family.declared_size
        && !(1..=1_000_000).contains(&size)
    {
        return Err(remember_usage_error(format!(
            "--of-n must be in 1..=1000000, got {size}"
        )));
    }
    if let Some(index) = family.attempt_index
        && !(1..=1_000_000).contains(&index)
    {
        return Err(remember_usage_error(format!(
            "--attempt must be in 1..=1000000, got {index}"
        )));
    }
    if let (Some(index), Some(declared)) = (family.attempt_index, family.declared_size)
        && index > declared
    {
        return Err(remember_usage_error(format!(
            "--attempt {index} is outside the declared family size --of-n {declared}"
        )));
    }
    if family.attempt_index.is_some() != family.disposition.is_some() {
        return Err(remember_usage_error(
            "--attempt and --attempt-outcome must be provided together".to_owned(),
        ));
    }
    if let Some(disposition) = family.disposition
        && !matches!(disposition, "selected" | "rejected")
    {
        return Err(remember_usage_error(format!(
            "--attempt-outcome must be `selected` or `rejected`, got `{disposition}`"
        )));
    }
    Ok(crate::db::MemoryAttemptFamily {
        family_id: trimmed.to_owned(),
        declared_size: family.declared_size,
        attempt_index: family.attempt_index,
        disposition: family.disposition.map(str::to_owned),
    })
}

/// Outcome of one controlled remember write (bd-1pi9m.4).
#[derive(Clone, Debug, PartialEq)]
pub enum RememberOutcome {
    /// A new memory row was created (or previewed under `--dry-run`).
    Created(Box<RememberMemoryReport>),
    /// The idempotency key + content hash matched a prior write; the
    /// original memory id is returned and nothing is written.
    AlreadyRecorded(RememberAlreadyRecordedReport),
    /// The top near-duplicate neighbor absorbed this write.
    Reinforced(RememberReinforceReport),
}

/// Result of an idempotent `ee remember` replay (bd-1pi9m.4): the key and
/// content hash matched a prior write, so no new row was created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RememberAlreadyRecordedReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Canonical workspace ID.
    pub workspace_id: String,
    /// Resolved database path.
    pub database_path: PathBuf,
    /// Memory id recorded by the original write.
    pub memory_id: String,
    /// Idempotency key that matched.
    pub idempotency_key: String,
    /// Whether the replay ran under `--dry-run`.
    pub dry_run: bool,
}

impl RememberAlreadyRecordedReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "remember",
            "version": self.version,
            "mode": "idempotent_replay",
            "status": "already_recorded",
            "memoryId": &self.memory_id,
            "workspaceId": &self.workspace_id,
            "databasePath": self.database_path.display().to_string(),
            "idempotencyKey": &self.idempotency_key,
            "reinforced": false,
            "persisted": false,
            "dryRun": self.dry_run,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "Already recorded: {} (idempotency key `{}`)\n  No new memory row was written.\n",
            self.memory_id, self.idempotency_key
        )
    }
}

/// Result of a remember-time reinforcement (bd-1pi9m.4): the top
/// near-duplicate neighbor absorbed this write instead of a new row.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberReinforceReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Canonical workspace ID.
    pub workspace_id: String,
    /// Canonical workspace path.
    pub workspace_path: PathBuf,
    /// Resolved database path.
    pub database_path: PathBuf,
    /// Surviving (reinforced) memory id.
    pub memory_id: String,
    /// Cosine similarity between the new content and the surviving memory.
    pub similarity: f32,
    /// Threshold the similarity was measured against.
    pub threshold: f32,
    /// Always `true`; mirrors the per-line batch field.
    pub reinforced: bool,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether the reinforcement transaction was committed.
    pub persisted: bool,
    /// Memory confidence before the bounded bump.
    pub confidence_before: f32,
    /// Memory confidence after the bounded bump (monotonic, <= 1.0).
    pub confidence_after: f32,
    /// Evidence span attached to the surviving memory.
    pub evidence_span_id: Option<String>,
    /// `memory.reinforce` audit row id.
    pub audit_id: Option<String>,
    /// Provenance URIs folded into the surviving memory's evidence.
    pub source_uris: Vec<String>,
    /// Reinforcement timestamp stamped on the memory row's `updated_at`
    /// and recorded in the audit details (the memories table carries no
    /// dedicated `last_reinforced_at` column).
    pub last_reinforced_at: Option<String>,
}

impl RememberReinforceReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "remember",
            "version": self.version,
            "mode": "reinforce",
            "status": if self.dry_run { "would_reinforce" } else { "reinforced" },
            "reinforced": self.reinforced,
            "dryRun": self.dry_run,
            "persisted": self.persisted,
            "memoryId": &self.memory_id,
            "workspaceId": &self.workspace_id,
            "databasePath": self.database_path.display().to_string(),
            "similarity": self.similarity,
            "threshold": self.threshold,
            "confidenceBefore": self.confidence_before,
            "confidenceAfter": self.confidence_after,
            "evidenceSpanId": &self.evidence_span_id,
            "auditId": &self.audit_id,
            "sourceUris": &self.source_uris,
            "lastReinforcedAt": &self.last_reinforced_at,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = if self.dry_run {
            format!(
                "DRY RUN: Would reinforce {} (similarity {:.4} >= threshold {:.4})\n",
                self.memory_id, self.similarity, self.threshold
            )
        } else {
            format!(
                "Reinforced {} (similarity {:.4} >= threshold {:.4})\n",
                self.memory_id, self.similarity, self.threshold
            )
        };
        output.push_str(&format!(
            "  Confidence: {:.4} -> {:.4}\n",
            self.confidence_before, self.confidence_after
        ));
        if let Some(evidence_span_id) = &self.evidence_span_id {
            output.push_str(&format!("  Evidence span: {evidence_span_id}\n"));
        }
        if let Some(audit_id) = &self.audit_id {
            output.push_str(&format!("  Audit: {audit_id}\n"));
        }
        output
    }
}

/// `>=` so a similarity exactly at the configured threshold reinforces.
#[must_use]
pub fn remember_reinforce_should_apply(similarity: f32, threshold: f32) -> bool {
    similarity >= threshold
}

fn remember_content_hash(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

fn remember_request_hash(
    options: &RememberMemoryOptions<'_>,
    typed_field_assignments: &[String],
    attempt_family: Option<&RememberAttemptFamily<'_>>,
) -> Result<String, DomainError> {
    let content = MemoryContent::parse(options.content)
        .map_err(|error| remember_usage_error(error.to_string()))?
        .as_str()
        .to_owned();
    let canonical_family = attempt_family
        .map(validate_remember_attempt_family)
        .transpose()?;
    if typed_field_assignments.is_empty() && canonical_family.is_none() {
        return Ok(remember_content_hash(&content));
    }
    if typed_field_assignments.is_empty() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ee.remember.request.v1\0");
        hasher.update(&(content.len() as u64).to_le_bytes());
        hasher.update(content.as_bytes());
        if let Some(family) = &canonical_family {
            hasher.update(b"\0attempt_family\0");
            hasher.update(family.family_id.as_bytes());
            hasher.update(&u64::from(family.declared_size.unwrap_or(0)).to_le_bytes());
            hasher.update(&u64::from(family.attempt_index.unwrap_or(0)).to_le_bytes());
            hasher.update(family.disposition.as_deref().unwrap_or("").as_bytes());
        }
        return Ok(format!("blake3:{}", hasher.finalize().to_hex()));
    }
    let kind = MemoryKind::from_str(options.kind)
        .map_err(|error| remember_usage_error(error.to_string()))?;
    let field_hint = typed_assignment_field_hint(&kind, typed_field_assignments);
    let typed_fields =
        crate::models::memory::canonicalize_typed_memory_field_assignments_json_with_redactor(
            &kind,
            typed_field_assignments,
            str::to_owned,
        )
        .map_err(|error| typed_field_validation_error(&kind, field_hint.as_deref(), error))?
        .ok_or_else(|| remember_usage_error("typed field assignments were empty".to_owned()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.remember.request.v1\0");
    hasher.update(&(content.len() as u64).to_le_bytes());
    hasher.update(content.as_bytes());
    hasher.update(&(typed_fields.len() as u64).to_le_bytes());
    hasher.update(typed_fields.as_bytes());
    if let Some(family) = &canonical_family {
        hasher.update(b"\0attempt_family\0");
        hasher.update(family.family_id.as_bytes());
        hasher.update(&u64::from(family.declared_size.unwrap_or(0)).to_le_bytes());
        hasher.update(&u64::from(family.attempt_index.unwrap_or(0)).to_le_bytes());
        hasher.update(family.disposition.as_deref().unwrap_or("").as_bytes());
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn remember_typed_fields_value(
    kind: &MemoryKind,
    typed_fields_json: Option<&str>,
) -> Result<Option<serde_json::Value>, DomainError> {
    typed_fields_json
        .map(|raw| {
            crate::models::memory::typed_memory_fields_from_json(kind, raw)
                .map(|fields| serde_json::json!(fields))
                .map_err(|error| {
                    remember_usage_error(format!("invalid canonical typed fields: {error}"))
                })
        })
        .transpose()
}

fn remember_duplicate_similarity_threshold(workspace_path: &Path) -> f32 {
    crate::config::workspace_config(workspace_path)
        .and_then(|config| config.curation.duplicate_similarity)
        .map_or(REMEMBER_DEFAULT_DUPLICATE_SIMILARITY, |value| value as f32)
}

fn validate_remember_idempotency_key(raw: &str) -> Result<String, DomainError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(remember_usage_error(
            "idempotency key cannot be empty".to_owned(),
        ));
    }
    if trimmed.len() > REMEMBER_IDEMPOTENCY_KEY_MAX_BYTES {
        return Err(remember_usage_error(format!(
            "idempotency key exceeds the {REMEMBER_IDEMPOTENCY_KEY_MAX_BYTES}-byte cap ({} bytes)",
            trimmed.len()
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(remember_usage_error(
            "idempotency key must not contain control characters".to_owned(),
        ));
    }
    Ok(trimmed.to_owned())
}

fn remember_idempotency_conflict_error(idempotency_key: &str) -> DomainError {
    DomainError::UsageCodeWithDetails {
        code: REMEMBER_IDEMPOTENCY_CONFLICT_CODE,
        message: format!(
            "idempotency key already exists with different content or typed fields: {idempotency_key}"
        ),
        repair: Some(
            "Replay the original content and typed fields for this key, or supply a new --idempotency-key."
                .to_owned(),
        ),
        details_json: serde_json::json!({ "idempotencyKey": idempotency_key }).to_string(),
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RememberReinforceNeighbor {
    memory_id: String,
    similarity: f32,
    hamming_distance: u32,
}

/// Find the top near-duplicate neighbor for `content`. SimHash fingerprints
/// computed in-process over the most recent live memories gate the search
/// (the stored `content_simhash` column is only populated when embed-dedup
/// is enabled, so it cannot be relied on here); cosine similarity ranks
/// the gated candidates. Deterministic: ties on similarity break on
/// (hamming distance, memory id) ascending.
fn remember_reinforce_top_neighbor(
    connection: &DbConnection,
    workspace_id: &str,
    content: &str,
) -> Result<Option<RememberReinforceNeighbor>, DomainError> {
    let memories = connection
        .list_memories(workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list reinforce candidate memories: {error}"),
            repair: Some("Run `ee doctor --json` and inspect the memories table.".to_owned()),
        })?;
    if memories.is_empty() {
        return Ok(None);
    }

    // `list_memories` orders by id ascending; mem_ ids carry ULID payloads,
    // so the tail of the list is the most recent window.
    let window_start = memories.len().saturating_sub(REMEMBER_REINFORCE_SCAN_LIMIT);
    let query_fingerprint = crate::search::simhash::simhash_128(content);
    let mut gated: Vec<(u32, &StoredMemory)> = memories[window_start..]
        .iter()
        .filter_map(|memory| {
            let fingerprint = crate::search::simhash::simhash_128(&memory.content);
            let distance = crate::search::simhash::hamming_distance(query_fingerprint, fingerprint);
            (distance <= REMEMBER_REINFORCE_HAMMING_K).then_some((distance, memory))
        })
        .collect();
    gated.sort_by(|(left_distance, left), (right_distance, right)| {
        left_distance
            .cmp(right_distance)
            .then_with(|| left.id.cmp(&right.id))
    });
    gated.truncate(REMEMBER_REINFORCE_CANDIDATE_LIMIT);

    let embedder = HashEmbedder::default_256();
    let query_embedding = embedder.embed_sync(content);
    let mut top: Option<RememberReinforceNeighbor> = None;
    for (hamming_distance, memory) in gated {
        let candidate_embedding = embedder.embed_sync(&memory.content);
        let Some(similarity) = cosine_similarity(&query_embedding, &candidate_embedding) else {
            continue;
        };
        let better = match &top {
            None => true,
            Some(current) => match similarity.partial_cmp(&current.similarity) {
                Some(Ordering::Greater) => true,
                Some(Ordering::Equal) => {
                    (hamming_distance, memory.id.as_str())
                        < (current.hamming_distance, current.memory_id.as_str())
                }
                _ => false,
            },
        };
        if better {
            top = Some(RememberReinforceNeighbor {
                memory_id: memory.id.clone(),
                similarity,
                hamming_distance,
            });
        }
    }
    Ok(top)
}

fn generate_remember_reinforce_session_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("sess_{payload}")
}

fn generate_remember_evidence_span_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("ev_{payload}")
}

struct RememberReinforceContext<'a> {
    workspace_id: &'a str,
    workspace_path: &'a Path,
    database_path: &'a Path,
    target_memory_id: &'a str,
    similarity: f32,
    threshold: f32,
    canonical_content: &'a str,
    content_hash: &'a str,
    source: Option<&'a str>,
    idempotency_key: Option<&'a str>,
    dry_run: bool,
}

fn remember_reinforce_storage_error(
    error: impl std::fmt::Display,
    target_memory_id: &str,
) -> DomainError {
    DomainError::Storage {
        message: format!("Failed to reinforce memory {target_memory_id}: {error}"),
        repair: Some("ee doctor --json".to_owned()),
    }
}

/// Apply (or preview, under `--dry-run`) one reinforcement: attach the new
/// source as an evidence span on the surviving memory, apply a bounded
/// helpful-equivalent Bayesian confidence bump, and write the
/// `memory.reinforce` audit row — all in one transaction.
fn apply_remember_reinforce(
    connection: &DbConnection,
    context: &RememberReinforceContext<'_>,
) -> Result<RememberReinforceReport, DomainError> {
    let existing = connection
        .get_memory(context.target_memory_id)
        .map_err(|error| remember_reinforce_storage_error(error, context.target_memory_id))?
        .ok_or_else(|| DomainError::Storage {
            message: format!(
                "Reinforce target memory {} disappeared before the write",
                context.target_memory_id
            ),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let confidence_before = existing.confidence;
    let prior = connection
        .get_memory_bayes_posterior(context.target_memory_id)
        .map_err(|error| remember_reinforce_storage_error(error, context.target_memory_id))?
        .and_then(|(alpha, beta)| BetaPosterior::new(alpha, beta))
        .unwrap_or_else(BetaPosterior::jeffreys);
    // Helpful-equivalent weight: the same `alpha += 1` update the feedback
    // path applies for an `ee outcome --signal helpful` event.
    let posterior = prior.update_helpful();
    // Bounded, monotonic non-decreasing confidence bump: add the posterior
    // mean gain from one helpful-equivalent observation, never exceed 1.0,
    // never decrease.
    let mean_gain = (posterior.mean() - prior.mean()).max(0.0) as f32;
    let confidence_after = (confidence_before + mean_gain).clamp(confidence_before, 1.0_f32);
    let source_uris = context
        .source
        .map(|source| {
            ProvenanceUri::from_str(source)
                .map(|uri| uri.to_string())
                .map_err(|error| remember_usage_error(format!("invalid provenance URI: {error}")))
        })
        .transpose()?
        .into_iter()
        .collect::<Vec<_>>();

    if context.dry_run {
        return Ok(RememberReinforceReport {
            version: env!("CARGO_PKG_VERSION"),
            workspace_id: context.workspace_id.to_owned(),
            workspace_path: context.workspace_path.to_path_buf(),
            database_path: context.database_path.to_path_buf(),
            memory_id: context.target_memory_id.to_owned(),
            similarity: context.similarity,
            threshold: context.threshold,
            reinforced: true,
            dry_run: true,
            persisted: false,
            confidence_before,
            confidence_after,
            evidence_span_id: None,
            audit_id: None,
            source_uris,
            last_reinforced_at: None,
        });
    }

    let reinforced_at = Utc::now().to_rfc3339();
    let audit_id = generate_audit_id();
    let evidence_span_id = generate_remember_evidence_span_id();
    let existing_session = connection
        .get_session_by_cass_id(context.workspace_id, REMEMBER_REINFORCE_SESSION_KEY)
        .map_err(|error| remember_reinforce_storage_error(error, context.target_memory_id))?;
    let (session_id, session_exists) = match existing_session {
        Some(session) => (session.id, true),
        None => (generate_remember_reinforce_session_id(), false),
    };
    let evidence_metadata = serde_json::json!({
        "schema": REMEMBER_REINFORCE_EVIDENCE_SCHEMA_V1,
        "command": "ee remember --reinforce",
        "targetMemoryId": context.target_memory_id,
        "similarity": context.similarity,
        "threshold": context.threshold,
        "sourceUris": &source_uris,
        "reinforcedAt": &reinforced_at,
    })
    .to_string();
    let evidence_input = CreateEvidenceSpanInput {
        workspace_id: context.workspace_id.to_owned(),
        session_id: session_id.clone(),
        memory_id: Some(context.target_memory_id.to_owned()),
        producer_kind: EvidenceProducerKind::RememberReinforcement,
        cass_span_id: format!("reinforce:{evidence_span_id}"),
        span_kind: "summary".to_owned(),
        start_line: 1,
        end_line: 1,
        start_byte: None,
        end_byte: None,
        role: Some("reinforcement".to_owned()),
        excerpt: context.canonical_content.to_owned(),
        content_hash: context.content_hash.to_owned(),
        metadata_json: Some(evidence_metadata),
        inherited_redaction_classes: Vec::new(),
    };
    let audit_details = serde_json::json!({
        "schema": REMEMBER_REINFORCE_AUDIT_SCHEMA_V1,
        "command": "ee remember --reinforce",
        "memoryId": context.target_memory_id,
        "similarity": context.similarity,
        "threshold": context.threshold,
        "sourceUris": &source_uris,
        "contentHash": context.content_hash,
        "evidenceSpanId": &evidence_span_id,
        "priorConfidence": confidence_before,
        "newConfidence": confidence_after,
        "priorAlpha": prior.alpha(),
        "priorBeta": prior.beta(),
        "posteriorAlpha": posterior.alpha(),
        "posteriorBeta": posterior.beta(),
        "reinforcedAt": &reinforced_at,
        "idempotencyKey": context.idempotency_key,
    })
    .to_string();
    let audit_input = CreateAuditInput {
        workspace_id: Some(context.workspace_id.to_owned()),
        actor: Some("ee remember".to_owned()),
        action: audit_actions::MEMORY_REINFORCE.to_owned(),
        target_type: Some("memory".to_owned()),
        target_id: Some(context.target_memory_id.to_owned()),
        details: Some(audit_details),
    };

    connection
        .with_transaction(|| {
            if !session_exists {
                connection.insert_session(
                    &session_id,
                    &CreateSessionInput {
                        workspace_id: context.workspace_id.to_owned(),
                        cass_session_id: REMEMBER_REINFORCE_SESSION_KEY.to_owned(),
                        source_path: None,
                        agent_name: None,
                        model: None,
                        started_at: None,
                        ended_at: None,
                        message_count: 0,
                        token_count: None,
                        content_hash: remember_content_hash(REMEMBER_REINFORCE_SESSION_KEY),
                        metadata_json: None,
                    },
                )?;
            }
            connection.insert_evidence_span(&evidence_span_id, &evidence_input)?;
            let posterior_updated = connection.update_memory_bayes_posterior(
                context.target_memory_id,
                posterior.alpha(),
                posterior.beta(),
            )?;
            let reinforcement_applied = connection.apply_memory_reinforcement(
                context.target_memory_id,
                context.workspace_id,
                confidence_after,
                &reinforced_at,
            )?;
            if !posterior_updated || !reinforcement_applied {
                return Err(crate::db::DbError::MalformedRow {
                    operation: DbOperation::Execute,
                    message: format!(
                        "reinforce target memory {} vanished or was tombstoned mid-transaction",
                        context.target_memory_id
                    ),
                });
            }
            connection.insert_audit(&audit_id, &audit_input)?;
            if let Some(key) = context.idempotency_key {
                connection.insert_remember_idempotency_key(&CreateRememberIdempotencyKeyInput {
                    workspace_id: context.workspace_id.to_owned(),
                    idempotency_key: key.to_owned(),
                    content_hash: context.content_hash.to_owned(),
                    memory_id: context.target_memory_id.to_owned(),
                })?;
            }
            Ok(())
        })
        .map_err(|error| remember_reinforce_storage_error(error, context.target_memory_id))?;

    Ok(RememberReinforceReport {
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: context.workspace_id.to_owned(),
        workspace_path: context.workspace_path.to_path_buf(),
        database_path: context.database_path.to_path_buf(),
        memory_id: context.target_memory_id.to_owned(),
        similarity: context.similarity,
        threshold: context.threshold,
        reinforced: true,
        dry_run: false,
        persisted: true,
        confidence_before,
        confidence_after,
        evidence_span_id: Some(evidence_span_id),
        audit_id: Some(audit_id),
        source_uris,
        last_reinforced_at: Some(reinforced_at),
    })
}

fn record_remember_idempotency_key(
    report: &RememberMemoryReport,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<(), DomainError> {
    let connection = open_remember_database_with_retry(&report.database_path)?;
    connection
        .insert_remember_idempotency_key(&CreateRememberIdempotencyKeyInput {
            workspace_id: report.workspace_id.clone(),
            idempotency_key: idempotency_key.to_owned(),
            content_hash: request_hash.to_owned(),
            memory_id: report.memory_id.to_string(),
        })
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Memory {} was stored, but recording idempotency key `{idempotency_key}` failed: {error}",
                report.memory_id
            ),
            repair: Some("ee doctor --json".to_owned()),
        })
        .map(|_| ())
}

/// `remember_memory` layered with the bd-1pi9m.4 write controls:
/// idempotent replay detection and near-duplicate reinforcement. With
/// default controls this is exactly the plain create path.
pub fn remember_memory_with_controls(
    options: &RememberMemoryOptions<'_>,
    controls: &RememberWriteControls<'_>,
) -> Result<RememberOutcome, DomainError> {
    remember_memory_with_controls_and_typed_fields(options, controls, &[])
}

/// Controlled remember write with explicit registry-backed typed fields.
pub fn remember_memory_with_controls_and_typed_fields(
    options: &RememberMemoryOptions<'_>,
    controls: &RememberWriteControls<'_>,
    typed_field_assignments: &[String],
) -> Result<RememberOutcome, DomainError> {
    remember_memory_with_controls_typed_fields_and_family(
        options,
        controls,
        typed_field_assignments,
        None,
    )
}

/// Controlled remember write with typed fields and an optional attempt-family
/// multiplicity declaration (bd-multiplicity-aware-trust-p0u7g).
pub fn remember_memory_with_controls_typed_fields_and_family(
    options: &RememberMemoryOptions<'_>,
    controls: &RememberWriteControls<'_>,
    typed_field_assignments: &[String],
    attempt_family: Option<&RememberAttemptFamily<'_>>,
) -> Result<RememberOutcome, DomainError> {
    validate_remember_level_kind_cross_wire(options.level, options.kind)?;
    if controls.reinforce && !typed_field_assignments.is_empty() {
        return Err(remember_usage_error(
            "--field cannot be combined with --reinforce because reinforcement does not mutate the surviving memory's typed sidecar"
                .to_owned(),
        ));
    }
    if controls.reinforce && attempt_family.is_some() {
        return Err(remember_usage_error(
            "--family cannot be combined with --reinforce because reinforcement corroborates an existing memory instead of recording a new sibling attempt"
                .to_owned(),
        ));
    }
    let idempotency_key = controls
        .idempotency_key
        .map(validate_remember_idempotency_key)
        .transpose()?;
    let idempotency_request_hash = idempotency_key
        .as_ref()
        .map(|_| remember_request_hash(options, typed_field_assignments, attempt_family))
        .transpose()?;

    if idempotency_key.is_some() || controls.reinforce {
        let canonical_content = MemoryContent::parse(options.content)
            .map_err(|error| remember_usage_error(error.to_string()))?
            .as_str()
            .to_owned();
        let workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
        let database_path = options
            .database_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
        let workspace_id = stable_workspace_id(&workspace_path);
        let content_hash = remember_content_hash(&canonical_content);

        if database_path.exists() {
            let connection = open_remember_database_with_retry(&database_path)?;
            migrate_remember_database_with_retry(&connection)?;

            if let Some(key) = idempotency_key.as_deref() {
                let existing = connection
                    .get_remember_idempotency_key(&workspace_id, key)
                    .map_err(|error| DomainError::Storage {
                        message: format!("Failed to look up idempotency key: {error}"),
                        repair: Some("ee doctor --json".to_owned()),
                    })?;
                if let Some(existing) = existing {
                    if idempotency_request_hash
                        .as_deref()
                        .is_some_and(|request_hash| existing.content_hash == request_hash)
                    {
                        return Ok(RememberOutcome::AlreadyRecorded(
                            RememberAlreadyRecordedReport {
                                version: env!("CARGO_PKG_VERSION"),
                                workspace_id,
                                database_path,
                                memory_id: existing.memory_id,
                                idempotency_key: existing.idempotency_key,
                                dry_run: options.dry_run,
                            },
                        ));
                    }
                    return Err(remember_idempotency_conflict_error(key));
                }
            }

            if controls.reinforce {
                let threshold = remember_duplicate_similarity_threshold(&workspace_path);
                let neighbor = remember_reinforce_top_neighbor(
                    &connection,
                    &workspace_id,
                    &canonical_content,
                )?;
                if let Some(neighbor) = neighbor
                    && remember_reinforce_should_apply(neighbor.similarity, threshold)
                {
                    let report = apply_remember_reinforce(
                        &connection,
                        &RememberReinforceContext {
                            workspace_id: &workspace_id,
                            workspace_path: &workspace_path,
                            database_path: &database_path,
                            target_memory_id: &neighbor.memory_id,
                            similarity: neighbor.similarity,
                            threshold,
                            canonical_content: &canonical_content,
                            content_hash: &content_hash,
                            source: options.source,
                            idempotency_key: idempotency_key.as_deref(),
                            dry_run: options.dry_run,
                        },
                    )?;
                    return Ok(RememberOutcome::Reinforced(report));
                }
                // Below threshold (or no neighbor): fall through to create.
            }
        }
    }

    let report = remember_memory_with_index_mode(
        options,
        controls.defer_index_processing,
        typed_field_assignments,
        attempt_family,
    )?;
    if let Some(key) = idempotency_key.as_deref()
        && !options.dry_run
    {
        let request_hash = idempotency_request_hash.as_deref().ok_or_else(|| {
            remember_usage_error("idempotency request hash was not prepared".to_owned())
        })?;
        record_remember_idempotency_key(&report, key, request_hash)?;
    }
    Ok(RememberOutcome::Created(Box::new(report)))
}

/// Store one memory in the separate user-global store used by `ee remember --global`.
///
/// This is intentionally a narrow wrapper around the normal remember pipeline:
/// global writes still run the same validation, audit, index-job, auto-link, and
/// curation proposal code, but their storage target is
/// `<user-data-root>/global/{ee.db,indexes}` instead of the current workspace DB.
pub fn remember_global_memory_with_controls(
    options: &RememberMemoryOptions<'_>,
    controls: &RememberWriteControls<'_>,
) -> Result<RememberOutcome, DomainError> {
    remember_global_memory_with_controls_and_typed_fields(options, controls, &[])
}

/// Global-store counterpart to
/// [`remember_memory_with_controls_and_typed_fields`].
pub fn remember_global_memory_with_controls_and_typed_fields(
    options: &RememberMemoryOptions<'_>,
    controls: &RememberWriteControls<'_>,
    typed_field_assignments: &[String],
) -> Result<RememberOutcome, DomainError> {
    validate_remember_level_kind_cross_wire(options.level, options.kind)?;
    if controls.reinforce {
        return Err(remember_usage_error(
            "--global cannot be combined with --reinforce".to_owned(),
        ));
    }

    let paths = super::global_store::default_global_store_paths_from_env()
        .map_err(remember_global_store_error)?;
    let workspace_id = if options.dry_run {
        super::global_store::global_workspace_id(&paths)
    } else {
        let (connection, workspace_id) = super::global_store::open_or_create_global_store(&paths)
            .map_err(remember_global_store_error)?;
        if let Err(error) = connection.close() {
            tracing::warn!(
                target: "ee::memory",
                event = "global_store_bootstrap_close_failed",
                database_path = %paths.database_path.display(),
                error = %error,
            );
        }
        workspace_id
    };
    let store_override = RememberStoreOverride {
        workspace_id: workspace_id.clone(),
        workspace_path: paths.root.clone(),
        database_path: paths.database_path.clone(),
        index_dir: paths.index_dir.clone(),
    };
    let tags = remember_tags_with_global_scope(options.tags);
    let global_options = RememberMemoryOptions {
        workspace_path: options.workspace_path,
        database_path: Some(&paths.database_path),
        content: options.content,
        workflow_id: options.workflow_id,
        level: options.level,
        kind: options.kind,
        tags: Some(tags.as_str()),
        confidence: options.confidence,
        source: options.source,
        allow_secret_mention: options.allow_secret_mention,
        valid_from: options.valid_from,
        valid_to: options.valid_to,
        dry_run: options.dry_run,
        auto_link: options.auto_link,
        propose_candidates: options.propose_candidates,
    };

    let idempotency_key = controls
        .idempotency_key
        .map(validate_remember_idempotency_key)
        .transpose()?;
    let idempotency_request_hash = idempotency_key
        .as_ref()
        .map(|_| remember_request_hash(&global_options, typed_field_assignments, None))
        .transpose()?;
    if let Some(key) = idempotency_key.as_deref() {
        if paths.database_path.exists() {
            let connection = open_remember_database_with_retry(&paths.database_path)?;
            migrate_remember_database_with_retry(&connection)?;
            if let Some(existing) = connection
                .get_remember_idempotency_key(&workspace_id, key)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to look up global idempotency key: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                })?
            {
                if idempotency_request_hash
                    .as_deref()
                    .is_some_and(|request_hash| existing.content_hash == request_hash)
                {
                    return Ok(RememberOutcome::AlreadyRecorded(
                        RememberAlreadyRecordedReport {
                            version: env!("CARGO_PKG_VERSION"),
                            workspace_id,
                            database_path: paths.database_path,
                            memory_id: existing.memory_id,
                            idempotency_key: existing.idempotency_key,
                            dry_run: options.dry_run,
                        },
                    ));
                }
                return Err(remember_idempotency_conflict_error(key));
            }
        }
    }

    let mut id_source = RememberIdSource::Ambient;
    let report = remember_memory_inner_with_store(
        &global_options,
        &mut id_source,
        None,
        controls.defer_index_processing,
        Some(&store_override),
        typed_field_assignments,
        None,
    )?;
    if let Some(key) = idempotency_key.as_deref()
        && !global_options.dry_run
    {
        let request_hash = idempotency_request_hash.as_deref().ok_or_else(|| {
            remember_usage_error("idempotency request hash was not prepared".to_owned())
        })?;
        record_remember_idempotency_key(&report, key, request_hash)?;
    }
    Ok(RememberOutcome::Created(Box::new(report)))
}

fn remember_tags_with_global_scope(tags: Option<&str>) -> String {
    let mut values = tags
        .map(|tags| {
            tags.split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let already_global = values.iter().any(|tag| {
        let normalized = tag.trim().to_ascii_lowercase().replace('-', "_");
        matches!(normalized.as_str(), "global" | "house_rule")
    });
    if !already_global {
        values.push(GLOBAL_MEMORY_SCOPE_TAG.to_owned());
    }
    values.join(",")
}

fn remember_global_store_error(message: String) -> DomainError {
    DomainError::Storage {
        message,
        repair: Some("Ensure HOME or XDG_DATA_HOME is set and writable, then retry.".to_owned()),
    }
}

// -----------------------------------------------------------------------------
// `ee remember --batch --stdin` (bd-1pi9m.4)
// -----------------------------------------------------------------------------

/// Options for one `ee remember --batch --stdin` invocation.
#[derive(Clone, Copy, Debug)]
pub struct RememberBatchOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Batch-level `--reinforce`; each line may override with its own
    /// `reinforce` field.
    pub reinforce: bool,
    /// Validate and report without writing anything.
    pub dry_run: bool,
    /// Create bounded workflow-local auto-links after successful writes.
    pub auto_link: bool,
    /// Propose curation candidates after persistence.
    pub propose_candidates: bool,
}

/// Per-line outcome for `ee remember --batch --stdin`.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberBatchLineResult {
    /// 1-based line number in the piped JSONL input.
    pub line: usize,
    /// `stored`, `already_recorded`, `reinforced`, `failed`, or the
    /// dry-run previews `would_store` / `would_reinforce`.
    pub status: &'static str,
    /// Created or surviving memory id when the line landed.
    pub memory_id: Option<String>,
    pub error_code: Option<&'static str>,
    pub error_message: Option<String>,
    /// Whether this line strengthened an existing memory.
    pub reinforced: bool,
    /// Similarity to the surviving memory when reinforced.
    pub similarity: Option<f32>,
    /// Staged adjacency suggestions from the create path.
    pub suggested_links: Vec<RememberSuggestedLink>,
}

impl RememberBatchLineResult {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "line": self.line,
            "status": self.status,
            "memoryId": &self.memory_id,
            "errorCode": &self.error_code,
            "errorMessage": &self.error_message,
            "reinforced": self.reinforced,
            "similarity": self.similarity,
            "suggestedLinks": self
                .suggested_links
                .iter()
                .map(remember_suggested_link_json)
                .collect::<Vec<_>>(),
        })
    }
}

fn remember_suggested_link_json(link: &RememberSuggestedLink) -> serde_json::Value {
    serde_json::json!({
        "schema": link.schema,
        "relation": &link.relation,
        "targetMemoryId": &link.target_memory_id,
        "score": link.score,
        "confidence": link.confidence,
        "evidenceCount": link.evidence_count,
        "evidenceSummary": &link.evidence_summary,
        "source": &link.source,
        "matchedTags": &link.matched_tags,
        "nextAction": &link.next_action,
    })
}

/// Result of one `ee remember --batch --stdin` invocation.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberBatchReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// `stored` for live batches, `dry_run` for previews.
    pub status: &'static str,
    pub dry_run: bool,
    pub line_count: usize,
    pub stored_count: usize,
    pub reinforced_count: usize,
    pub already_recorded_count: usize,
    pub failed_count: usize,
    pub results: Vec<RememberBatchLineResult>,
}

impl RememberBatchReport {
    /// `true` when every supplied line failed (exit 5 at the handler,
    /// mirroring the journal batch contract).
    #[must_use]
    pub const fn all_failed(&self) -> bool {
        self.line_count > 0 && self.failed_count == self.line_count
    }

    #[must_use]
    pub fn results_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.results
                .iter()
                .map(RememberBatchLineResult::data_json)
                .collect(),
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "remember",
            "version": self.version,
            "mode": "batch",
            "status": self.status,
            "dryRun": self.dry_run,
            "lineCount": self.line_count,
            "storedCount": self.stored_count,
            "reinforcedCount": self.reinforced_count,
            "alreadyRecordedCount": self.already_recorded_count,
            "failedCount": self.failed_count,
            "results": self.results_json(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!(
            "Remember batch: {} stored, {} reinforced, {} already recorded, {} failed ({} lines{})\n",
            self.stored_count,
            self.reinforced_count,
            self.already_recorded_count,
            self.failed_count,
            self.line_count,
            if self.dry_run { ", dry run" } else { "" }
        );
        for result in &self.results {
            match result.status {
                "failed" => output.push_str(&format!(
                    "  line {}: failed [{}] {}\n",
                    result.line,
                    result.error_code.unwrap_or("unknown"),
                    result.error_message.as_deref().unwrap_or("")
                )),
                status => output.push_str(&format!(
                    "  line {}: {status} {}\n",
                    result.line,
                    result.memory_id.as_deref().unwrap_or("")
                )),
            }
        }
        output
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct RememberBatchLineDraft {
    content: String,
    level: Option<String>,
    kind: Option<String>,
    tags: Option<String>,
    workflow: Option<String>,
    confidence: Option<f32>,
    source: Option<String>,
    allow_secret_mention: bool,
    valid_from: Option<String>,
    valid_to: Option<String>,
    idempotency_key: Option<String>,
    reinforce: Option<bool>,
    typed_field_assignments: Vec<String>,
}

struct RememberBatchLineError {
    code: &'static str,
    message: String,
}

impl RememberBatchLineError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn typed(kind: &MemoryKind, field_hint: Option<&str>, error: MemoryValidationError) -> Self {
        let error = typed_field_validation_error(kind, field_hint, error);
        Self::new(error.code(), error.message())
    }
}

fn remember_batch_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    camel: &str,
    snake: &str,
) -> Result<Option<String>, RememberBatchLineError> {
    for key in [camel, snake] {
        match object.get(key) {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::String(text)) => return Ok(Some(text.clone())),
            Some(_) => {
                return Err(RememberBatchLineError::new(
                    "remember_invalid_json",
                    format!("`{key}` must be a string"),
                ));
            }
        }
    }
    Ok(None)
}

fn remember_batch_bool_field(
    object: &serde_json::Map<String, serde_json::Value>,
    camel: &str,
    snake: &str,
) -> Result<Option<bool>, RememberBatchLineError> {
    for key in [camel, snake] {
        match object.get(key) {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Bool(value)) => return Ok(Some(*value)),
            Some(_) => {
                return Err(RememberBatchLineError::new(
                    "remember_invalid_json",
                    format!("`{key}` must be a boolean"),
                ));
            }
        }
    }
    Ok(None)
}

fn remember_batch_typed_field_assignments(
    object: &serde_json::Map<String, serde_json::Value>,
    kind: &str,
) -> Result<Vec<String>, RememberBatchLineError> {
    let Some(value) = object.get("fields") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let fields = value.as_object().ok_or_else(|| {
        RememberBatchLineError::new(
            "remember_invalid_json",
            "`fields` must be an object whose values are strings or arrays of strings",
        )
    })?;
    if fields.is_empty() {
        return Ok(Vec::new());
    }

    let kind = MemoryKind::from_str(kind).map_err(|error| {
        RememberBatchLineError::new("remember_validation_failed", error.to_string())
    })?;
    let valid_fields = crate::models::memory::typed_memory_field_names(&kind);
    let mut normalized_fields = serde_json::Map::new();
    for (raw_name, value) in fields {
        let name = crate::models::memory::normalize_typed_memory_field_name(raw_name).map_err(
            |reason| {
                RememberBatchLineError::typed(
                    &kind,
                    None,
                    MemoryValidationError::TypedFieldInvalid {
                        field: raw_name.clone(),
                        reason,
                    },
                )
            },
        )?;
        if !valid_fields.contains(&name) {
            return Err(RememberBatchLineError::typed(
                &kind,
                None,
                MemoryValidationError::TypedFieldNotAllowed {
                    kind: kind.as_str().to_owned(),
                    field: name,
                    valid_fields: valid_fields.clone(),
                },
            ));
        }
        if value.is_null() {
            return Err(RememberBatchLineError::typed(
                &kind,
                None,
                MemoryValidationError::TypedFieldInvalid {
                    field: name,
                    reason: "null is not a write value; omit the field instead".to_owned(),
                },
            ));
        }
        if value.as_str().is_some_and(|text| text.trim().is_empty()) {
            return Err(RememberBatchLineError::typed(
                &kind,
                None,
                MemoryValidationError::TypedFieldInvalid {
                    field: name,
                    reason: "value must not be empty".to_owned(),
                },
            ));
        }
        if let Some(items) = value.as_array()
            && (items.is_empty()
                || items
                    .iter()
                    .any(|item| item.as_str().is_some_and(|text| text.trim().is_empty())))
        {
            return Err(RememberBatchLineError::typed(
                &kind,
                None,
                MemoryValidationError::TypedFieldInvalid {
                    field: name,
                    reason: "list must contain at least one non-empty string".to_owned(),
                },
            ));
        }
        if normalized_fields
            .insert(name.clone(), value.clone())
            .is_some()
        {
            return Err(RememberBatchLineError::typed(
                &kind,
                None,
                MemoryValidationError::TypedFieldInvalid {
                    field: name,
                    reason: "field appears more than once after name normalization".to_owned(),
                },
            ));
        }
    }

    let raw_json = serde_json::to_string(&normalized_fields).map_err(|error| {
        RememberBatchLineError::new(
            "remember_invalid_json",
            format!("failed to encode `fields`: {error}"),
        )
    })?;
    let canonical = crate::models::memory::canonicalize_typed_memory_fields_json(&kind, &raw_json)
        .map_err(|error| RememberBatchLineError::typed(&kind, None, error))?;
    let fields = crate::models::memory::typed_memory_fields_from_json(&kind, &canonical)
        .map_err(|error| RememberBatchLineError::typed(&kind, None, error))?;
    let mut assignments = Vec::new();
    for (name, value) in fields {
        match value {
            serde_json::Value::String(value) => {
                assignments.push(format!("{name}={value}"));
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    let Some(value) = value.as_str() else {
                        return Err(RememberBatchLineError::typed(
                            &kind,
                            None,
                            MemoryValidationError::TypedFieldInvalid {
                                field: name,
                                reason: "canonical list contained a non-string item".to_owned(),
                            },
                        ));
                    };
                    assignments.push(format!("{name}={value}"));
                }
            }
            _ => {
                return Err(RememberBatchLineError::typed(
                    &kind,
                    None,
                    MemoryValidationError::TypedFieldInvalid {
                        field: name,
                        reason: "canonical value was not a string or string array".to_owned(),
                    },
                ));
            }
        }
    }
    Ok(assignments)
}

fn parse_remember_batch_line(line: &str) -> Result<RememberBatchLineDraft, RememberBatchLineError> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
        RememberBatchLineError::new(
            "remember_invalid_json",
            format!("invalid JSONL line: {error}"),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        RememberBatchLineError::new(
            "remember_invalid_json",
            "each JSONL line must be one remember input object",
        )
    })?;

    let content = remember_batch_string_field(object, "content", "content")?.ok_or_else(|| {
        RememberBatchLineError::new(
            "remember_content_required",
            "JSONL entry is missing the required `content` string",
        )
    })?;
    let tags = match object.get("tags") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(serde_json::Value::Array(items)) => {
            let mut parsed = Vec::with_capacity(items.len());
            for item in items {
                let Some(tag) = item.as_str() else {
                    return Err(RememberBatchLineError::new(
                        "remember_invalid_json",
                        "`tags` must be a comma-separated string or an array of strings",
                    ));
                };
                parsed.push(tag.to_owned());
            }
            if parsed.is_empty() {
                None
            } else {
                Some(parsed.join(","))
            }
        }
        Some(_) => {
            return Err(RememberBatchLineError::new(
                "remember_invalid_json",
                "`tags` must be a comma-separated string or an array of strings",
            ));
        }
    };
    let confidence = match object.get("confidence") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(number)) => match number.as_f64() {
            Some(value) => Some(value as f32),
            None => {
                return Err(RememberBatchLineError::new(
                    "remember_invalid_json",
                    "`confidence` must be a number",
                ));
            }
        },
        Some(_) => {
            return Err(RememberBatchLineError::new(
                "remember_invalid_json",
                "`confidence` must be a number",
            ));
        }
    };

    let kind = remember_batch_string_field(object, "kind", "kind")?;
    let typed_field_assignments =
        remember_batch_typed_field_assignments(object, kind.as_deref().unwrap_or("fact"))?;

    Ok(RememberBatchLineDraft {
        content,
        level: remember_batch_string_field(object, "level", "level")?,
        kind,
        tags,
        workflow: remember_batch_string_field(object, "workflow", "workflow")?,
        confidence,
        source: remember_batch_string_field(object, "source", "source")?,
        allow_secret_mention: remember_batch_bool_field(
            object,
            "allowSecretMention",
            "allow_secret_mention",
        )?
        .unwrap_or(false),
        valid_from: remember_batch_string_field(object, "validFrom", "valid_from")?,
        valid_to: remember_batch_string_field(object, "validTo", "valid_to")?,
        idempotency_key: remember_batch_string_field(object, "idempotencyKey", "idempotency_key")?,
        reinforce: remember_batch_bool_field(object, "reinforce", "reinforce")?,
        typed_field_assignments,
    })
}

fn remember_batch_error_code(error: &DomainError) -> &'static str {
    match error {
        DomainError::UsageCodeWithDetails { code, .. } => code,
        DomainError::Usage { .. }
        | DomainError::UsageWithDetails { .. }
        | DomainError::NotFound { .. } => "remember_validation_failed",
        DomainError::PolicyDenied { .. } | DomainError::PolicyDeniedWithDetails { .. } => {
            "remember_policy_denied"
        }
        DomainError::Configuration { .. } => "remember_configuration_failed",
        _ => "remember_storage_failed",
    }
}

/// Append a JSONL batch of remember inputs (the `ee remember --batch
/// --stdin` surface). Each line is validated and persisted INDEPENDENTLY —
/// a harness flushing 12 lessons must not lose 11 because one was oversize.
pub fn remember_memory_batch_stdin(
    options: &RememberBatchOptions<'_>,
    input: &str,
) -> Result<RememberBatchReport, DomainError> {
    let lines: Vec<&str> = input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Err(DomainError::Usage {
            message: "remember --batch --stdin requires at least one JSONL line".to_owned(),
            repair: Some(
                "printf '%s\\n' '{\"content\":\"...\"}' | ee remember --batch --stdin --json"
                    .to_owned(),
            ),
        });
    }
    if lines.len() > REMEMBER_BATCH_MAX_LINES {
        return Err(DomainError::Usage {
            message: format!(
                "remember --batch --stdin accepts at most {REMEMBER_BATCH_MAX_LINES} lines per \
                 invocation; got {}",
                lines.len()
            ),
            repair: Some("split the JSONL input into smaller batches".to_owned()),
        });
    }

    let mut results = Vec::with_capacity(lines.len());
    let mut stored_count = 0_usize;
    let mut reinforced_count = 0_usize;
    let mut already_recorded_count = 0_usize;
    let mut failed_count = 0_usize;

    for (index, line) in lines.iter().enumerate() {
        let line_number = index + 1;
        let draft = match parse_remember_batch_line(line) {
            Ok(draft) => draft,
            Err(error) => {
                failed_count += 1;
                results.push(RememberBatchLineResult {
                    line: line_number,
                    status: "failed",
                    memory_id: None,
                    error_code: Some(error.code),
                    error_message: Some(error.message),
                    reinforced: false,
                    similarity: None,
                    suggested_links: Vec::new(),
                });
                continue;
            }
        };

        let line_options = RememberMemoryOptions {
            workspace_path: options.workspace_path,
            database_path: options.database_path,
            content: &draft.content,
            workflow_id: draft.workflow.as_deref(),
            level: draft.level.as_deref().unwrap_or("episodic"),
            kind: draft.kind.as_deref().unwrap_or("fact"),
            tags: draft.tags.as_deref(),
            confidence: draft.confidence.unwrap_or(0.8),
            source: draft.source.as_deref(),
            allow_secret_mention: draft.allow_secret_mention,
            valid_from: draft.valid_from.as_deref(),
            valid_to: draft.valid_to.as_deref(),
            dry_run: options.dry_run,
            auto_link: options.auto_link,
            propose_candidates: options.propose_candidates,
        };
        let line_controls = RememberWriteControls {
            reinforce: draft.reinforce.unwrap_or(options.reinforce),
            idempotency_key: draft.idempotency_key.as_deref(),
            // bd-2efx1: every line leaves its index job pending; one
            // coalesced rebuild below covers the whole batch.
            defer_index_processing: true,
        };

        // Per-line independent persistence: each line runs its own full
        // remember flow, so a failure here reports on this line without
        // touching earlier or later lines.
        match remember_memory_with_controls_and_typed_fields(
            &line_options,
            &line_controls,
            &draft.typed_field_assignments,
        ) {
            Ok(RememberOutcome::Created(report)) => {
                stored_count += 1;
                results.push(RememberBatchLineResult {
                    line: line_number,
                    status: if options.dry_run {
                        "would_store"
                    } else {
                        "stored"
                    },
                    memory_id: Some(report.memory_id.to_string()),
                    error_code: None,
                    error_message: None,
                    reinforced: false,
                    similarity: None,
                    suggested_links: report.suggested_links,
                });
            }
            Ok(RememberOutcome::AlreadyRecorded(report)) => {
                already_recorded_count += 1;
                results.push(RememberBatchLineResult {
                    line: line_number,
                    status: "already_recorded",
                    memory_id: Some(report.memory_id),
                    error_code: None,
                    error_message: None,
                    reinforced: false,
                    similarity: None,
                    suggested_links: Vec::new(),
                });
            }
            Ok(RememberOutcome::Reinforced(report)) => {
                reinforced_count += 1;
                results.push(RememberBatchLineResult {
                    line: line_number,
                    status: if options.dry_run {
                        "would_reinforce"
                    } else {
                        "reinforced"
                    },
                    memory_id: Some(report.memory_id),
                    error_code: None,
                    error_message: None,
                    reinforced: true,
                    similarity: Some(report.similarity),
                    suggested_links: Vec::new(),
                });
            }
            Err(error) => {
                failed_count += 1;
                results.push(RememberBatchLineResult {
                    line: line_number,
                    status: "failed",
                    memory_id: None,
                    error_code: Some(remember_batch_error_code(&error)),
                    error_message: Some(error.message()),
                    reinforced: false,
                    similarity: None,
                    suggested_links: Vec::new(),
                });
            }
        }
    }

    // bd-2efx1: one coalesced index rebuild for the whole batch. Every
    // stored line enqueued its job transactionally; draining here replaces
    // the per-line full rebuild that made batch ingest O(n²).
    if !options.dry_run && (stored_count > 0 || reinforced_count > 0) {
        let workspace_path = resolve_workspace_path(options.workspace_path, false)?;
        let database_path = options
            .database_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
        let workspace_id = stable_workspace_id(&workspace_path);
        let index_dir = workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR);
        let connection = open_remember_database_with_retry(&database_path)?;
        process_pending_index_jobs_coalesced(&connection, &workspace_id, &index_dir, None)
            .map_err(remember_search_index_error)?;
    }

    Ok(RememberBatchReport {
        version: env!("CARGO_PKG_VERSION"),
        status: if options.dry_run { "dry_run" } else { "stored" },
        dry_run: options.dry_run,
        line_count: lines.len(),
        stored_count,
        reinforced_count,
        already_recorded_count,
        failed_count,
        results,
    })
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
    let typed_fields = match conn.get_memory_typed_fields_json(options.memory_id) {
        Ok(Some(raw)) => {
            let kind = match MemoryKind::from_str(&memory.kind) {
                Ok(kind) => kind,
                Err(error) => {
                    return MemoryShowReport::error(format!(
                        "Failed to parse memory kind for typed fields: {error}"
                    ));
                }
            };
            match crate::models::memory::typed_memory_fields_from_json(&kind, &raw) {
                Ok(fields) => Some(serde_json::json!(fields)),
                Err(error) => {
                    return MemoryShowReport::error(format!(
                        "Invalid typed memory fields: {error}"
                    ));
                }
            }
        }
        Ok(None) => None,
        Err(e) => {
            return MemoryShowReport::error(format!("Failed to query typed memory fields: {e}"));
        }
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

    MemoryShowReport::found(MemoryDetails {
        memory,
        tags,
        typed_fields,
    })
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
    // relative paths, symlinked paths, and the user-global store root all
    // address the same records (GH#23: prefers the DB's own path-keyed
    // workspace row, falling back to the canonical-path hash).
    let workspace_id = workspace_id_for_database(&conn, options.workspace_path);

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

/// Resolve the workspace id a memory verb should scope to.
///
/// Prefers the opened database's canonical path-keyed workspace row (GH#23):
/// `remember` and search canonicalize workspace paths, so memory verbs must do
/// the same before considering a legacy lexical alias. A database may contain
/// both rows after a path-naive command registers `./relative-path`; choosing
/// the raw alias first can silently scope reads to an empty workspace while the
/// canonical row owns every memory. The raw path remains a fallback for older
/// databases and the user-global store (ADR 0083), where the recorded root may
/// predate canonicalization. When no row matches, use the canonical-path hash,
/// preserving the historical workspace-ID derivation.
pub(crate) fn workspace_id_for_database(conn: &DbConnection, workspace_path: &Path) -> String {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    if let Ok(Some(row)) = conn.get_workspace_by_path(&canonical.to_string_lossy()) {
        return row.id;
    }
    if canonical != workspace_path
        && let Ok(Some(row)) = conn.get_workspace_by_path(&workspace_path.to_string_lossy())
    {
        return row.id;
    }
    stable_workspace_id(&canonical)
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
    let workspace_id = workspace_id_for_database(&conn, options.workspace_path);
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
    let workspace_id = workspace_id_for_database(&conn, options.workspace_path);
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
    let demotes_peer_attestation = memory.trust_class == TrustClass::PeerHumanAttested.as_str();
    let mut evidence_refs = vec![
        format!("actor:{}", actor.unwrap_or("ee memory level")),
        format!("reason:{manual_reason}"),
    ];
    if demotes_peer_attestation {
        evidence_refs
            .push("trust_class_transition:peer_human_attested->agent_assertion".to_owned());
    }

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
    let workspace_id = workspace_id_for_database(&conn, options.workspace_path);
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
                .into_iter()
                .filter(|link| {
                    crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
                })
                .map(|link| stored_memory_link_item(&link))
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
                    repair: Some("Use ee memory show to inspect them.".to_owned()),
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
    let workspace_id = workspace_id_for_database(&conn, options.workspace_path);
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
                repair: Some("Use ee memory show to inspect it.".to_owned()),
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

/// Stable schema name for the read-only memory time-travel report.
pub use crate::models::schema::TIMELINE_SCHEMA_V1;

/// Options for reconstructing what was knowable about a topic at a point in time.
#[derive(Clone, Debug)]
pub struct MemoryTimelineOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Workspace path used to derive the workspace id.
    pub workspace_path: &'a Path,
    /// Natural-language topic to match against memory content and tags.
    pub topic: &'a str,
    /// RFC3339 timestamp for the historical view.
    pub as_of: &'a str,
    /// Maximum rows per report section.
    pub limit: u32,
}

/// One memory that was in effect at the requested timeline instant.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineMemory {
    pub memory_id: String,
    pub level: String,
    pub kind: String,
    pub content: String,
    pub tags: Vec<String>,
    pub confidence: f32,
    pub trust_class: String,
    pub trust_subclass: Option<String>,
    pub provenance_uri: Option<String>,
    pub known_at: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub validity_then: String,
    pub validity_window_kind: String,
    pub is_tombstoned_then: bool,
}

/// One deterministic lifecycle change observed after the requested instant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineChange {
    pub change_type: String,
    pub changed_at: String,
    pub memory_id: String,
    pub level: String,
    pub kind: String,
    pub content_preview: String,
    pub reason: String,
}

/// Read-only `ee.timeline.v1` data payload.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTimelineReport {
    pub schema: &'static str,
    pub command: String,
    pub topic: String,
    pub as_of: String,
    pub memories_then: Vec<TimelineMemory>,
    pub changes_since: Vec<TimelineChange>,
    pub decisions_in_effect: Vec<TimelineMemory>,
    pub total_memories_then: u32,
    pub total_changes_since: u32,
    pub total_decisions_in_effect: u32,
    pub truncated: bool,
}

impl MemoryTimelineReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "schema": TIMELINE_SCHEMA_V1,
                "command": "timeline",
                "topic": self.topic,
                "asOf": self.as_of,
                "memoriesThen": [],
                "changesSince": [],
                "decisionsInEffect": [],
                "totalMemoriesThen": self.total_memories_then,
                "totalChangesSince": self.total_changes_since,
                "totalDecisionsInEffect": self.total_decisions_in_effect,
                "truncated": true,
            })
        })
    }

    #[must_use]
    pub fn human_output(&self) -> String {
        let mut out = String::new();
        out.push_str("ee timeline\n\n");
        out.push_str(&format!("Topic: {}\n", self.topic));
        out.push_str(&format!("As of: {}\n", self.as_of));
        out.push_str(&format!(
            "Memories then: {} shown of {}\n",
            self.memories_then.len(),
            self.total_memories_then
        ));
        for memory in &self.memories_then {
            out.push_str(&format!(
                "- {} [{} {} confidence {:.3}] {}\n",
                memory.memory_id, memory.level, memory.kind, memory.confidence, memory.content
            ));
        }
        out.push_str(&format!(
            "Changes since: {} shown of {}\n",
            self.changes_since.len(),
            self.total_changes_since
        ));
        for change in &self.changes_since {
            out.push_str(&format!(
                "- {} {} at {} ({})\n",
                change.memory_id, change.change_type, change.changed_at, change.reason
            ));
        }
        out.push_str(&format!(
            "Decisions in effect: {} shown of {}\n",
            self.decisions_in_effect.len(),
            self.total_decisions_in_effect
        ));
        for decision in &self.decisions_in_effect {
            out.push_str(&format!("- {} {}\n", decision.memory_id, decision.content));
        }
        if self.truncated {
            out.push_str("\nResults truncated by --limit.\n");
        }
        out
    }
}

#[derive(Clone, Debug)]
struct TimelineSourceMemory {
    memory: StoredMemory,
    tags: Vec<String>,
    effective_from: DateTime<Utc>,
    valid_to: Option<DateTime<Utc>>,
    tombstoned_at: Option<DateTime<Utc>>,
}

/// Reconstruct a deterministic as-of memory timeline for a topic.
pub fn build_memory_timeline(
    options: &MemoryTimelineOptions<'_>,
) -> Result<MemoryTimelineReport, DomainError> {
    let topic = options.topic.trim();
    if topic.is_empty() {
        return Err(DomainError::Usage {
            message: "timeline topic must not be empty".to_owned(),
            repair: Some(
                "ee timeline \"release process\" --as-of 2026-05-01T00:00:00Z --json".to_owned(),
            ),
        });
    }
    let as_of = parse_timeline_as_of(options.as_of)?;
    let normalized_as_of = normalize_timeline_timestamp(&as_of);
    let tokens = timeline_topic_tokens(topic);
    let conn = open_migrated_memory_database(options.database_path).map_err(|message| {
        DomainError::Storage {
            message,
            repair: Some(crate::core::storeless_workspace_repair(
                options.database_path,
            )),
        }
    })?;
    let workspace_path = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&workspace_path);
    let stored = conn
        .list_memories_for_retrieval(&workspace_id, None, true)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories for timeline: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;

    let mut sources = Vec::new();
    for memory in stored {
        let tags = conn
            .get_memory_tags(&memory.id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to load memory tags for timeline: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        if !timeline_memory_matches_topic(&memory, &tags, &tokens, topic) {
            continue;
        }
        sources.push(TimelineSourceMemory {
            effective_from: timeline_effective_from(&memory, &as_of),
            valid_to: parse_stored_timeline_timestamp(memory.valid_to.as_deref()),
            tombstoned_at: parse_stored_timeline_timestamp(memory.tombstoned_at.as_deref()),
            memory,
            tags,
        });
    }
    sources.sort_by(timeline_source_order);

    let mut all_memories_then = Vec::new();
    let mut all_changes_since = Vec::new();
    let mut all_decisions = Vec::new();
    for source in &sources {
        if timeline_source_active_at(source, &as_of) {
            let memory = timeline_memory(source, &as_of);
            if source.memory.kind == "decision" {
                all_decisions.push(memory.clone());
            }
            all_memories_then.push(memory);
        }
        append_timeline_changes_since(source, &as_of, &mut all_changes_since);
    }
    all_memories_then.sort_by(timeline_memory_order);
    all_decisions.sort_by(timeline_memory_order);
    all_changes_since.sort_by(timeline_change_order);

    let limit = usize::try_from(options.limit).unwrap_or(usize::MAX);
    let total_memories_then = all_memories_then.len() as u32;
    let total_changes_since = all_changes_since.len() as u32;
    let total_decisions_in_effect = all_decisions.len() as u32;
    let truncated = all_memories_then.len() > limit
        || all_changes_since.len() > limit
        || all_decisions.len() > limit;
    all_memories_then.truncate(limit);
    all_changes_since.truncate(limit);
    all_decisions.truncate(limit);

    Ok(MemoryTimelineReport {
        schema: TIMELINE_SCHEMA_V1,
        command: "timeline".to_owned(),
        topic: topic.to_owned(),
        as_of: normalized_as_of,
        memories_then: all_memories_then,
        changes_since: all_changes_since,
        decisions_in_effect: all_decisions,
        total_memories_then,
        total_changes_since,
        total_decisions_in_effect,
        truncated,
    })
}

fn parse_timeline_as_of(raw: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| DomainError::Usage {
            message: format!("--as-of must be an RFC3339 timestamp: {error}"),
            repair: Some("ee timeline \"topic\" --as-of 2026-05-01T00:00:00Z --json".to_owned()),
        })
}

fn parse_stored_timeline_timestamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
    raw.and_then(|value| {
        DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc))
    })
}

fn normalize_timeline_timestamp(timestamp: &DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn timeline_topic_tokens(topic: &str) -> Vec<String> {
    let mut tokens: Vec<String> = topic
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|part| {
            let token = part.trim().to_ascii_lowercase();
            (!token.is_empty()).then_some(token)
        })
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn timeline_memory_matches_topic(
    memory: &StoredMemory,
    tags: &[String],
    tokens: &[String],
    topic: &str,
) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        memory.content,
        memory.kind,
        memory.level,
        tags.join(" ")
    )
    .to_ascii_lowercase();
    let topic_lower = topic.to_ascii_lowercase();
    haystack.contains(&topic_lower)
        || (!tokens.is_empty() && tokens.iter().all(|token| haystack.contains(token)))
}

fn timeline_effective_from(memory: &StoredMemory, fallback: &DateTime<Utc>) -> DateTime<Utc> {
    parse_stored_timeline_timestamp(memory.valid_from.as_deref())
        .or_else(|| parse_stored_timeline_timestamp(Some(memory.created_at.as_str())))
        .unwrap_or_else(|| fallback.to_owned())
}

fn timeline_source_active_at(source: &TimelineSourceMemory, as_of: &DateTime<Utc>) -> bool {
    &source.effective_from <= as_of
        && source
            .valid_to
            .as_ref()
            .is_none_or(|valid_to| valid_to > as_of)
        && source
            .tombstoned_at
            .as_ref()
            .is_none_or(|tombstoned_at| tombstoned_at > as_of)
}

fn timeline_validity_then(source: &TimelineSourceMemory, as_of: &DateTime<Utc>) -> &'static str {
    if &source.effective_from > as_of {
        "future"
    } else if source
        .tombstoned_at
        .as_ref()
        .is_some_and(|tombstoned_at| tombstoned_at <= as_of)
    {
        "tombstoned"
    } else if source
        .valid_to
        .as_ref()
        .is_some_and(|valid_to| valid_to <= as_of)
    {
        "expired"
    } else {
        "active"
    }
}

fn timeline_memory(source: &TimelineSourceMemory, as_of: &DateTime<Utc>) -> TimelineMemory {
    TimelineMemory {
        memory_id: source.memory.id.clone(),
        level: source.memory.level.clone(),
        kind: source.memory.kind.clone(),
        content: source.memory.content.clone(),
        tags: source.tags.clone(),
        confidence: source.memory.confidence,
        trust_class: source.memory.trust_class.clone(),
        trust_subclass: source.memory.trust_subclass.clone(),
        provenance_uri: source.memory.provenance_uri.clone(),
        known_at: normalize_timeline_timestamp(&source.effective_from),
        valid_from: source.memory.valid_from.clone(),
        valid_to: source.memory.valid_to.clone(),
        validity_then: timeline_validity_then(source, as_of).to_owned(),
        validity_window_kind: validity_window_kind(
            source.memory.valid_from.as_deref(),
            source.memory.valid_to.as_deref(),
        )
        .to_owned(),
        is_tombstoned_then: source
            .tombstoned_at
            .as_ref()
            .is_some_and(|tombstoned_at| tombstoned_at <= as_of),
    }
}

fn append_timeline_changes_since(
    source: &TimelineSourceMemory,
    as_of: &DateTime<Utc>,
    changes: &mut Vec<TimelineChange>,
) {
    if &source.effective_from > as_of {
        changes.push(timeline_change(
            "added",
            source.effective_from.to_owned(),
            source,
            "memory became applicable after as-of",
        ));
    }
    if let Some(valid_to) = source.valid_to.as_ref()
        && valid_to > as_of
        && &source.effective_from <= as_of
    {
        changes.push(timeline_change(
            "superseded",
            valid_to.to_owned(),
            source,
            "memory validity window ended after as-of",
        ));
    }
    if let Some(tombstoned_at) = source.tombstoned_at.as_ref()
        && tombstoned_at > as_of
    {
        changes.push(timeline_change(
            "tombstoned",
            tombstoned_at.to_owned(),
            source,
            "memory was tombstoned after as-of",
        ));
    }
}

fn timeline_change(
    change_type: &str,
    changed_at: DateTime<Utc>,
    source: &TimelineSourceMemory,
    reason: &str,
) -> TimelineChange {
    TimelineChange {
        change_type: change_type.to_owned(),
        changed_at: normalize_timeline_timestamp(&changed_at),
        memory_id: source.memory.id.clone(),
        level: source.memory.level.clone(),
        kind: source.memory.kind.clone(),
        content_preview: truncate_content(&source.memory.content).0,
        reason: reason.to_owned(),
    }
}

fn timeline_source_order(left: &TimelineSourceMemory, right: &TimelineSourceMemory) -> Ordering {
    left.effective_from
        .cmp(&right.effective_from)
        .then_with(|| left.memory.id.cmp(&right.memory.id))
}

fn timeline_memory_order(left: &TimelineMemory, right: &TimelineMemory) -> Ordering {
    left.known_at
        .cmp(&right.known_at)
        .then_with(|| left.memory_id.cmp(&right.memory_id))
}

fn timeline_change_order(left: &TimelineChange, right: &TimelineChange) -> Ordering {
    left.changed_at
        .cmp(&right.changed_at)
        .then_with(|| left.change_type.cmp(&right.change_type))
        .then_with(|| left.memory_id.cmp(&right.memory_id))
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
    /// Optional graph-derived impact analysis for dry-run revision previews.
    pub impact_analysis: Option<crate::graph::dominance::MemoryImpactAnalysisReport>,
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
            impact_analysis: None,
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
            impact_analysis: None,
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
            impact_analysis: None,
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
            impact_analysis: None,
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
            impact_analysis: None,
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
            impact_analysis: None,
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
            impact_analysis: None,
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
            impact_analysis: None,
            error: Some(message),
        }
    }

    /// Attach graph-derived impact analysis to a dry-run preview.
    #[must_use]
    pub fn with_impact_analysis(
        mut self,
        impact_analysis: Option<crate::graph::dominance::MemoryImpactAnalysisReport>,
    ) -> Self {
        self.impact_analysis = impact_analysis;
        self
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

    let revised_trust_class = if original.trust_class == TrustClass::PeerHumanAttested.as_str() {
        changed_fields.push("trust_class".to_owned());
        TrustClass::AgentAssertion.as_str().to_owned()
    } else {
        original.trust_class.clone()
    };

    // If dry run, return preview
    if options.dry_run {
        return MemoryReviseReport::dry_run_preview(
            options.original_memory_id.to_owned(),
            options.reason.clone(),
            changed_fields,
        )
        .with_impact_analysis(memory_revision_impact_analysis(
            &conn,
            &original.workspace_id,
            options.original_memory_id,
            options.database_path,
        ));
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
        trust_class: revised_trust_class.clone(),
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
        "from_trust_class": original.trust_class.clone(),
        "to_trust_class": revised_trust_class,
        "reason": options.reason.as_str(),
        "actor": options.actor.unwrap_or("ee memory revise"),
        "revised_at": revised_at,
    });

    let result: Result<(), String> = conn
        .with_transaction(|| {
            conn.insert_memory_revision(&new_id, &logical_id, &memory_input)?;
            // bd-multiplicity-aware-trust-p0u7g: the live revision inherits
            // the attempt-family pointer; the slot ledger inherits by
            // logical_id and is never copied.
            conn.carry_memory_attempt_family_pointer(options.original_memory_id, &new_id)?;
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

fn memory_revision_impact_analysis(
    conn: &crate::db::DbConnection,
    workspace_id: &str,
    memory_id: &str,
    database_path: &Path,
) -> Option<crate::graph::dominance::MemoryImpactAnalysisReport> {
    if !revision_dominance_feature_enabled(database_path) {
        return Some(revision_dominance_disabled_impact_analysis(memory_id));
    }
    let graph = crate::graph::build_revision_dag_from_logical_ids(conn, workspace_id).ok()?;
    let snapshot_version = conn
        .get_latest_graph_snapshot(workspace_id, crate::db::GraphSnapshotType::RevisionDag)
        .ok()
        .flatten()
        .map_or(0, |snapshot| u64::from(snapshot.snapshot_version));
    crate::graph::dominance::compute_memory_impact_analysis(&graph, memory_id, snapshot_version)
        .ok()
}

fn revision_dominance_feature_enabled(database_path: &Path) -> bool {
    let Some(workspace_root) = workspace_root_from_database_path(database_path) else {
        return false;
    };
    let options = ConfigSurfaceOptions {
        workspace_root,
        config_path: None,
    };
    get_config(&options, GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY)
        .map(|report| report.value == "true")
        .unwrap_or(false)
}

fn workspace_root_from_database_path(database_path: &Path) -> Option<PathBuf> {
    if database_path.file_name()?.to_str()? != "ee.db" {
        return None;
    }
    let ee_dir = database_path.parent()?;
    if ee_dir.file_name()?.to_str()? != ".ee" {
        return None;
    }
    ee_dir.parent().map(Path::to_path_buf)
}

fn revision_dominance_disabled_impact_analysis(
    memory_id: &str,
) -> crate::graph::dominance::MemoryImpactAnalysisReport {
    crate::graph::dominance::MemoryImpactAnalysisReport {
        schema: crate::graph::dominance::MEMORY_IMPACT_ANALYSIS_SCHEMA_V1,
        memory_id: memory_id.to_owned(),
        snapshot_version: 0,
        revision_lineage: Vec::new(),
        impact_analysis: crate::graph::dominance::RevisionImpactAnalysis {
            immediate_dominator: None,
            dominance_frontier: Vec::new(),
            affected_memory_count: 0,
            validation_status: "disabled".to_owned(),
        },
        frontiers: Vec::new(),
        degraded: vec![crate::graph::dominance::DominanceDegradation {
            code: "graph_feature_disabled".to_owned(),
            severity: "medium".to_owned(),
            message: format!(
                "Revision dominance is disabled by {GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY}."
            ),
            repair: Some(format!(
                "ee config set {GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY} true"
            )),
        }],
    }
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

/// Stable schema for redaction-safe peer conflict observations.
pub const PEER_CONFLICT_SCHEMA_V1: &str = "ee.peer_conflict.v1";

const PEER_CONFLICT_HASH_PREFIX_LEN: usize = 32;

/// Stable event kind emitted by the conservative SRR6.37 peer-conflict detector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerConflictKind {
    /// A peer memory has the same content hash as the primary memory.
    DuplicateDetected,
    /// A peer memory is close enough by SimHash to require provenance-aware rendering.
    NearDuplicateCandidate,
    /// A peer memory appears to contradict the primary memory.
    ContradictionCandidate,
}

impl PeerConflictKind {
    /// Stable wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateDetected => "duplicate_detected",
            Self::NearDuplicateCandidate => "near_duplicate_candidate",
            Self::ContradictionCandidate => "contradiction_candidate",
        }
    }
}

/// Stable detector verdict for a peer conflict observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerConflictDetectorVerdict {
    /// Exact content-hash duplicate.
    ExactDuplicate,
    /// SimHash near duplicate.
    NearDuplicate,
    /// Deterministic contradiction heuristic matched.
    Contradiction,
}

impl PeerConflictDetectorVerdict {
    /// Stable wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactDuplicate => "exact_duplicate",
            Self::NearDuplicate => "near_duplicate",
            Self::Contradiction => "contradiction",
        }
    }
}

/// Conservative contradiction signal used by the first peer-conflict detector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerContradictionSignal {
    /// One claim negates another while preserving substantial token overlap.
    ClaimNegationOverlap,
    /// Rule-like wording inverts the predicate (for example always vs never).
    RulePredicateInversion,
    /// Same revision token with different content and different trust class.
    TrustClassDisagreementAtSameRevision,
    /// One peer record explicitly supersedes the other.
    ExplicitSupersessionChain,
}

impl PeerContradictionSignal {
    /// Stable wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaimNegationOverlap => "claim_negation_overlap",
            Self::RulePredicateInversion => "rule_predicate_inversion",
            Self::TrustClassDisagreementAtSameRevision => {
                "trust_class_disagreement_at_same_revision"
            }
            Self::ExplicitSupersessionChain => "explicit_supersession_chain",
        }
    }
}

/// SimHash score attached to a near-duplicate peer conflict row.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerNearDuplicateScore {
    /// Hamming distance between the primary and peer SimHashes.
    pub hamming_distance: u32,
}

/// Deterministic contradiction score attached to a peer conflict row.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerContradictionScore {
    /// Conservative score in `[0, 1]`.
    pub score: f32,
    /// Stable signal name.
    pub signal: &'static str,
}

/// Redaction-safe peer conflict event. Raw memory IDs and memory bodies are
/// intentionally excluded; callers pass hashed memory refs and content hashes.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerConflictEvent {
    /// Stable schema marker.
    pub schema: &'static str,
    /// The detector is read-only and only reports an observation.
    pub side_effect_free: bool,
    /// Stable event kind.
    pub kind: &'static str,
    /// Caller-supplied RFC3339 timestamp for deterministic replay.
    pub ts: String,
    /// Hashed workspace identifier.
    pub workspace_id_hash: String,
    /// Hashed primary memory reference.
    pub primary_memory_hash: String,
    /// Hashed peer memory references participating in this row.
    pub peer_memory_hashes: Vec<String>,
    /// Trust classes in primary-then-peer order.
    pub trust_classes: Vec<String>,
    /// Stable detector verdict.
    pub detector_verdict: &'static str,
    /// Optional near-duplicate score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub near_duplicate_score: Option<PeerNearDuplicateScore>,
    /// Optional contradiction score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contradiction_score: Option<PeerContradictionScore>,
    /// Rendering policy for downstream context/why/curate surfaces.
    pub rendering_policy: &'static str,
}

/// Caller-provided memory facts for peer conflict detection.
#[derive(Clone, Debug)]
pub struct PeerConflictMemory<'a> {
    /// Redaction-safe memory reference hash.
    pub memory_hash: &'a str,
    /// Content hash used for exact duplicate detection.
    pub content_hash: &'a str,
    /// Redacted or local-only body used by deterministic heuristics.
    pub content: &'a str,
    /// Stable SimHash for near-duplicate ordering.
    pub simhash: SimHash128,
    /// Trust class at detection time.
    pub trust_class: &'a str,
    /// Optional revision token.
    pub revision_token: Option<&'a str>,
    /// Redaction-safe hashes this record explicitly supersedes.
    pub supersedes_hashes: &'a [&'a str],
}

impl<'a> PeerConflictMemory<'a> {
    /// Create peer conflict facts from caller-owned values.
    #[must_use]
    pub const fn new(
        memory_hash: &'a str,
        content_hash: &'a str,
        content: &'a str,
        simhash: SimHash128,
        trust_class: &'a str,
    ) -> Self {
        Self {
            memory_hash,
            content_hash,
            content,
            simhash,
            trust_class,
            revision_token: None,
            supersedes_hashes: &[],
        }
    }

    /// Attach an optional revision token.
    #[must_use]
    pub const fn with_revision_token(mut self, revision_token: Option<&'a str>) -> Self {
        self.revision_token = revision_token;
        self
    }

    /// Attach explicit supersession links.
    #[must_use]
    pub const fn with_supersedes_hashes(mut self, supersedes_hashes: &'a [&'a str]) -> Self {
        self.supersedes_hashes = supersedes_hashes;
        self
    }
}

/// Options for deterministic peer-conflict detection.
#[derive(Clone, Debug)]
pub struct PeerConflictDetectionOptions<'a> {
    /// Hashed workspace identifier.
    pub workspace_id_hash: &'a str,
    /// RFC3339 timestamp to place on every emitted row.
    pub observed_at: &'a str,
    /// Maximum SimHash distance for near-duplicate rows.
    pub near_duplicate_hamming_distance: u32,
    /// Maximum near-duplicate rows to emit.
    pub near_duplicate_limit: usize,
}

impl<'a> PeerConflictDetectionOptions<'a> {
    /// Create options with conservative defaults.
    #[must_use]
    pub const fn new(workspace_id_hash: &'a str, observed_at: &'a str) -> Self {
        Self {
            workspace_id_hash,
            observed_at,
            near_duplicate_hamming_distance: 12,
            near_duplicate_limit: 8,
        }
    }
}

/// Produce a stable BLAKE3 hash suitable for peer-conflict memory/workspace refs.
#[must_use]
pub fn peer_conflict_hash(domain: &str, value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PEER_CONFLICT_SCHEMA_V1.as_bytes());
    hasher.update(b"\0");
    hasher.update(domain.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    let hex = hasher.finalize().to_hex().to_string();
    format!("blake3:{}", &hex[..PEER_CONFLICT_HASH_PREFIX_LEN])
}

/// Produce a stable content hash for exact peer duplicate detection.
#[must_use]
pub fn peer_conflict_content_hash(content: &str) -> String {
    peer_conflict_hash("content", &normalize_content(content))
}

/// Detect exact duplicates, near duplicates, and conservative contradictions
/// between a primary memory and peer-origin memories. Returned rows are sorted
/// deterministically and contain only hashed memory/workspace references.
#[must_use]
pub fn detect_peer_memory_conflicts(
    primary: &PeerConflictMemory<'_>,
    peers: &[PeerConflictMemory<'_>],
    options: &PeerConflictDetectionOptions<'_>,
) -> Vec<PeerConflictEvent> {
    let mut events = Vec::new();
    let mut exact_peer_hashes = BTreeSet::new();

    for peer in peers {
        if peer.memory_hash == primary.memory_hash {
            continue;
        }
        if peer.content_hash == primary.content_hash {
            exact_peer_hashes.insert(peer.memory_hash);
            events.push(peer_conflict_event(
                PeerConflictKind::DuplicateDetected,
                PeerConflictDetectorVerdict::ExactDuplicate,
                primary,
                peer,
                options,
                None,
                None,
            ));
        }
    }

    let near_duplicate_candidates = ranked_simhash_candidates(
        primary.simhash,
        peers
            .iter()
            .filter(|peer| {
                peer.memory_hash != primary.memory_hash
                    && !exact_peer_hashes.contains(peer.memory_hash)
            })
            .map(|peer| (peer.memory_hash, peer.simhash)),
        options.near_duplicate_hamming_distance,
        options.near_duplicate_limit,
    );
    for candidate in near_duplicate_candidates {
        let Some(peer) = peers
            .iter()
            .find(|peer| peer.memory_hash == candidate.candidate_id)
        else {
            continue;
        };
        events.push(peer_conflict_event(
            PeerConflictKind::NearDuplicateCandidate,
            PeerConflictDetectorVerdict::NearDuplicate,
            primary,
            peer,
            options,
            Some(PeerNearDuplicateScore {
                hamming_distance: candidate.hamming_distance,
            }),
            None,
        ));
    }

    for peer in peers {
        if peer.memory_hash == primary.memory_hash {
            continue;
        }
        let Some((signal, score)) = contradiction_signal(primary, peer) else {
            continue;
        };
        events.push(peer_conflict_event(
            PeerConflictKind::ContradictionCandidate,
            PeerConflictDetectorVerdict::Contradiction,
            primary,
            peer,
            options,
            None,
            Some(PeerContradictionScore {
                score,
                signal: signal.as_str(),
            }),
        ));
    }

    events.sort_by(compare_peer_conflict_events);
    events
}

fn peer_conflict_event(
    kind: PeerConflictKind,
    verdict: PeerConflictDetectorVerdict,
    primary: &PeerConflictMemory<'_>,
    peer: &PeerConflictMemory<'_>,
    options: &PeerConflictDetectionOptions<'_>,
    near_duplicate_score: Option<PeerNearDuplicateScore>,
    contradiction_score: Option<PeerContradictionScore>,
) -> PeerConflictEvent {
    PeerConflictEvent {
        schema: PEER_CONFLICT_SCHEMA_V1,
        side_effect_free: true,
        kind: kind.as_str(),
        ts: options.observed_at.to_owned(),
        workspace_id_hash: options.workspace_id_hash.to_owned(),
        primary_memory_hash: primary.memory_hash.to_owned(),
        peer_memory_hashes: vec![peer.memory_hash.to_owned()],
        trust_classes: vec![primary.trust_class.to_owned(), peer.trust_class.to_owned()],
        detector_verdict: verdict.as_str(),
        near_duplicate_score,
        contradiction_score,
        rendering_policy: "surface_both_with_provenance",
    }
}

fn compare_peer_conflict_events(left: &PeerConflictEvent, right: &PeerConflictEvent) -> Ordering {
    peer_conflict_kind_rank(left.kind)
        .cmp(&peer_conflict_kind_rank(right.kind))
        .then_with(|| left.primary_memory_hash.cmp(&right.primary_memory_hash))
        .then_with(|| {
            peer_near_duplicate_hamming_rank(left).cmp(&peer_near_duplicate_hamming_rank(right))
        })
        .then_with(|| left.peer_memory_hashes.cmp(&right.peer_memory_hashes))
        .then_with(|| left.detector_verdict.cmp(right.detector_verdict))
}

fn peer_conflict_kind_rank(kind: &str) -> u8 {
    match kind {
        "duplicate_detected" => 0,
        "near_duplicate_candidate" => 1,
        "contradiction_candidate" => 2,
        _ => 3,
    }
}

fn peer_near_duplicate_hamming_rank(event: &PeerConflictEvent) -> u32 {
    event
        .near_duplicate_score
        .as_ref()
        .map_or(u32::MAX, |score| score.hamming_distance)
}

fn contradiction_signal(
    primary: &PeerConflictMemory<'_>,
    peer: &PeerConflictMemory<'_>,
) -> Option<(PeerContradictionSignal, f32)> {
    if primary.supersedes_hashes.contains(&peer.memory_hash)
        || peer.supersedes_hashes.contains(&primary.memory_hash)
    {
        return Some((PeerContradictionSignal::ExplicitSupersessionChain, 0.95));
    }
    if primary.revision_token.is_some()
        && primary.revision_token == peer.revision_token
        && primary.content_hash != peer.content_hash
        && primary.trust_class != peer.trust_class
    {
        return Some((
            PeerContradictionSignal::TrustClassDisagreementAtSameRevision,
            0.8,
        ));
    }

    let primary_normalized = normalize_content(primary.content);
    let peer_normalized = normalize_content(peer.content);
    let overlap = jaccard_similarity(&primary_normalized, &peer_normalized);
    if overlap < 0.25 {
        return None;
    }
    let primary_negates = contains_negation_signal(&primary_normalized);
    let peer_negates = contains_negation_signal(&peer_normalized);
    let primary_always = contains_any_token(&primary_normalized, &["always", "must", "require"]);
    let peer_always = contains_any_token(&peer_normalized, &["always", "must", "require"]);
    let primary_never = contains_any_token(&primary_normalized, &["never", "forbid", "avoid"]);
    let peer_never = contains_any_token(&peer_normalized, &["never", "forbid", "avoid"]);

    if (primary_always && peer_never) || (peer_always && primary_never) {
        return Some((PeerContradictionSignal::RulePredicateInversion, 0.75));
    }
    if primary_negates != peer_negates && overlap >= 0.4 {
        return Some((PeerContradictionSignal::ClaimNegationOverlap, 0.6));
    }
    None
}

fn contains_negation_signal(normalized: &str) -> bool {
    contains_any_token(
        normalized,
        &["not", "no", "never", "without", "forbid", "avoid"],
    ) || normalized.contains("do not")
}

fn contains_any_token(normalized: &str, needles: &[&str]) -> bool {
    normalized
        .split_whitespace()
        .any(|token| needles.contains(&token))
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

    // Sort by severity (exact first), then by similarity score (descending).
    // `total_cmp` over `partial_cmp(...).unwrap_or(Equal)`: `similarity_score`
    // is `jaccard_similarity(...)` (always finite in [0, 1] by construction;
    // see fn body at line ~6600), so `partial_cmp` always returns
    // `Some(Ordering)` today and the `unwrap_or(Equal)` is unreachable. The
    // dedupe-check report is a `ee remember` user-facing surface that runs
    // on every remember call — same DB + same input → same warning order.
    // If a future refactor changes `jaccard_similarity` (e.g. ratio of two
    // u64 counts where the second could be zero) and lets a NaN through,
    // the bare `partial_cmp(...).unwrap_or(Equal)` would collapse the sort
    // into intransitivity and silently scramble the warning order across
    // re-runs against the same inputs. Defense-in-depth pattern shipped in
    // 4a067ecb (causalBottlenecks + hits), 9b83f9a9 (proximityHotspots),
    // 18f20375 (influence.rs), and 2eab2028 (focus_suggest.rs).
    warnings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| b.similarity_score.total_cmp(&a.similarity_score))
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
    use crate::search::simhash::simhash_128;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn cross_wire_details(error: &DomainError) -> Result<serde_json::Value, String> {
        match error {
            DomainError::UsageCodeWithDetails { details_json, .. } => {
                serde_json::from_str(details_json)
                    .map_err(|parse| format!("details_json must parse: {parse}"))
            }
            other => Err(format!("expected UsageCodeWithDetails, got {other:?}")),
        }
    }

    #[test]
    fn remember_cross_wire_guard_flags_levels_passed_as_kind() -> TestResult {
        for (provided, canonical) in [
            ("working", "working"),
            ("episodic", "episodic"),
            ("semantic", "semantic"),
            ("procedural", "procedural"),
            ("Episodic", "episodic"),
            (" semantic ", "semantic"),
        ] {
            let error = remember_level_kind_cross_wire_error("episodic", provided)
                .ok_or_else(|| format!("level token `{provided}` as kind must error"))?;
            ensure(
                error.code(),
                REMEMBER_KIND_IS_LEVEL_CODE,
                "kind-as-level code",
            )?;
            ensure(
                error.exit_code(),
                crate::models::ProcessExitCode::Usage,
                "kind-as-level exit code",
            )?;
            let message = error.message();
            ensure(
                message.contains(&format!("did you mean `--level {canonical}`")),
                true,
                "kind-as-level did-you-mean message",
            )?;
            let details = cross_wire_details(&error)?;
            ensure(
                details["failureModeCode"].as_str(),
                Some(REMEMBER_KIND_IS_LEVEL_CODE),
                "kind-as-level failureModeCode",
            )?;
            ensure(
                details["argument"].as_str(),
                Some("--kind"),
                "kind-as-level argument",
            )?;
            ensure(
                details["provided"].as_str(),
                Some(provided),
                "kind-as-level provided",
            )?;
            ensure(
                details["providedTruncated"].as_bool(),
                Some(false),
                "kind-as-level provided truncation",
            )?;
            ensure(
                details["didYouMean"]["argument"].as_str(),
                Some("--level"),
                "kind-as-level didYouMean argument",
            )?;
            ensure(
                details["didYouMean"]["value"].as_str(),
                Some(canonical),
                "kind-as-level didYouMean value",
            )?;
            ensure(
                details["canonicalKinds"].as_array().map(std::vec::Vec::len),
                Some(KNOWN_MEMORY_KINDS.len()),
                "kind-as-level canonicalKinds",
            )?;
            ensure(
                details["recovery"].as_array().map(std::vec::Vec::len),
                Some(1),
                "kind-as-level recovery",
            )?;
            ensure(
                details["recovery"][0]["flagName"].as_str(),
                Some("--level"),
                "kind-as-level recovery flag",
            )?;
            ensure(
                details["recovery"][0]["valueHint"].as_str(),
                Some(canonical),
                "kind-as-level recovery value",
            )?;
        }
        Ok(())
    }

    #[test]
    fn remember_cross_wire_guard_flags_kinds_passed_as_level() -> TestResult {
        for (provided, canonical) in [
            ("rule", "rule"),
            ("anti-pattern", "anti-pattern"),
            ("Anti_Pattern", "anti-pattern"),
            ("playbook-step", "playbook-step"),
            (" decision ", "decision"),
        ] {
            let error = remember_level_kind_cross_wire_error(provided, "fact")
                .ok_or_else(|| format!("kind token `{provided}` as level must error"))?;
            ensure(
                error.code(),
                REMEMBER_LEVEL_IS_KIND_CODE,
                "level-as-kind code",
            )?;
            let details = cross_wire_details(&error)?;
            ensure(
                details["argument"].as_str(),
                Some("--level"),
                "level-as-kind argument",
            )?;
            ensure(
                details["provided"].as_str(),
                Some(provided),
                "level-as-kind provided",
            )?;
            ensure(
                details["providedTruncated"].as_bool(),
                Some(false),
                "level-as-kind provided truncation",
            )?;
            ensure(
                details["didYouMean"]["argument"].as_str(),
                Some("--kind"),
                "level-as-kind didYouMean argument",
            )?;
            ensure(
                details["didYouMean"]["value"].as_str(),
                Some(canonical),
                "level-as-kind didYouMean value",
            )?;
            ensure(
                details["recovery"].as_array().map(std::vec::Vec::len),
                Some(1),
                "level-as-kind recovery",
            )?;
            ensure(
                details["recovery"][0]["flagName"].as_str(),
                Some("--kind"),
                "level-as-kind recovery flag",
            )?;
            ensure(
                details["recovery"][0]["valueHint"].as_str(),
                Some(canonical),
                "level-as-kind recovery value",
            )?;
        }
        Ok(())
    }

    #[test]
    fn remember_cross_wire_guard_bounds_caller_controlled_echo() -> TestResult {
        let provided = format!("episodic{}", " ".repeat(512));
        let error = remember_level_kind_cross_wire_error("episodic", &provided)
            .ok_or_else(|| "padded level token as kind must error".to_owned())?;
        let details = cross_wire_details(&error)?;
        let echoed = details["provided"]
            .as_str()
            .ok_or_else(|| "bounded provided token missing".to_owned())?;
        ensure(
            echoed.len() <= REMEMBER_CROSS_WIRE_ECHO_MAX_BYTES,
            true,
            "provided token byte cap",
        )?;
        ensure(
            echoed.is_char_boundary(echoed.len()),
            true,
            "provided token UTF-8 boundary",
        )?;
        ensure(
            details["providedTruncated"].as_bool(),
            Some(true),
            "provided truncation marker",
        )?;
        ensure(
            error.message().contains("--level episodic"),
            true,
            "bounded message retains canonical repair",
        )
    }

    #[test]
    fn remember_cross_wire_guard_accepts_custom_kinds_and_valid_pairs() -> TestResult {
        // Planted negative: a lookalike custom kind sharing a level prefix must
        // pass untouched — exact-match only, never prefix rejection.
        for (level, kind) in [
            ("episodic", "fact"),
            ("semantic", "decision"),
            ("working", "anti-pattern"),
            ("episodic", "episodic-note"),
            ("episodic", "EpisodicNote"),
            ("semantic", "episodic-note"),
            ("procedural", "workingset"),
            ("episodic", "working-set"),
            ("episodic", "proceduralish"),
            ("episodic", "rules"),
            ("episodic", "episodic_"),
        ] {
            ensure(
                remember_level_kind_cross_wire_error(level, kind).is_none(),
                true,
                &format!("({level}, {kind}) must stay accepted"),
            )?;
        }
        // Both flags cross-wired: the kind-direction guidance wins
        // deterministically.
        let error = remember_level_kind_cross_wire_error("rule", "episodic")
            .ok_or_else(|| "double cross-wire must error".to_owned())?;
        ensure(
            error.code(),
            REMEMBER_KIND_IS_LEVEL_CODE,
            "double cross-wire precedence",
        )?;
        Ok(())
    }

    #[test]
    fn remember_cross_wire_guard_precedes_store_creation_in_both_directions() -> TestResult {
        for (level, kind, expected_code) in [
            ("episodic", "semantic", REMEMBER_KIND_IS_LEVEL_CODE),
            ("rule", "fact", REMEMBER_LEVEL_IS_KIND_CODE),
        ] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let result = remember_memory(&RememberMemoryOptions {
                workspace_path: temp.path(),
                database_path: None,
                content: "Cross-wired input must fail before store creation.",
                workflow_id: None,
                level,
                kind,
                tags: None,
                confidence: 0.8,
                source: None,
                allow_secret_mention: false,
                valid_from: None,
                valid_to: None,
                dry_run: false,
                auto_link: false,
                propose_candidates: false,
            });
            match result {
                Err(error) => ensure(error.code(), expected_code, "cross-wire error code")?,
                Ok(report) => {
                    return Err(format!(
                        "cross-wired remember unexpectedly created {}",
                        report.memory_id
                    ));
                }
            }
            ensure(
                temp.path().join(".ee").exists(),
                false,
                "cross-wire rejection must not create .ee",
            )?;
        }
        Ok(())
    }

    fn remember_test_memory_input(workspace_id: &str, content: &str) -> CreateMemoryInput {
        CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    fn setup_remember_test_workspace(connection: &DbConnection) -> Result<String, String> {
        let workspace_id = "wsp_01234567890123456789012345".to_owned();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: "/tmp/remember-embed-dedup-test".to_owned(),
                    name: Some("remember embed dedup test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(workspace_id)
    }

    #[test]
    fn invalid_attempt_family_error_never_echoes_secret_shaped_identifier() -> TestResult {
        let raw_family = "AKIAIOSFODNN7EXAMPLE/invalid";
        let error = validate_remember_attempt_family(&RememberAttemptFamily {
            family_id: raw_family,
            declared_size: Some(3),
            attempt_index: Some(1),
            disposition: Some("selected"),
        })
        .expect_err("invalid family alphabet must be rejected");
        let message = error.message();
        ensure(
            message.contains(&crate::models::public_attempt_family_alias(raw_family)),
            true,
            "invalid family error exposes the safe alias",
        )?;
        ensure(
            message.contains(raw_family),
            false,
            "invalid family error omits the raw caller-controlled identifier",
        )
    }

    #[test]
    fn list_memories_prefers_populated_canonical_workspace_over_empty_lexical_alias() -> TestResult
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("campaign");
        std::fs::create_dir(&workspace).map_err(|error| error.to_string())?;
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let lexical_workspace = workspace.join("..").join("campaign");
        ensure(
            lexical_workspace != canonical_workspace,
            true,
            "lexical alias retains its parent component",
        )?;

        let database_path = temp.path().join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;

        let canonical_workspace_id = stable_workspace_id(&canonical_workspace);
        connection
            .insert_workspace(
                &canonical_workspace_id,
                &CreateWorkspaceInput {
                    path: canonical_workspace.to_string_lossy().into_owned(),
                    name: Some("canonical campaign".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_000000000000000000000alias",
                &CreateWorkspaceInput {
                    path: lexical_workspace.to_string_lossy().into_owned(),
                    name: Some("empty lexical alias".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000001",
                &remember_test_memory_input(&canonical_workspace_id, "canonical memory one"),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000002",
                &remember_test_memory_input(&canonical_workspace_id, "canonical memory two"),
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = list_memories(&ListMemoriesOptions {
            database_path: &database_path,
            workspace_path: &lexical_workspace,
            level: None,
            tag: None,
            limit: 1,
            include_tombstoned: true,
        });

        ensure(report.error, None, "list report error")?;
        ensure(report.total_count, 2, "canonical workspace memory count")?;
        ensure(report.truncated, true, "limit reports truncation")?;
        ensure(
            report.filter.include_tombstoned,
            true,
            "include-tombstoned filter",
        )?;
        ensure(report.memories.len(), 1, "limited memory count")?;
        ensure(
            report.memories[0].id.as_str(),
            "mem_00000000000000000000000001",
            "canonical workspace first memory",
        )
    }

    #[test]
    fn workspace_id_for_database_preserves_legacy_lexical_only_row() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("legacy-global-root");
        std::fs::create_dir(&workspace).map_err(|error| error.to_string())?;
        let lexical_workspace = workspace.join("..").join("legacy-global-root");
        let legacy_workspace_id = "wsp_00000000000000000000legacy";

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                legacy_workspace_id,
                &CreateWorkspaceInput {
                    path: lexical_workspace.to_string_lossy().into_owned(),
                    name: Some("legacy lexical workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        ensure(
            workspace_id_for_database(&connection, &lexical_workspace),
            legacy_workspace_id.to_owned(),
            "legacy lexical workspace fallback",
        )
    }

    fn peer_conflict_memory<'a>(
        id: &'a str,
        content: &'a str,
        trust_class: &'a str,
    ) -> PeerConflictMemory<'a> {
        let memory_hash = Box::leak(peer_conflict_hash("memory", id).into_boxed_str());
        let content_hash = Box::leak(peer_conflict_content_hash(content).into_boxed_str());
        PeerConflictMemory::new(
            memory_hash,
            content_hash,
            content,
            simhash_128(content),
            trust_class,
        )
    }

    fn peer_conflict_options() -> PeerConflictDetectionOptions<'static> {
        PeerConflictDetectionOptions::new(
            "blake3:0123456789abcdef0123456789abcdef",
            "2026-05-20T10:50:00Z",
        )
    }

    #[test]
    fn remember_git_capture_commit_transform_is_deterministic_and_redacted() -> TestResult {
        let input = RememberGitCaptureInput {
            mode: RememberGitCaptureMode::Commit,
            reference: Some("HEAD".to_owned()),
            commit_sha: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            commit_subject: Some("fix capture regression in hook installer".to_owned()),
            commit_body: Some(
                "The hook template now keeps ee capture suggestions intact.".to_owned(),
            ),
            changed_files: vec![
                "src/hooks/installer.rs".to_owned(),
                "tests/e2e_capture.rs".to_owned(),
                "src/hooks/installer.rs".to_owned(),
            ],
            diff_text: [
                "diff --git a/src/hooks/installer.rs b/src/hooks/installer.rs",
                "+++ b/src/hooks/installer.rs",
                "+pub fn session_end_capture_python() -> &'static str {",
                "+    let database_url = \"postgres://admin:SuperSecretPass123!@db.example.com/prod\";",
                "+    \"### ee capture suggestions\"",
                "+}",
            ]
            .join("\n"),
        };

        let first = build_remember_git_capture_candidate(&input);
        let second = build_remember_git_capture_candidate(&input);

        ensure(first.clone(), second, "deterministic candidate")?;
        ensure(first.schema, REMEMBER_GIT_CAPTURE_SCHEMA_V1, "schema")?;
        ensure(first.kind, "failure", "fix-shaped commit kind")?;
        ensure(first.level, "episodic", "capture level")?;
        ensure(first.source.starts_with("git-sha://012345"), true, "source")?;
        ensure(first.redacted, true, "secret redacted")?;
        ensure(
            first.content.contains("SuperSecretPass123"),
            false,
            "raw password absent",
        )?;
        ensure(
            first.content.contains("REDACTED"),
            true,
            "redaction placeholder present",
        )?;
        ensure(
            first
                .content
                .contains("ee-anchor:path:src/hooks/installer.rs"),
            true,
            "path anchor token",
        )?;
        ensure(
            first
                .content
                .contains("ee-anchor:symbol:session_end_capture_python"),
            true,
            "symbol anchor token",
        )?;
        ensure(
            first.content.contains("Diff fingerprint: blake3:"),
            true,
            "diff fingerprint present",
        )?;
        ensure(
            first.tags.contains(&"from-commit".to_owned()),
            true,
            "mode tag",
        )?;
        ensure(first.tags.contains(&"rust".to_owned()), true, "rust tag")
    }

    #[test]
    fn remember_git_capture_decision_message_suggests_decision_kind() -> TestResult {
        let input = RememberGitCaptureInput {
            mode: RememberGitCaptureMode::Diff,
            reference: Some("main".to_owned()),
            commit_sha: None,
            commit_subject: Some(
                "Decision: choose hash embeddings for capture dry runs".to_owned(),
            ),
            commit_body: Some("Rationale: deterministic tests need no model download.".to_owned()),
            changed_files: vec!["src/core/memory.rs".to_owned()],
            diff_text:
                "+pub struct RememberGitCaptureCandidate {\n+    pub schema: &'static str,\n+}"
                    .to_owned(),
        };

        let candidate = build_remember_git_capture_candidate(&input);

        ensure(candidate.kind, "decision", "decision-shaped kind")?;
        ensure(
            candidate.source.starts_with("git-sha://diff/main/"),
            true,
            "diff source",
        )?;
        ensure(
            candidate
                .content
                .contains("Decision: choose hash embeddings"),
            true,
            "message evidence retained",
        )?;
        ensure(
            candidate
                .content
                .contains("ee-anchor:symbol:RememberGitCaptureCandidate"),
            true,
            "struct symbol anchored",
        )
    }

    #[test]
    fn remember_git_capture_empty_working_tree_still_previews_without_anchors() -> TestResult {
        let input = RememberGitCaptureInput {
            mode: RememberGitCaptureMode::WorkingTree,
            reference: None,
            commit_sha: None,
            commit_subject: None,
            commit_body: None,
            changed_files: Vec::new(),
            diff_text: String::new(),
        };

        let candidate = build_remember_git_capture_candidate(&input);

        ensure(candidate.kind, "fact", "empty input kind")?;
        ensure(candidate.redacted, false, "empty input redaction")?;
        ensure(candidate.changed_files.is_empty(), true, "no files")?;
        ensure(candidate.changed_symbols.is_empty(), true, "no symbols")?;
        ensure(
            candidate
                .content
                .contains("Changed surfaces: none reported by git."),
            true,
            "empty surface note",
        )?;
        ensure(
            candidate.source.starts_with("git-sha://diff/working-tree/"),
            true,
            "working tree source",
        )
    }

    #[test]
    fn remember_git_capture_rejects_option_shaped_or_space_refs() -> TestResult {
        match validate_git_capture_ref("-bad") {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("must not start"), true, "dash ref message")?;
            }
            other => return Err(format!("expected dash ref usage error, got {other:?}")),
        }
        match validate_git_capture_ref("HEAD bad") {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("whitespace"), true, "space ref message")?;
            }
            other => return Err(format!("expected space ref usage error, got {other:?}")),
        }
        Ok(())
    }

    #[test]
    fn peer_conflict_detects_exact_content_hash_duplicates_without_raw_refs() -> TestResult {
        let primary = peer_conflict_memory(
            "local-memory-1",
            "Run cargo fmt before release.",
            "human_explicit",
        );
        let peer = peer_conflict_memory(
            "peer-memory-1",
            "Run cargo fmt before release.",
            "agent_assertion",
        );
        let events = detect_peer_memory_conflicts(&primary, &[peer], &peer_conflict_options());

        ensure(events.len(), 1, "exact duplicate event count")?;
        let event = &events[0];
        ensure(
            event.kind,
            PeerConflictKind::DuplicateDetected.as_str(),
            "exact duplicate kind",
        )?;
        ensure(
            event.detector_verdict,
            PeerConflictDetectorVerdict::ExactDuplicate.as_str(),
            "exact duplicate verdict",
        )?;
        ensure(
            event.trust_classes.clone(),
            vec!["human_explicit".to_owned(), "agent_assertion".to_owned()],
            "trust classes",
        )?;
        let json = serde_json::to_string(event).map_err(|error| error.to_string())?;
        for forbidden in [
            "local-memory-1",
            "peer-memory-1",
            "Run cargo fmt before release",
        ] {
            if json.contains(forbidden) {
                return Err(format!("peer conflict event leaked raw value: {forbidden}"));
            }
        }
        Ok(())
    }

    #[test]
    fn peer_conflict_near_duplicate_order_is_stable() -> TestResult {
        let primary = peer_conflict_memory(
            "local-memory-1",
            "Run cargo fmt before release.",
            "human_explicit",
        );
        let peer_a = peer_conflict_memory(
            "peer-memory-a",
            "Run cargo fmt before every release.",
            "agent_assertion",
        );
        let peer_b = peer_conflict_memory(
            "peer-memory-b",
            "Run cargo clippy before release.",
            "agent_validated",
        );
        let mut options = peer_conflict_options();
        options.near_duplicate_hamming_distance = 128;

        let first =
            detect_peer_memory_conflicts(&primary, &[peer_b.clone(), peer_a.clone()], &options);
        let second = detect_peer_memory_conflicts(&primary, &[peer_a, peer_b], &options);
        let first_near = first
            .iter()
            .filter(|event| event.kind == PeerConflictKind::NearDuplicateCandidate.as_str())
            .cloned()
            .collect::<Vec<_>>();
        let second_near = second
            .iter()
            .filter(|event| event.kind == PeerConflictKind::NearDuplicateCandidate.as_str())
            .cloned()
            .collect::<Vec<_>>();

        ensure(first_near.len(), 2, "near duplicate event count")?;
        ensure(
            first_near.clone(),
            second_near,
            "near duplicate ordering must not depend on peer input order",
        )?;
        let distances = first_near
            .iter()
            .map(|event| {
                event
                    .near_duplicate_score
                    .as_ref()
                    .map(|score| score.hamming_distance)
                    .ok_or_else(|| "near duplicate score missing".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut sorted_distances = distances.clone();
        sorted_distances.sort_unstable();
        ensure(distances, sorted_distances, "near duplicate hamming order")
    }

    #[test]
    fn peer_conflict_scores_rule_predicate_inversions() -> TestResult {
        let primary = peer_conflict_memory(
            "local-memory-1",
            "Always run cargo fmt before release.",
            "agent_validated",
        );
        let peer = peer_conflict_memory(
            "peer-memory-1",
            "Never run cargo fmt before release.",
            "agent_assertion",
        );
        let events = detect_peer_memory_conflicts(&primary, &[peer], &peer_conflict_options());
        let contradiction = events
            .iter()
            .find(|event| event.kind == PeerConflictKind::ContradictionCandidate.as_str())
            .ok_or_else(|| "expected contradiction candidate event".to_string())?;
        let score = contradiction
            .contradiction_score
            .as_ref()
            .ok_or_else(|| "contradiction score missing".to_string())?;

        ensure(
            contradiction.detector_verdict,
            PeerConflictDetectorVerdict::Contradiction.as_str(),
            "contradiction verdict",
        )?;
        ensure(
            score.signal,
            PeerContradictionSignal::RulePredicateInversion.as_str(),
            "contradiction signal",
        )?;
        if score.score < 0.7 {
            return Err(format!("contradiction score too low: {}", score.score));
        }
        Ok(())
    }

    #[test]
    fn curation_member_memory_ids_use_radix_payload_order_and_dedup() -> TestResult {
        let lower = MemoryId::from_uuid(uuid::Uuid::from_u128(7100)).to_string();
        let higher = MemoryId::from_uuid(uuid::Uuid::from_u128(7200)).to_string();
        let mut memory_ids = vec![higher.clone(), lower.clone(), higher.clone()];

        sort_and_dedup_memory_ids_by_ulid_payload(&mut memory_ids);

        ensure(memory_ids, vec![lower, higher], "curation member ID order")
    }

    #[test]
    fn store_remembered_memory_queues_audit_when_lane_is_enabled() -> TestResult {
        use crate::core::audit_lane::{AuditLane, AuditLaneConfig, insert_audit_event_batch};

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;

        let workspace_id = "wsp_01234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: "/tmp/audit-lane-memory-test".to_owned(),
                    name: Some("audit lane memory test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let memory_id = "mem_00000000000000000000002002";
        let audit_id = "audit_auditlane00000000000000001";
        let index_job_id = "sidx_auditlane00000000000000001";
        let memory_input = CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Route remember audit through the audit lane.".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        };
        let index_input = CreateSearchIndexJobInput {
            workspace_id: workspace_id.to_owned(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(memory_id.to_owned()),
            documents_total: 1,
        };
        let (handle, mut lane) = AuditLane::new(AuditLaneConfig {
            capacity: 4,
            batch_size: 4,
            shutdown_event_limit: 4,
        });

        store_remembered_memory_with_retry(
            &connection,
            memory_id,
            audit_id,
            index_job_id,
            &memory_input,
            None,
            None,
            &RememberEmbedDedupDecision::disabled(),
            None,
            "{}",
            &index_input,
            None,
            Some(&handle),
        )
        .map_err(|error| error.to_string())?;

        ensure(
            connection
                .get_audit(audit_id)
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "enabled lane skips direct audit insert",
        )?;
        let report = lane.drain_available(|batch| {
            insert_audit_event_batch(&connection, batch)
                .expect("drained audit events should batch insert");
        });
        ensure(report.drained_events, 1, "drained event count")?;

        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "drained audit row missing".to_owned())?;
        ensure(
            audit.action,
            audit_actions::MEMORY_CREATE.to_owned(),
            "audit action",
        )?;
        ensure(
            audit.target_id,
            Some(memory_id.to_owned()),
            "audit target id",
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_embed_dedup_decision_selects_confirmed_candidate() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = setup_remember_test_workspace(&connection)?;
        let content = "Run cargo fmt before committing Rust changes.";
        let fingerprint = crate::search::simhash::simhash_128(content).to_be_bytes();
        connection
            .insert_memory_with_content_simhash(
                "mem_embeddedupsource0000000000",
                &remember_test_memory_input(&workspace_id, content),
                fingerprint,
            )
            .map_err(|error| error.to_string())?;

        let decision = remember_embed_dedup_decision(
            &connection,
            &remember_test_memory_input(&workspace_id, content),
            EmbedDedupConfig {
                enabled: true,
                hamming_k: 0,
                cosine_floor: 0.99,
            },
        )
        .map_err(|error| error.to_string())?;

        ensure(
            decision.content_simhash,
            Some(fingerprint),
            "new memory SimHash",
        )?;
        ensure(decision.decision, "reuse", "dedup decision")?;
        ensure(
            decision.reason,
            "simhash_within_threshold_and_cosine_confirmed",
            "dedup reason",
        )?;
        let link = decision
            .link
            .ok_or_else(|| "confirmed candidate must produce a dedup link".to_owned())?;
        ensure(
            link.target_memory_id,
            "mem_embeddedupsource0000000000".to_owned(),
            "dedup target",
        )?;
        ensure(link.hamming_distance, 0_u32, "dedup hamming distance")?;
        if link.cosine_similarity < 0.99 {
            return Err(format!(
                "expected cosine confirmation >= 0.99, got {}",
                link.cosine_similarity
            ));
        }

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_embed_dedup_requeries_after_serialized_writer_progress() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = setup_remember_test_workspace(&connection)?;
        let content = "Serialize identical remembers before final duplicate selection.";
        let memory_input = remember_test_memory_input(&workspace_id, content);
        let config = EmbedDedupConfig {
            enabled: true,
            hamming_k: 0,
            cosine_floor: 0.99,
        };

        // Model the adversarial schedule precisely: both writers finish their
        // immutable fingerprint/vector work before either obtains the
        // workspace lock. The first then commits while the second waits.
        let first_probe = remember_embed_dedup_probe(&memory_input, config);
        let second_probe = remember_embed_dedup_probe(&memory_input, config);
        let first_decision =
            remember_embed_dedup_decision_from_probe(&connection, &memory_input, &first_probe)
                .map_err(|error| error.to_string())?;
        ensure(
            first_decision.decision,
            "new_embed",
            "first writer decision",
        )?;
        let first_simhash = first_decision
            .content_simhash
            .ok_or_else(|| "enabled first writer omitted content SimHash".to_owned())?;
        connection
            .insert_memory_with_content_simhash(
                "mem_serializeddedupfirst0000000",
                &memory_input,
                first_simhash,
            )
            .map_err(|error| error.to_string())?;

        // Candidate selection happens only after the second writer enters the
        // serialized lane, so it must observe the predecessor rather than
        // replaying a stale pre-lock decision.
        let second_decision =
            remember_embed_dedup_decision_from_probe(&connection, &memory_input, &second_probe)
                .map_err(|error| error.to_string())?;
        ensure(second_decision.decision, "reuse", "second writer decision")?;
        ensure(
            second_decision
                .link
                .as_ref()
                .map(|link| link.target_memory_id.as_str()),
            Some("mem_serializeddedupfirst0000000"),
            "second writer links its serialized predecessor",
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_near_duplicates_surface_confirmed_embed_dedup_link() -> TestResult {
        let decision = RememberEmbedDedupDecision::reused(
            [2_u8; 16],
            RememberEmbedDedupLink {
                target_memory_id: "mem_existingduplicate000000000".to_owned(),
                hamming_distance: 4,
                cosine_similarity: 0.981,
                cosine_floor: 0.97,
            },
        );

        let near_duplicates = remember_near_duplicates_from_embed_dedup_decision(&decision);

        ensure(near_duplicates.len(), 1_usize, "near duplicate count")?;
        let duplicate = &near_duplicates[0];
        ensure(
            duplicate.memory_id.as_str(),
            "mem_existingduplicate000000000",
            "near duplicate memory id",
        )?;
        ensure(duplicate.similarity, 0.981_f32, "near duplicate similarity")?;
        ensure(duplicate.threshold, 0.97_f32, "near duplicate threshold")?;
        ensure(
            duplicate.hamming_distance,
            4_u32,
            "near duplicate hamming distance",
        )?;
        ensure(
            duplicate.source.as_str(),
            "embedding_reuse",
            "near duplicate source",
        )?;
        ensure(
            duplicate
                .next_actions
                .iter()
                .any(|action| action.contains("remember --reinforce")),
            true,
            "near duplicate reinforce action",
        )
    }

    #[test]
    fn remember_near_duplicates_empty_without_confirmed_embed_dedup_link() {
        assert!(
            remember_near_duplicates_from_embed_dedup_decision(&RememberEmbedDedupDecision::fresh(
                [1_u8; 16],
                "cosine_under_floor"
            ))
            .is_empty()
        );
        assert!(
            remember_near_duplicates_from_embed_dedup_decision(
                &RememberEmbedDedupDecision::disabled()
            )
            .is_empty()
        );
    }

    #[test]
    fn store_remembered_memory_persists_simhash_and_dedup_link() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = setup_remember_test_workspace(&connection)?;
        let target_id = "mem_embeddeduptarget0000000000";
        connection
            .insert_memory_with_content_simhash(
                target_id,
                &remember_test_memory_input(&workspace_id, "Prefer remote RCH verification."),
                [1_u8; 16],
            )
            .map_err(|error| error.to_string())?;

        let memory_id = "mem_embeddedupnew0000000000000";
        let audit_id = "audit_embeddedupnew0000000000001";
        let index_job_id = "sidx_embeddedupnew0000000000000";
        let memory_input =
            remember_test_memory_input(&workspace_id, "Prefer remote RCH verification.");
        let index_input = CreateSearchIndexJobInput {
            workspace_id: workspace_id.clone(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(memory_id.to_owned()),
            documents_total: 1,
        };
        let decision = RememberEmbedDedupDecision::reused(
            [2_u8; 16],
            RememberEmbedDedupLink {
                target_memory_id: target_id.to_owned(),
                hamming_distance: 3,
                cosine_similarity: 0.992,
                cosine_floor: 0.97,
            },
        );

        store_remembered_memory_with_retry(
            &connection,
            memory_id,
            audit_id,
            index_job_id,
            &memory_input,
            None,
            None,
            &decision,
            Some("link_embeddedupnew0000000000000"),
            "{}",
            &index_input,
            None,
            None,
        )
        .map_err(|error| error.to_string())?;

        let candidates = connection
            .list_memory_simhash_candidates(&workspace_id, [2_u8; 16], 0, 10)
            .map_err(|error| error.to_string())?;
        ensure(
            candidates
                .iter()
                .map(|candidate| candidate.memory_id.as_str())
                .collect::<Vec<_>>(),
            vec![memory_id],
            "persisted SimHash candidate",
        )?;
        let links = connection
            .list_memory_links_for_memory(memory_id, Some(MemoryLinkRelation::Related))
            .map_err(|error| error.to_string())?;
        ensure(links.len(), 1_usize, "one dedup link")?;
        let link = &links[0];
        ensure(link.dst_memory_id.as_str(), target_id, "dedup link target")?;
        ensure(
            link.source.as_str(),
            MemoryLinkSource::Auto.as_str(),
            "dedup link source",
        )?;
        let metadata: serde_json::Value = serde_json::from_str(
            link.metadata_json
                .as_deref()
                .ok_or_else(|| "dedup link metadata missing".to_owned())?,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            metadata
                .get("relationship")
                .and_then(serde_json::Value::as_str),
            Some("embedding_reuse"),
            "dedup relationship metadata",
        )?;

        connection.close().map_err(|error| error.to_string())
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

        let manual = assess_memory_evidence_freshness(
            &freshness_memory(
                "Freshness source release evidence line",
                Some("manual://lived-audit/2026-06-02".to_owned()),
            ),
            Some(temp.path()),
        );
        ensure(
            manual.status,
            EvidenceFreshnessStatus::UnsupportedSource,
            "manual source is accepted but not file-freshness-checkable",
        )?;

        let unknown =
            assess_memory_evidence_freshness(&freshness_memory("No explicit source.", None), None);
        ensure(unknown.status, EvidenceFreshnessStatus::Unknown, "unknown")
    }

    #[test]
    fn assess_memory_evidence_freshness_cache_reuses_file_reads() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(
            temp.path().join("source.md"),
            "first cached evidence line\nsecond cached evidence line\n",
        )
        .map_err(|error| error.to_string())?;

        let mut cache = EvidenceFreshnessFileCache::default();
        let first = assess_memory_evidence_freshness_with_cache(
            &freshness_memory(
                "first cached evidence line",
                Some("file://source.md#L1".to_owned()),
            ),
            Some(temp.path()),
            &mut cache,
        );
        let second = assess_memory_evidence_freshness_with_cache(
            &freshness_memory(
                "second cached evidence line",
                Some("file://source.md#L2".to_owned()),
            ),
            Some(temp.path()),
            &mut cache,
        );

        ensure(first.status, EvidenceFreshnessStatus::Fresh, "first span")?;
        ensure(second.status, EvidenceFreshnessStatus::Fresh, "second span")?;
        ensure(cache.cached_file_count(), 1_usize, "cached file count")
    }

    #[cfg(unix)]
    #[test]
    fn assess_memory_evidence_freshness_rejects_symlinked_file_source() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_source = temp.path().join("outside.md");
        let linked_source = temp.path().join("linked.md");
        std::fs::write(&outside_source, "Freshness source release evidence line\n")
            .map_err(|error| error.to_string())?;
        symlink(&outside_source, &linked_source).map_err(|error| error.to_string())?;

        let freshness = assess_memory_evidence_freshness(
            &freshness_memory(
                "Freshness source release evidence line",
                Some("file://linked.md".to_owned()),
            ),
            Some(temp.path()),
        );

        ensure(
            freshness.status,
            EvidenceFreshnessStatus::UnreachableSource,
            "symlinked file source status",
        )?;
        if freshness.detail.contains("symlinked path component") {
            Ok(())
        } else {
            Err(format!(
                "unexpected symlinked freshness detail: {}",
                freshness.detail
            ))
        }
    }

    /// Regression guard for the bounded-read defense in
    /// `read_provenance_file_text`.
    ///
    /// Pre-fix, the helper checked `metadata.len() > MAX_PROVENANCE_FILE_BYTES`
    /// and then handed the path to `fs::read_to_string`. That second
    /// call pre-sizes its destination `String` from the file's
    /// metadata length on every supported platform and ignores the
    /// downstream UTF-8 size cap — so a peer that grew the file
    /// between the metadata stat and the read would defeat the cap
    /// and pin a matching multi-GiB allocation on every
    /// `ee remember` / `ee why <id>` evidence-freshness invocation
    /// in a shared swarm checkout. The fix bounds the read at
    /// `MAX_PROVENANCE_FILE_BYTES + 1` via `file.take(...)` so peak
    /// allocation is proportional to the cap regardless of on-disk
    /// growth, and surfaces the refusal as
    /// `UnreachableSource` at the freshness-check layer instead of
    /// materializing the oversized payload. Same defensive pattern
    /// as the parallel guards in handoff.rs (6d8d00e5),
    /// preflight_guard.rs (7f56d89b), and workspace.rs (ed0f69f8).
    #[test]
    fn read_provenance_file_refuses_oversize_payload() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_path = temp.path().join("oversize.md");
        // CAP + 1 bytes of valid UTF-8 filler. Filling the whole
        // buffer past the cap is what asserts the bounded read
        // didn't silently truncate at exactly CAP and accept the
        // result as a normal "freshness source".
        let cap = usize::try_from(MAX_PROVENANCE_FILE_BYTES).map_err(|error| error.to_string())?;
        let payload = vec![b' '; cap + 1];
        std::fs::write(&source_path, &payload).map_err(|error| error.to_string())?;

        let direct = read_provenance_file_text(&source_path)
            .expect_err("oversized provenance file must be refused before allocation");
        assert!(
            direct.contains(&MAX_PROVENANCE_FILE_BYTES.to_string()),
            "rejection must cite the cap; got {direct:?}",
        );
        // The freshness-check wrapper must propagate the refusal as
        // UnreachableSource (not Fresh, not ChangedSource — both
        // would imply the body was successfully materialized).
        let freshness = assess_memory_evidence_freshness(
            &freshness_memory(
                "freshness source release evidence line",
                Some(format!(
                    "file://{}",
                    source_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("oversize.md")
                )),
            ),
            Some(temp.path()),
        );
        ensure(
            freshness.status,
            EvidenceFreshnessStatus::UnreachableSource,
            "oversized provenance source status",
        )
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
    fn remember_cross_wire_rejection_does_not_consume_seeded_memory_id() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut guarded = Deterministic::from_seed(12_344);
        let result = remember_memory_seeded(
            &RememberMemoryOptions {
                workspace_path: temp.path(),
                database_path: None,
                content: "A rejected cross-wire must not advance deterministic state.",
                workflow_id: None,
                level: "episodic",
                kind: "semantic",
                tags: None,
                confidence: 0.8,
                source: None,
                allow_secret_mention: false,
                valid_from: None,
                valid_to: None,
                dry_run: false,
                auto_link: false,
                propose_candidates: false,
            },
            &mut guarded,
        );
        match result {
            Err(error) => ensure(
                error.code(),
                REMEMBER_KIND_IS_LEVEL_CODE,
                "seeded cross-wire error code",
            )?,
            Ok(report) => {
                return Err(format!(
                    "cross-wired seeded remember unexpectedly created {}",
                    report.memory_id
                ));
            }
        }

        let guarded_next = MemoryId::now_seeded(&mut guarded);
        let mut pristine = Deterministic::from_seed(12_344);
        let pristine_next = MemoryId::now_seeded(&mut pristine);
        ensure(
            guarded_next,
            pristine_next,
            "rejected seeded remember leaves the ID stream untouched",
        )
    }

    #[test]
    fn remember_memory_seeded_replays_memory_audit_and_index_ids() -> TestResult {
        fn run_seeded_remember(
            seed: u64,
        ) -> Result<(String, Option<String>, Option<String>), String> {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
            let mut determinism = Deterministic::from_seed(seed);
            let report = remember_memory_seeded(
                &RememberMemoryOptions {
                    workspace_path: temp.path(),
                    database_path: None,
                    content: "Run cargo fmt --check before release.",
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
                    auto_link: false,
                    propose_candidates: false,
                },
                &mut determinism,
            )
            .map_err(|error| error.message())?;

            assert!(report.persisted);
            assert!(report.memory_id.to_string().starts_with("mem_"));
            assert!(
                report
                    .audit_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("audit_"))
            );
            assert!(
                report
                    .index_job_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("sidx_"))
            );
            Ok((
                report.memory_id.to_string(),
                report.audit_id,
                report.index_job_id,
            ))
        }

        let first = run_seeded_remember(12_345)?;
        let replay = run_seeded_remember(12_345)?;
        let other_seed = run_seeded_remember(12_346)?;

        assert_eq!(first, replay);
        assert_ne!(first, other_seed);
        Ok(())
    }

    fn enable_revision_dominance(workspace: &Path) -> TestResult {
        std::fs::write(
            workspace.join(".ee").join("config.toml"),
            "[graph.feature.revision_dominance]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())
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

    fn denied_memory_link_metadata() -> String {
        serde_json::json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_memory_link_denied",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_memory_link_decision_denied",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }

    #[test]
    fn memory_link_list_ignores_denied_mesh_links() -> TestResult {
        let (temp, source) = remember_revisable_memory("Memory link source.")?;
        let allowed = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: Some(&source.database_path),
            content: "Allowed memory link target.",
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
            auto_link: false,
            propose_candidates: false,
        })
        .map_err(|error| error.message())?;
        let denied = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: Some(&source.database_path),
            content: "Denied mesh memory link target.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: Some("links"),
            confidence: 0.8,
            source: Some("file://README.md#L81-83"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: false,
            propose_candidates: false,
        })
        .map_err(|error| error.message())?;
        let source_id = source.memory_id.to_string();
        let allowed_id = allowed.memory_id.to_string();
        let denied_id = denied.memory_id.to_string();

        let allowed_link = update_memory_link(&MemoryLinkOptions {
            workspace_path: temp.path(),
            database_path: &source.database_path,
            memory_id: &source_id,
            mode: MemoryLinkMode::Create {
                target_memory_id: allowed_id.clone(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.75,
                confidence: 0.9,
                directed: true,
                evidence_count: 2,
                source: MemoryLinkSource::Human,
                metadata_json: None,
            },
            actor: Some("test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;
        let allowed_link_id = allowed_link
            .link
            .and_then(|link| link.link_id)
            .ok_or_else(|| "allowed link id missing".to_string())?;

        let connection = crate::db::DbConnection::open_file(&source.database_path)
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_link(
                "link_00000000000000000000000201",
                &CreateMemoryLinkInput {
                    src_memory_id: source_id.clone(),
                    dst_memory_id: denied_id.clone(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 0.9,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Import,
                    created_by: Some("memory-link-mesh-filter-test".to_owned()),
                    metadata_json: Some(denied_memory_link_metadata()),
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let listed = update_memory_link(&MemoryLinkOptions {
            workspace_path: temp.path(),
            database_path: &source.database_path,
            memory_id: &source_id,
            mode: MemoryLinkMode::List { relation: None },
            actor: None,
            dry_run: false,
            include_tombstoned: false,
        })
        .map_err(|error| error.message())?;

        ensure(listed.status, "listed".to_owned(), "list status")?;
        ensure(listed.links.len(), 1, "visible link count")?;
        ensure(
            listed.links[0].link_id.clone(),
            Some(allowed_link_id),
            "allowed link remains",
        )?;
        ensure(
            listed
                .links
                .iter()
                .any(|link| link.target_memory_id == denied_id),
            false,
            "denied mesh link target absent",
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
    fn remember_memory_populates_typed_fields_sidecar_from_body() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Tried page-level cache prefetch. Result: -8% on small-N reads. Reverted at SHA 9af3c21. Family: aggressive prefetch, third failure in this family. Cause: cache pollution. Regression surface: small-N reads.",
            workflow_id: None,
            level: "episodic",
            kind: "failure",
            tags: Some("negative-evidence,prefetch"),
            confidence: 0.8,
            source: Some("file://README.md#L100"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: false,
            propose_candidates: false,
        })
        .map_err(|error| error.message())?;

        let connection = crate::db::DbConnection::open_file(&report.database_path)
            .map_err(|error| error.to_string())?;
        let typed_fields = connection
            .get_memory_typed_fields_json(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "typed fields sidecar missing".to_owned())?;
        let parsed: serde_json::Value =
            serde_json::from_str(&typed_fields).map_err(|error| error.to_string())?;

        ensure(
            parsed["schema"].as_str(),
            Some(crate::models::memory::TYPED_MEMORY_FIELDS_SCHEMA_V2),
            "typed fields schema",
        )?;
        ensure(
            parsed["kind"].as_str(),
            Some("failure"),
            "typed fields kind",
        )?;
        ensure(
            parsed["fields"]["cause"].as_str(),
            Some("cache pollution"),
            "typed cause",
        )?;
        ensure(
            parsed["fields"]["family"].as_str(),
            Some("aggressive prefetch"),
            "typed family",
        )?;
        ensure(
            parsed["fields"]["regression_surface"].as_str(),
            Some("small-N reads"),
            "typed regression surface",
        )?;
        ensure(
            parsed["fields"]["reverted_at_sha"].as_str(),
            Some("9af3c21"),
            "typed reverted SHA",
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_memory_persists_explicit_typed_fields_and_reports_them() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
        let options = RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "The remote verification lane won the storage decision.",
            workflow_id: None,
            level: "semantic",
            kind: "decision",
            tags: Some("typed-field,decision"),
            confidence: 0.9,
            source: Some("manual://remember/explicit-typed-fields"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: false,
            propose_candidates: false,
        };
        let assignments = vec![
            "chosen=RCH remote".to_owned(),
            "options=local Cargo".to_owned(),
            "options=RCH remote".to_owned(),
            "rationale=avoid local build artifacts".to_owned(),
        ];
        let report = match remember_memory_with_controls_and_typed_fields(
            &options,
            &RememberWriteControls::default(),
            &assignments,
        )
        .map_err(|error| error.message())?
        {
            RememberOutcome::Created(report) => report,
            other => return Err(format!("expected created outcome, got {other:?}")),
        };

        let reported = report
            .typed_fields
            .as_ref()
            .ok_or_else(|| "remember report omitted typed fields".to_owned())?;
        ensure(
            reported["chosen"].as_str(),
            Some("RCH remote"),
            "reported chosen field",
        )?;
        ensure(
            reported["options"].as_array().map(Vec::len),
            Some(2),
            "reported options",
        )?;

        let connection = crate::db::DbConnection::open_file(&report.database_path)
            .map_err(|error| error.to_string())?;
        let stored = connection
            .get_memory_typed_fields_json(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stored typed fields missing".to_owned())?;
        let stored: serde_json::Value =
            serde_json::from_str(&stored).map_err(|error| error.to_string())?;
        ensure(
            stored["fields"]["chosen"].as_str(),
            Some("RCH remote"),
            "stored chosen field",
        )?;
        connection.close().map_err(|error| error.to_string())
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
    fn remember_memory_persists_high_confidence_cotag_links_and_keeps_weak_suggestions()
    -> TestResult {
        // bd-pp1fk: a strong co-tag neighbor (>= 50% tag overlap, score >= 0.75)
        // is now persisted as a durable, audited `related` link so the graph
        // wakes up from ordinary tagged remembers. A weak neighbor (one shared
        // tag out of three => score 0.683) stays an advisory suggestion.
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        // High-overlap neighbor: shares {alpha, beta} with `third` (2/3 tags).
        let high = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Release checks include cargo fmt.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("alpha,beta,extra"),
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
        // Weak neighbor: shares only {gamma} with `third` (1/3 tags), and
        // shares nothing with `high`, so its own remember creates no link.
        let weak = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Release docs mention supported targets.",
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: Some("gamma,docs,targets"),
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
            tags: Some("alpha,beta,gamma"),
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

        // The strong neighbor became a durable, audited co-tag link.
        ensure(
            third.auto_link_status,
            "linked".to_string(),
            "auto-link status",
        )?;
        ensure(third.auto_links.len(), 1, "co-tag auto-link count")?;
        let cotag = third
            .auto_links
            .first()
            .ok_or_else(|| "co-tag auto-link missing".to_string())?;
        ensure(
            cotag.target_memory_id.clone(),
            high.memory_id.to_string(),
            "co-tag link targets the high-overlap neighbor",
        )?;
        ensure(cotag.relation.clone(), "related".to_string(), "relation")?;
        ensure(cotag.source.clone(), "auto".to_string(), "source")?;
        ensure(cotag.weight, REMEMBER_AUTO_COTAG_LINK_WEIGHT, "weight")?;
        ensure(
            cotag.audit_id.is_empty(),
            false,
            "co-tag link must carry an audit id (no silent mutation)",
        )?;

        // The weak neighbor stays an advisory suggestion, never auto-linked.
        ensure(third.suggested_links.len(), 1, "weak suggestion retained")?;
        ensure(
            third.suggested_links[0].target_memory_id.clone(),
            weak.memory_id.to_string(),
            "weak neighbor is the surviving suggestion",
        )?;

        // Exactly one durable link exists, and it is audited.
        let connection = crate::db::DbConnection::open_file(&third.database_path)
            .map_err(|error| error.to_string())?;
        let links = connection
            .list_all_memory_links(None)
            .map_err(|error| error.to_string())?;
        ensure(links.len(), 1, "exactly one durable co-tag link")?;
        let stored = links
            .first()
            .ok_or_else(|| "stored co-tag link missing".to_string())?;
        ensure(stored.id.clone(), cotag.link_id.clone(), "stored link id")?;
        Ok(())
    }

    #[test]
    fn remember_memory_no_auto_link_suppresses_cotag_links() -> TestResult {
        // bd-pp1fk: the existing --no-auto-link (auto_link=false) toggle must
        // suppress co-tag auto-links too, leaving strong neighbors as advisory
        // suggestions and the graph empty.
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let _first = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Release checks include cargo fmt.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("alpha,beta"),
            confidence: 0.9,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: false,
            propose_candidates: true,
        })
        .map_err(|error| error.message())?;
        let second = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Before release, run checks and record evidence.",
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some("alpha,beta"),
            confidence: 0.85,
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
            second.auto_links.is_empty(),
            true,
            "no auto-links when disabled",
        )?;
        ensure(
            second.suggested_links.is_empty(),
            false,
            "strong neighbor still surfaced as a suggestion",
        )?;

        let connection = crate::db::DbConnection::open_file(&second.database_path)
            .map_err(|error| error.to_string())?;
        let links = connection
            .list_all_memory_links(None)
            .map_err(|error| error.to_string())?;
        ensure(
            links.is_empty(),
            true,
            "no durable links when auto-link disabled",
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

    fn timeline_test_memory_input(
        workspace_id: &str,
        kind: &str,
        content: &str,
        valid_from: &str,
        valid_to: Option<&str>,
        tags: &[&str],
    ) -> CreateMemoryInput {
        CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "procedural".to_owned(),
            kind: kind.to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.86,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some(format!("file:///timeline/{kind}.md:1")),
            trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
            trust_subclass: None,
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            valid_from: Some(valid_from.to_owned()),
            valid_to: valid_to.map(str::to_owned),
        }
    }

    fn setup_timeline_fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = temp.path().join("workspace");
        fs::create_dir_all(workspace_path.join(".ee")).map_err(|error| error.to_string())?;
        let canonical = workspace_path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let database_path = canonical.join(".ee").join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = stable_workspace_id(&canonical);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: canonical.to_string_lossy().into_owned(),
                    name: Some("timeline fixture".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000101",
                &timeline_test_memory_input(
                    &workspace_id,
                    "rule",
                    "Timeline audit policy: use RCH before release.",
                    "2026-05-01T00:00:00Z",
                    Some("2026-05-03T00:00:00Z"),
                    &["timeline", "audit"],
                ),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000102",
                &timeline_test_memory_input(
                    &workspace_id,
                    "rule",
                    "Timeline audit policy: central batch verify owns release proof.",
                    "2026-05-03T00:00:00Z",
                    None,
                    &["timeline", "audit"],
                ),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000103",
                &timeline_test_memory_input(
                    &workspace_id,
                    "decision",
                    "Decision: timeline audit uses memory validity windows.",
                    "2026-05-02T00:00:00Z",
                    None,
                    &["timeline", "decision"],
                ),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000104",
                &timeline_test_memory_input(
                    &workspace_id,
                    "fact",
                    "Timeline audit temporary workaround.",
                    "2026-05-01T00:00:00Z",
                    None,
                    &["timeline", "temporary"],
                ),
            )
            .map_err(|error| error.to_string())?;
        connection
            .restore_imported_memory_tombstone(
                "mem_00000000000000000000000104",
                "2026-05-04T00:00:00Z",
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000105",
                &timeline_test_memory_input(
                    &workspace_id,
                    "fact",
                    "Garden notes are unrelated to release audits.",
                    "2026-05-01T00:00:00Z",
                    None,
                    &["garden"],
                ),
            )
            .map_err(|error| error.to_string())?;

        Ok((temp, database_path, canonical))
    }

    #[test]
    fn memory_timeline_reconstructs_as_of_state_and_changes_since() -> TestResult {
        let (_temp, database_path, workspace_path) = setup_timeline_fixture()?;

        let report = build_memory_timeline(&MemoryTimelineOptions {
            database_path: &database_path,
            workspace_path: &workspace_path,
            topic: "timeline audit",
            as_of: "2026-05-02T12:00:00Z",
            limit: 20,
        })
        .map_err(|error| error.message())?;

        ensure(report.schema, TIMELINE_SCHEMA_V1, "schema")?;
        ensure(report.as_of.as_str(), "2026-05-02T12:00:00Z", "as_of")?;
        ensure(report.total_memories_then, 3, "then count")?;
        ensure(report.total_changes_since, 3, "changes count")?;
        ensure(report.total_decisions_in_effect, 1, "decision count")?;
        ensure(report.truncated, false, "not truncated")?;
        ensure(
            report
                .memories_then
                .iter()
                .any(|memory| memory.memory_id == "mem_00000000000000000000000101"),
            true,
            "old policy visible then",
        )?;
        ensure(
            report
                .memories_then
                .iter()
                .any(|memory| memory.memory_id == "mem_00000000000000000000000104"),
            true,
            "later tombstone still visible then",
        )?;
        ensure(
            report
                .decisions_in_effect
                .iter()
                .any(|memory| memory.memory_id == "mem_00000000000000000000000103"),
            true,
            "decision in effect",
        )?;
        ensure(
            report.changes_since.iter().any(|change| {
                change.memory_id == "mem_00000000000000000000000102"
                    && change.change_type == "added"
                    && change.changed_at == "2026-05-03T00:00:00Z"
            }),
            true,
            "new policy added since",
        )?;
        ensure(
            report.changes_since.iter().any(|change| {
                change.memory_id == "mem_00000000000000000000000101"
                    && change.change_type == "superseded"
            }),
            true,
            "old policy superseded since",
        )?;
        ensure(
            report.changes_since.iter().any(|change| {
                change.memory_id == "mem_00000000000000000000000104"
                    && change.change_type == "tombstoned"
            }),
            true,
            "later tombstone reported",
        )
    }

    #[test]
    fn memory_timeline_valid_to_boundary_switches_current_revision() -> TestResult {
        let (_temp, database_path, workspace_path) = setup_timeline_fixture()?;

        let report = build_memory_timeline(&MemoryTimelineOptions {
            database_path: &database_path,
            workspace_path: &workspace_path,
            topic: "timeline audit policy",
            as_of: "2026-05-03T00:00:00Z",
            limit: 20,
        })
        .map_err(|error| error.message())?;

        ensure(
            report
                .memories_then
                .iter()
                .any(|memory| memory.memory_id == "mem_00000000000000000000000101"),
            false,
            "valid_to boundary excludes old policy",
        )?;
        ensure(
            report
                .memories_then
                .iter()
                .any(|memory| memory.memory_id == "mem_00000000000000000000000102"),
            true,
            "valid_from boundary includes new policy",
        )
    }

    #[test]
    fn memory_timeline_zero_limit_keeps_totals_and_marks_truncated() -> TestResult {
        let (_temp, database_path, workspace_path) = setup_timeline_fixture()?;

        let report = build_memory_timeline(&MemoryTimelineOptions {
            database_path: &database_path,
            workspace_path: &workspace_path,
            topic: "timeline audit",
            as_of: "2026-05-02T12:00:00Z",
            limit: 0,
        })
        .map_err(|error| error.message())?;

        ensure(report.total_memories_then, 3, "total memories retained")?;
        ensure(report.memories_then.is_empty(), true, "memory page empty")?;
        ensure(report.changes_since.is_empty(), true, "change page empty")?;
        ensure(
            report.decisions_in_effect.is_empty(),
            true,
            "decision page empty",
        )?;
        ensure(report.truncated, true, "zero limit truncates")
    }

    #[test]
    fn memory_timeline_empty_topic_fails_closed() -> TestResult {
        let (_temp, database_path, workspace_path) = setup_timeline_fixture()?;

        match build_memory_timeline(&MemoryTimelineOptions {
            database_path: &database_path,
            workspace_path: &workspace_path,
            topic: "   ",
            as_of: "2026-05-02T12:00:00Z",
            limit: 10,
        }) {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("topic"), true, "mentions topic")
            }
            other => Err(format!("expected topic usage error, got {other:?}")),
        }
    }

    #[test]
    fn memory_timeline_invalid_as_of_fails_closed() -> TestResult {
        let (_temp, database_path, workspace_path) = setup_timeline_fixture()?;

        match build_memory_timeline(&MemoryTimelineOptions {
            database_path: &database_path,
            workspace_path: &workspace_path,
            topic: "timeline audit",
            as_of: "not-a-timestamp",
            limit: 10,
        }) {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("--as-of"), true, "mentions as-of")
            }
            other => Err(format!("expected as-of usage error, got {other:?}")),
        }
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
    fn revise_memory_non_dry_run_persists_new_revision() -> TestResult {
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

        ensure(report.success, true, "success")?;
        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.original_id, memory_id.clone(), "original id")?;
        let new_id = report
            .new_id
            .as_deref()
            .ok_or_else(|| "revise should report new memory id".to_string())?;
        ensure(new_id != memory_id, true, "new memory id differs")?;
        ensure(
            report.revision_group_id.as_deref(),
            Some(memory_id.as_str()),
            "revision group",
        )?;
        ensure(report.revision_number, Some(2), "revision number")?;
        ensure(
            report.changed_fields,
            vec!["content".to_string()],
            "changed fields",
        )?;
        ensure(report.error.is_none(), true, "no revision error")?;

        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let original = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "created memory should still exist".to_string())?;
        ensure(
            original.valid_to.is_some(),
            true,
            "original row was superseded",
        )?;
        let revised = connection
            .get_memory(new_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "new revision should exist".to_string())?;
        ensure(
            revised.content,
            "Store release checks and clippy gates as durable memory.".to_string(),
            "revised content",
        )?;
        ensure(
            revised.valid_to.is_none(),
            true,
            "new revision remains live",
        )
    }

    #[test]
    fn revise_memory_demotes_peer_attestation_for_locally_created_revision() -> TestResult {
        let (_temp, created) = remember_revisable_memory("Use the signed team release checklist.")?;
        let memory_id = created.memory_id.to_string();
        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        ensure(
            connection
                .update_memory_trust_class(&memory_id, TrustClass::PeerHumanAttested.as_str())
                .map_err(|error| error.to_string())?,
            true,
            "fixture trust update",
        )?;
        connection.close().map_err(|error| error.to_string())?;

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &created.database_path,
            original_memory_id: &memory_id,
            content: Some("Use a locally revised team release checklist."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Correction,
            actor: Some("SapphireBeacon"),
            dry_run: false,
        });

        ensure(report.success, true, "revision success")?;
        ensure(
            report.changed_fields,
            vec!["content".to_owned(), "trust_class".to_owned()],
            "revision reports trust demotion",
        )?;
        let new_id = report
            .new_id
            .as_deref()
            .ok_or_else(|| "peer revision should report a new memory id".to_owned())?;
        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let revised = connection
            .get_memory(new_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "peer revision should persist".to_owned())?;
        ensure(
            revised.trust_class,
            TrustClass::AgentAssertion.as_str().to_owned(),
            "local revision trust class",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn revise_memory_dry_run_preview_preserves_database() -> TestResult {
        let (temp, created) = remember_revisable_memory("Store release checks as durable memory.")?;
        enable_revision_dominance(temp.path())?;
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
        let impact = report
            .impact_analysis
            .as_ref()
            .ok_or_else(|| "dry-run revise should include impact analysis".to_string())?;
        ensure(
            impact.schema,
            crate::graph::dominance::MEMORY_IMPACT_ANALYSIS_SCHEMA_V1,
            "impact schema",
        )?;
        ensure(
            impact.memory_id.as_str(),
            memory_id.as_str(),
            "impact memory",
        )?;
        ensure(
            impact.impact_analysis.validation_status.as_str(),
            "unavailable",
            "singleton impact status",
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
    fn revise_memory_dry_run_gates_revision_dominance_by_default() -> TestResult {
        let (_temp, created) =
            remember_revisable_memory("Keep disabled revision analysis honest.")?;
        let memory_id = created.memory_id.to_string();

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &created.database_path,
            original_memory_id: &memory_id,
            content: Some("Keep disabled revision dominance analysis honest."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Update,
            actor: Some("StormyCove"),
            dry_run: true,
        });

        ensure(report.success, true, "success")?;
        let impact = report
            .impact_analysis
            .as_ref()
            .ok_or_else(|| "disabled gate should return an explicit impact block".to_string())?;
        ensure(
            impact.impact_analysis.validation_status.as_str(),
            "disabled",
            "disabled validation status",
        )?;
        ensure(
            impact.degraded[0].code.as_str(),
            "graph_feature_disabled",
            "disabled degraded code",
        )?;
        ensure(
            impact.degraded[0].repair.as_deref(),
            Some("ee config set graph.feature.revision_dominance.enabled true"),
            "disabled repair",
        )
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

    #[cfg(unix)]
    #[test]
    fn memory_config_reads_reject_symlinked_metadata_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).map_err(|error| error.to_string())?;
        let real_metadata = temp.path().join("real-ee");
        fs::create_dir(&real_metadata).map_err(|error| error.to_string())?;
        fs::write(
            real_metadata.join("config.toml"),
            "[policy.secret_detector]\nallow_phrases = [\"safe\"]\n",
        )
        .map_err(|error| error.to_string())?;
        symlink(&real_metadata, workspace.join(".ee")).map_err(|error| error.to_string())?;

        let error = match load_secret_detector_allow_config(&workspace) {
            Ok(config) => return Err(format!("expected symlink rejection, got {config:?}")),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("symlinked path component"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn memory_config_reads_reject_non_regular_config_path() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        fs::create_dir_all(&config_path).map_err(|error| error.to_string())?;

        let error = match load_secret_detector_allow_config(temp.path()) {
            Ok(config) => return Err(format!("expected non-regular rejection, got {config:?}")),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("not a regular file"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// Regression guard for the bounded-read defense in
    /// `read_workspace_config_if_present`. Pre-fix the helper called
    /// `fs::read_to_string` on `.ee/config.toml` with no size guard,
    /// so a peer-planted multi-MiB config would pin a matching
    /// allocation on every `ee remember` invocation
    /// (via both `load_secret_detector_allow_config` and
    /// `remember_cluster_coherence_config`). Same defect class that
    /// 7f56d89b (`PREFLIGHT_RULES_MAX_BYTES`) and aac04adb
    /// (`PREFLIGHT_RUN_STORE_MAX_BYTES`) closed for the parallel
    /// workspace-local `.ee/` files.
    ///
    /// This test plants a one-byte-over-cap `.ee/config.toml` and
    /// asserts the helper rejects with a structured Configuration
    /// error before the unbounded allocation. The error message
    /// names the offending path and the ceiling so an operator can
    /// fix the file directly.
    #[test]
    fn memory_config_reads_reject_oversize_config_file() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let ee_dir = temp.path().join(".ee");
        fs::create_dir(&ee_dir).map_err(|error| error.to_string())?;
        let config_path = ee_dir.join("config.toml");
        let cap = usize::try_from(super::WORKSPACE_CONFIG_MAX_BYTES)
            .map_err(|error| format!("cap fits in usize: {error}"))?;
        let mut payload = String::with_capacity(cap + 1);
        while payload.len() <= cap {
            payload.push('#');
        }
        fs::write(&config_path, &payload).map_err(|error| error.to_string())?;

        let error = match load_secret_detector_allow_config(temp.path()) {
            Ok(config) => {
                return Err(format!(
                    "expected oversize rejection before unbounded allocation, got {config:?}"
                ));
            }
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("exceeding the"),
            "rejection message must cite the ceiling; got: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(&super::WORKSPACE_CONFIG_MAX_BYTES.to_string()),
            "rejection message must name the cap constant; got: {error}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cluster_config_read_rejects_symlinked_config_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("workspace");
        let metadata = workspace.join(".ee");
        fs::create_dir_all(&metadata).map_err(|error| error.to_string())?;
        let outside_config = temp.path().join("outside-config.toml");
        fs::write(
            &outside_config,
            "[learn]\ncluster_coherence_threshold = 0.9\n",
        )
        .map_err(|error| error.to_string())?;
        symlink(&outside_config, metadata.join("config.toml"))
            .map_err(|error| error.to_string())?;

        let error = match remember_cluster_coherence_config(&workspace) {
            Ok(config) => return Err(format!("expected symlink rejection, got {config:?}")),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("symlinked path component"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn workspace_config_final_open_rejects_symlink_leaf() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let metadata = temp.path().join(".ee");
        fs::create_dir_all(&metadata).map_err(|error| error.to_string())?;
        let outside_config = temp.path().join("outside-config.toml");
        let outside_text = "[learn]\ncluster_coherence_threshold = 0.9\n";
        fs::write(&outside_config, outside_text).map_err(|error| error.to_string())?;
        let linked_config = metadata.join("config.toml");
        symlink(&outside_config, &linked_config).map_err(|error| error.to_string())?;

        let error = match super::open_workspace_config_file_for_read_no_follow(&linked_config) {
            Ok(_) => {
                return Err("final workspace config read open must reject symlinks".to_string());
            }
            Err(error) => error,
        };

        assert_ne!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "final symlink read should fail because the path is a symlink"
        );
        assert_eq!(
            fs::read_to_string(&outside_config).map_err(|error| error.to_string())?,
            outside_text,
            "workspace config read helper must not follow the symlink target"
        );
        Ok(())
    }

    #[test]
    fn cluster_config_read_rejects_non_regular_config_path() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        fs::create_dir_all(&config_path).map_err(|error| error.to_string())?;

        let error = match remember_cluster_coherence_config(temp.path()) {
            Ok(config) => return Err(format!("expected non-regular rejection, got {config:?}")),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("not a regular file"),
            "unexpected error: {error}"
        );
        Ok(())
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

    #[test]
    fn remember_index_deadline_after_commit_is_reported_as_queued() -> TestResult {
        for reason in [
            asupersync::CancelReason::deadline(),
            asupersync::CancelReason::timeout(),
        ] {
            let error = IndexRebuildError::Cancelled(reason);
            ensure(
                remember_index_failure_is_deferable(&error),
                true,
                "deadline-like index cancellation is deferable after commit",
            )?;
            let report = remember_index_job_queued_after_transient_failure("sidx_fixture", &error);
            ensure(
                report.job_id.clone(),
                "sidx_fixture".to_owned(),
                "queued job id",
            )?;
            ensure(
                report.outcome.clone(),
                "skipped".to_owned(),
                "queued outcome",
            )?;
            ensure(
                remember_index_status(&report),
                "queued".to_owned(),
                "public index status",
            )?;
            ensure(report.documents_indexed, 0, "queued document count")?;
            ensure(
                report
                    .error
                    .as_deref()
                    .is_some_and(|message| message.contains("transient failure")),
                true,
                "queued report retains a bounded transient diagnosis",
            )?;
        }

        ensure(
            remember_index_failure_is_deferable(&IndexRebuildError::Cancelled(
                asupersync::CancelReason::user("operator cancelled remember"),
            )),
            false,
            "explicit user cancellation remains terminal",
        )
    }

    #[test]
    fn remember_index_burst_routes_defer_inline_and_leadership() -> TestResult {
        let own_job = "sidx_own";
        ensure(
            remember_index_publish_route(false, &[own_job.to_owned()], own_job),
            RememberIndexPublishRoute::Inline,
            "a solitary remember retains the immediate-index fast path",
        )?;
        ensure(
            remember_index_publish_route(true, &[own_job.to_owned()], own_job),
            RememberIndexPublishRoute::Defer,
            "an active publisher defers the new durable job",
        )?;
        // bd-index-auto-freshness-m5kwf liveness: with the initial publisher
        // finished, a pending peer job must elect a drainer instead of
        // deferring unowned (30/30-durable-but-1/30-searchable hole).
        ensure(
            remember_index_publish_route(
                false,
                &[own_job.to_owned(), "sidx_peer".to_owned()],
                own_job,
            ),
            RememberIndexPublishRoute::LeadCoalescedDrain,
            "peer pending without an active publisher must attempt drain leadership",
        )?;
        ensure(
            remember_index_publish_route(
                true,
                &[own_job.to_owned(), "sidx_peer".to_owned()],
                own_job,
            ),
            RememberIndexPublishRoute::Defer,
            "an active publisher always wins over leadership",
        )?;

        let report = remember_index_job_queued_for_coalescing(own_job);
        ensure(
            report.outcome.clone(),
            "skipped".to_owned(),
            "queued outcome",
        )?;
        ensure(
            report.processing_mode.clone(),
            "deferred_to_coalesced_contention_rebuild".to_owned(),
            "bounded burst processing mode",
        )?;
        ensure(
            remember_index_status(&report),
            "queued".to_owned(),
            "public burst index posture",
        )
    }

    fn drained_test_report(job_id: &str) -> IndexProcessingJobReport {
        IndexProcessingJobReport {
            job_id: job_id.to_owned(),
            job_type: SearchIndexJobType::SingleDocument.as_str().to_owned(),
            document_source: Some("memory".to_owned()),
            document_id: None,
            outcome: "completed".to_owned(),
            processing_mode: "coalesced".to_owned(),
            documents_total: 1,
            documents_indexed: 1,
            error: None,
            fallback_to_full: None,
        }
    }

    #[test]
    fn remember_drain_leadership_elects_one_winner_and_losers_defer() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let connection = open_upgrade_test_db(temp.path())?;
        connection
            .ensure_advisory_locks_table()
            .map_err(|error| error.to_string())?;
        let canonical = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace_id = stable_workspace_id(&canonical);
        let index_dir = canonical.join(".ee").join(DEFAULT_INDEX_SUBDIR);

        // A peer job is pending and no publisher is active, so the next
        // PUBLIC remember must route to drain leadership.
        let deferred = remember_memory_with_index_mode(
            &upgrade_remember_options(
                temp.path(),
                "Leadership burst peer row about pending drains.",
                0.8,
                None,
                false,
            ),
            true,
            &[],
            None,
        )
        .map_err(|error| error.message())?;
        ensure(
            deferred.index_status.clone(),
            "queued".to_owned(),
            "deferred remember reports the truthful queued posture",
        )?;

        // A rival process holds the election lock: the public remember must
        // LOSE without blocking and report the deferred queued posture.
        let lock_id = remember_index_drain_leader_lock(&workspace_id);
        let rival = "remember-drain:rival:sidx_rival";
        let acquired = connection
            .acquire_advisory_lock(&lock_id, rival, Some(60), Some("test rival leader"))
            .map_err(|error| error.to_string())?;
        ensure(acquired.is_acquired(), true, "rival takes election lock")?;
        let loser = remember_memory(&upgrade_remember_options(
            temp.path(),
            "Public loser row must defer without blocking.",
            0.8,
            None,
            false,
        ))
        .map_err(|error| error.message())?;
        ensure(
            loser.index_status.clone(),
            "queued".to_owned(),
            "public election loser defers instead of blocking",
        )?;
        assert!(
            connection
                .release_advisory_lock(&lock_id, rival)
                .map_err(|error| error.to_string())?,
            "release rival election lock"
        );

        // Lock free, peers pending: the next PUBLIC remember wins the
        // election, drains everything coalesced, and reports its own
        // document as indexed.
        let winner = remember_memory(&upgrade_remember_options(
            temp.path(),
            "Public winner row drains the whole burst.",
            0.8,
            None,
            false,
        ))
        .map_err(|error| error.message())?;
        ensure(
            winner.index_status.clone(),
            "indexed".to_owned(),
            "public election winner publishes its own document",
        )?;
        let pending_after = connection
            .list_pending_search_index_jobs(&workspace_id, None)
            .map_err(|error| error.to_string())?;
        ensure(
            pending_after.len(),
            0,
            "the winner's coalesced drain leaves no unowned pending jobs",
        )?;
        ensure(index_dir.exists(), true, "winner published an index")?;
        // The election lock must be released for the next burst.
        let reacquired = connection
            .acquire_advisory_lock(&lock_id, "post-test-probe", Some(5), None)
            .map_err(|error| error.to_string())?;
        ensure(
            reacquired.is_acquired(),
            true,
            "leadership lock is released after the drain",
        )
    }

    #[test]
    fn remember_publish_barrier_tail_is_drained_and_searchable() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let connection = open_upgrade_test_db(temp.path())?;
        connection
            .ensure_advisory_locks_table()
            .map_err(|error| error.to_string())?;
        let canonical = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace_id = stable_workspace_id(&canonical);
        let index_dir = canonical.join(".ee").join(DEFAULT_INDEX_SUBDIR);

        // Barrier: an active publisher holds the index publish lock while
        // every other writer commits. The PUBLIC remembers must defer.
        let publish_lock = AdvisoryLockId::index(&workspace_id);
        let publisher = "remember:publisher:barrier";
        let held = connection
            .acquire_advisory_lock(
                &publish_lock,
                publisher,
                Some(60),
                Some("barrier publisher"),
            )
            .map_err(|error| error.to_string())?;
        ensure(held.is_acquired(), true, "barrier publisher takes lock")?;

        let mut loser_ids = Vec::new();
        for content in [
            "Barrier loser row one about stranded tails.",
            "Barrier loser row two about final drains.",
        ] {
            let report = remember_memory(&upgrade_remember_options(
                temp.path(),
                content,
                0.8,
                None,
                false,
            ))
            .map_err(|error| error.message())?;
            ensure(
                report.index_status.clone(),
                "queued".to_owned(),
                "losers defer while the publisher is active",
            )?;
            loser_ids.push(report.memory_id.to_string());
        }
        let pending_during = connection
            .list_pending_search_index_jobs(&workspace_id, None)
            .map_err(|error| error.to_string())?;
        ensure(
            pending_during.len(),
            2,
            "both losers enqueued during the active publisher",
        )?;

        // Publisher finishes LAST: it releases the lock and, as its final
        // production act, sweeps the tail. No subsequent write occurs.
        assert!(
            connection
                .release_advisory_lock(&publish_lock, publisher)
                .map_err(|error| error.to_string())?,
            "publisher releases the publish lock"
        );
        remember_drain_peer_tail_after_publish(&connection, &workspace_id, &index_dir);

        let pending_after = connection
            .list_pending_search_index_jobs(&workspace_id, None)
            .map_err(|error| error.to_string())?;
        ensure(
            pending_after.len(),
            0,
            "the post-publish sweep leaves no unowned pending jobs",
        )?;
        ensure(index_dir.exists(), true, "tail sweep published an index")?;

        // Searchable: the losers' documents are retrievable lexically.
        let report = crate::core::search::run_search_with_filters(
            &crate::core::search::SearchOptions {
                workspace_path: canonical.clone(),
                database_path: None,
                index_dir: None,
                query: "barrier loser row".to_owned(),
                limit: 10,
                speed: crate::search::SpeedMode::Instant,
                explain: false,
                as_of: None,
                include_tombstoned: false,
                include_expired: false,
                include_future: false,
                include_stale: false,
                relevance_floor: Some(0.0),
                dedup_mode: crate::core::search::SearchDedupMode::DocId,
                source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
                strict_source_mode: true,
                memory_scope: crate::models::MemoryScope::Workspace,
                strict_scope: false,
            },
            None,
            &[],
        )
        .map_err(|error| format!("barrier search failed: {error:?}"))?;
        let result_ids = report
            .results
            .iter()
            .map(|hit| hit.doc_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for memory_id in &loser_ids {
            ensure(
                result_ids.contains(memory_id),
                true,
                "every barrier loser must be searchable after the tail sweep",
            )?;
        }
        Ok(())
    }

    #[test]
    fn remember_drain_rounds_second_pass_catches_stragglers() -> TestResult {
        let own = "sidx_round_own";
        let straggler_drained = std::cell::Cell::new(false);
        let rounds = std::cell::Cell::new(0usize);
        let report = remember_drain_pending_rounds(
            own,
            2,
            || {
                rounds.set(rounds.get() + 1);
                if rounds.get() == 1 {
                    // First round drains our job while a straggler lands.
                    Ok(vec![drained_test_report(own)])
                } else {
                    straggler_drained.set(true);
                    Ok(vec![drained_test_report("sidx_straggler")])
                }
            },
            || Some(rounds.get() == 1),
            || unreachable!("own report was drained; the absent resolver must not run"),
        );
        ensure(rounds.get(), 2, "leader runs the bounded second pass")?;
        ensure(
            straggler_drained.get(),
            true,
            "the second pass drains the straggler",
        )?;
        ensure(report.job_id.clone(), own.to_owned(), "own report kept")?;
        ensure(
            remember_index_status(&report),
            "indexed".to_owned(),
            "own outcome reported from the first round",
        )?;

        // Planted negative: a failing drain with our job still pending must
        // NOT claim success — it reports the queued posture so the next
        // writer's election owns the remainder.
        let failed = remember_drain_pending_rounds(
            own,
            2,
            || {
                Err(IndexRebuildError::Index(
                    "synthetic drain failure".to_owned(),
                ))
            },
            || Some(true),
            || unreachable!("a failed round resolves before the absent resolver"),
        );
        ensure(
            remember_index_status(&failed),
            "queued".to_owned(),
            "drain failure with pending work never reports success",
        )?;

        // Planted negative: rounds that never list our job must resolve
        // through durable truth, never through an assumed success.
        let resolver_ran = std::cell::Cell::new(false);
        let absent = remember_drain_pending_rounds(
            own,
            2,
            || Ok(vec![drained_test_report("sidx_only_peers")]),
            || Some(false),
            || {
                resolver_ran.set(true);
                remember_index_job_queued_for_coalescing(own)
            },
        );
        ensure(
            resolver_ran.get(),
            true,
            "an absent own report consults the durable-state resolver",
        )?;
        ensure(
            remember_index_status(&absent),
            "queued".to_owned(),
            "absence never hard-codes success",
        )
    }

    #[test]
    fn remember_absent_drain_report_resolves_from_durable_state() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let connection = open_upgrade_test_db(temp.path())?;
        let canonical = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace_id = stable_workspace_id(&canonical);
        let job_input = |total: u32| crate::db::CreateSearchIndexJobInput {
            workspace_id: workspace_id.clone(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: None,
            documents_total: total,
        };

        // Pending: publish unproven, so the posture stays queued.
        connection
            .insert_search_index_job("sidx_durable_pending_000000000", &job_input(1))
            .map_err(|error| error.to_string())?;
        let pending = remember_index_job_report_from_durable_state(
            &connection,
            "sidx_durable_pending_000000000",
        );
        ensure(
            remember_index_status(&pending),
            "queued".to_owned(),
            "a pending row never reports success",
        )?;

        // Completed: report indexed with the durable counts, not invented ones.
        connection
            .insert_search_index_job("sidx_durable_done_00000000000", &job_input(1))
            .map_err(|error| error.to_string())?;
        connection
            .start_search_index_job("sidx_durable_done_00000000000")
            .map_err(|error| error.to_string())?;
        connection
            .complete_search_index_job("sidx_durable_done_00000000000", 1)
            .map_err(|error| error.to_string())?;
        let done = remember_index_job_report_from_durable_state(
            &connection,
            "sidx_durable_done_00000000000",
        );
        ensure(
            remember_index_status(&done),
            "indexed".to_owned(),
            "a completed row reports the indexed posture",
        )?;
        ensure(done.documents_indexed, 1, "durable indexed count kept")?;

        // Failed: stay truthful and keep the durable error message.
        connection
            .insert_search_index_job("sidx_durable_failed_000000000", &job_input(1))
            .map_err(|error| error.to_string())?;
        connection
            .start_search_index_job("sidx_durable_failed_000000000")
            .map_err(|error| error.to_string())?;
        connection
            .fail_search_index_job("sidx_durable_failed_000000000", "synthetic durable failure")
            .map_err(|error| error.to_string())?;
        let failed = remember_index_job_report_from_durable_state(
            &connection,
            "sidx_durable_failed_000000000",
        );
        ensure(
            remember_index_status(&failed),
            "failed".to_owned(),
            "a failed row reports failure",
        )?;
        ensure(
            failed.error.as_deref().unwrap_or_default().to_owned(),
            "synthetic durable failure".to_owned(),
            "the durable error message is preserved",
        )?;

        // Missing: absence of the row is not proof of anything but pending.
        let missing = remember_index_job_report_from_durable_state(
            &connection,
            "sidx_durable_missing_00000000",
        );
        ensure(
            remember_index_status(&missing),
            "queued".to_owned(),
            "a missing row never reports success",
        )
    }

    #[test]
    fn remember_workspace_lock_timeout_emits_advisory_lock_code() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection
            .ensure_advisory_locks_table()
            .map_err(|error| error.to_string())?;
        let lock_id = AdvisoryLockId::workspace("wsp_remember_timeout");
        // A non-PID holder is intentionally unprobeable, so the fixture cannot
        // be auto-reclaimed merely because a chosen synthetic PID is absent on
        // this worker (or held merely because that PID happens to be alive).
        let sensitive_holder = "fixture-sensitive-holder";
        let acquired = connection
            .acquire_advisory_lock(&lock_id, sensitive_holder, Some(300), Some("unit test"))
            .map_err(|error| error.to_string())?;
        ensure(acquired.is_acquired(), true, "fixture lock acquired")?;

        let error = match acquire_remember_workspace_lock_with_retry(
            &connection,
            "wsp_remember_timeout",
            "mem_01234567890123456789012345",
            1,
            Duration::from_secs(1),
            |_| Duration::ZERO,
        ) {
            Ok(_) => return Err("remember should not acquire a held workspace lock".to_owned()),
            Err(error) => error,
        };

        let json = crate::output::error_response_json(&error);
        ensure(
            json.contains(crate::models::degradation::ADVISORY_LOCK_TIMEOUT_CODE),
            true,
            "error envelope carries advisory lock timeout code",
        )?;
        ensure(
            json.contains(
                "ee diag advisory-lock --workspace . --resource-type workspace --release --json",
            ),
            true,
            "error envelope carries recovery command",
        )?;
        ensure(
            !json.contains(sensitive_holder),
            true,
            "timeout envelope does not expose competing holder identity",
        )?;

        connection
            .release_advisory_lock(&lock_id, sensitive_holder)
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_workspace_lock_wait_survives_deep_holder_progress() -> TestResult {
        // bd-rs4cm: `attempts` bounds SAME-HOLDER stagnation, not total
        // waiting. More than the removed 512-poll ceiling of the first fix
        // must still acquire while every observation shows queue progress.
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection
            .ensure_advisory_locks_table()
            .map_err(|error| error.to_string())?;
        let lock_id = AdvisoryLockId::workspace("wsp_remember_progress");
        let acquired = connection
            .acquire_advisory_lock(&lock_id, "holder-0", Some(300), Some("unit test"))
            .map_err(|error| error.to_string())?;
        ensure(acquired.is_acquired(), true, "fixture lock acquired")?;

        const HOLDER_TURNOVERS: usize = 513;
        let sleep_calls = std::cell::Cell::new(0usize);
        let delay_attempts = std::cell::RefCell::new(Vec::new());
        let guard = acquire_remember_workspace_lock_with_retry(
            &connection,
            "wsp_remember_progress",
            "mem_01234567890123456789012346",
            2,
            Duration::from_secs(30),
            |delay_attempt| {
                let call = sleep_calls.get();
                sleep_calls.set(call + 1);
                delay_attempts.borrow_mut().push(delay_attempt);
                let current_holder = format!("holder-{call}");
                assert!(
                    connection
                        .release_advisory_lock(&lock_id, &current_holder)
                        .is_ok(),
                    "release current holder"
                );
                if call < HOLDER_TURNOVERS {
                    let next_holder = format!("holder-{}", call + 1);
                    assert!(
                        connection
                            .acquire_advisory_lock(
                                &lock_id,
                                &next_holder,
                                Some(300),
                                Some("unit test"),
                            )
                            .is_ok_and(|result| result.is_acquired()),
                        "acquire next holder"
                    );
                }
                Duration::ZERO
            },
        )
        .map_err(|error| format!("progressing queue must not time out: {error:?}"))?;
        ensure(
            sleep_calls.get() > 512,
            true,
            "wait outlasted the removed 512-poll ceiling",
        )?;
        ensure(
            delay_attempts.borrow().iter().all(|attempt| *attempt == 0),
            true,
            "holder turnover resets the no-progress backoff",
        )?;
        drop(guard);
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_workspace_lock_holder_churn_obeys_elapsed_ceiling_without_ambient_cx() -> TestResult
    {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection
            .ensure_advisory_locks_table()
            .map_err(|error| error.to_string())?;
        let lock_id = AdvisoryLockId::workspace("wsp_remember_churn_ceiling");
        let acquired = connection
            .acquire_advisory_lock(&lock_id, "holder-0", Some(300), Some("unit test"))
            .map_err(|error| error.to_string())?;
        ensure(acquired.is_acquired(), true, "fixture lock acquired")?;

        let sleep_calls = std::cell::Cell::new(0usize);
        let elapsed_checks = std::cell::Cell::new(0usize);
        let error = match acquire_remember_workspace_lock_with_retry_and_elapsed(
            &connection,
            "wsp_remember_churn_ceiling",
            "mem_01234567890123456789012347",
            2,
            Duration::from_millis(1),
            |_| {
                let call = sleep_calls.get();
                sleep_calls.set(call + 1);
                let current_holder = format!("holder-{call}");
                assert!(
                    connection
                        .release_advisory_lock(&lock_id, &current_holder)
                        .is_ok(),
                    "release current holder"
                );
                let next_holder = format!("holder-{}", call + 1);
                assert!(
                    connection
                        .acquire_advisory_lock(
                            &lock_id,
                            &next_holder,
                            Some(300),
                            Some("unit test"),
                        )
                        .is_ok_and(|result| result.is_acquired()),
                    "acquire next holder"
                );
                Duration::ZERO
            },
            || {
                let check = elapsed_checks.get();
                elapsed_checks.set(check + 1);
                if check == 0 {
                    Duration::ZERO
                } else {
                    Duration::from_millis(1)
                }
            },
        ) {
            Ok(_) => return Err("holder churn must not bypass the elapsed ceiling".to_owned()),
            Err(error) => error,
        };
        ensure(
            error.message().contains("advisory lock timeout"),
            true,
            "elapsed ceiling preserves advisory-lock timeout classification",
        )?;
        ensure(
            sleep_calls.get() >= 1,
            true,
            "fixture changed the holder before the elapsed ceiling fired",
        )?;

        let final_holder = format!("holder-{}", sleep_calls.get());
        connection
            .release_advisory_lock(&lock_id, &final_holder)
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_workspace_lock_releases_when_guard_drops() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection
            .ensure_advisory_locks_table()
            .map_err(|error| error.to_string())?;
        let lock_id = AdvisoryLockId::workspace("wsp_remember_release");

        {
            let _owner = acquire_remember_workspace_lock_with_retry(
                &connection,
                "wsp_remember_release",
                "mem_01234567890123456789012346",
                1,
                Duration::from_secs(1),
                |_| Duration::ZERO,
            )
            .map_err(|error| error.to_string())?;
            ensure(
                connection
                    .is_lock_held(&lock_id)
                    .map_err(|error| error.to_string())?
                    .is_some(),
                true,
                "lock held while guard is alive",
            )?;
        }

        ensure(
            connection
                .is_lock_held(&lock_id)
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "lock released when guard drops",
        )?;
        connection.close().map_err(|error| error.to_string())
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

    // -------------------------------------------------------------------------
    // bd-1pi9m.4: `--batch --stdin`, `--reinforce`, idempotency keys.
    // -------------------------------------------------------------------------

    fn upgrade_test_workspace() -> Result<tempfile::TempDir, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
        Ok(temp)
    }

    fn set_workspace_duplicate_similarity(workspace: &Path, threshold: f64) -> TestResult {
        std::fs::write(
            workspace.join(".ee").join("config.toml"),
            format!("[curation]\nduplicate_similarity = {threshold}\n"),
        )
        .map_err(|error| error.to_string())
    }

    fn upgrade_remember_options<'a>(
        workspace_path: &'a Path,
        content: &'a str,
        confidence: f32,
        source: Option<&'a str>,
        dry_run: bool,
    ) -> RememberMemoryOptions<'a> {
        RememberMemoryOptions {
            workspace_path,
            database_path: None,
            content,
            workflow_id: None,
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence,
            source,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run,
            auto_link: false,
            propose_candidates: false,
        }
    }

    fn upgrade_batch_options(workspace_path: &Path, dry_run: bool) -> RememberBatchOptions<'_> {
        RememberBatchOptions {
            workspace_path,
            database_path: None,
            reinforce: false,
            dry_run,
            auto_link: false,
            propose_candidates: false,
        }
    }

    fn open_upgrade_test_db(workspace_path: &Path) -> Result<DbConnection, String> {
        DbConnection::open_file(&workspace_path.join(".ee").join("ee.db"))
            .map_err(|error| error.to_string())
    }

    fn upgrade_created_report(
        outcome: RememberOutcome,
        ctx: &str,
    ) -> Result<Box<RememberMemoryReport>, String> {
        match outcome {
            RememberOutcome::Created(report) => Ok(report),
            other => Err(format!("{ctx}: expected Created outcome, got {other:?}")),
        }
    }

    fn upgrade_reinforced_report(
        outcome: RememberOutcome,
        ctx: &str,
    ) -> Result<RememberReinforceReport, String> {
        match outcome {
            RememberOutcome::Reinforced(report) => Ok(report),
            other => Err(format!("{ctx}: expected Reinforced outcome, got {other:?}")),
        }
    }

    /// bd-2efx1: the batch lane defers per-line index publishing and
    /// drains every enqueued job with ONE coalesced rebuild — afterwards
    /// nothing is pending and the published index exists on disk.
    #[test]
    fn remember_batch_drains_index_jobs_with_one_coalesced_rebuild() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let input = concat!(
            "{\"content\":\"Coalesced batch row one about release gates.\"}\n",
            "{\"content\":\"Coalesced batch row two about clippy waivers.\"}\n",
            "{\"content\":\"Coalesced batch row three about index posture.\"}\n",
        );
        let report = remember_memory_batch_stdin(&upgrade_batch_options(temp.path(), false), input)
            .map_err(|error| error.message())?;
        ensure(report.stored_count, 3, "stored count")?;

        let canonical = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let connection = open_upgrade_test_db(temp.path())?;
        let pending = connection
            .list_pending_search_index_jobs(&stable_workspace_id(&canonical), None)
            .map_err(|error| error.to_string())?;
        ensure(pending.len(), 0, "pending index jobs after the batch drain")?;
        ensure(
            canonical.join(".ee").join(DEFAULT_INDEX_SUBDIR).exists(),
            true,
            "coalesced rebuild published an index directory",
        )
    }

    #[test]
    fn remember_batch_accepts_registry_fields_object() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let input = "{\"content\":\"Remote verification decision.\",\"kind\":\"decision\",\"fields\":{\"chosen\":\"RCH remote\",\"options\":[\"local Cargo\",\"RCH remote\"],\"rationale\":\"avoid local artifacts\"}}\n";
        let report = remember_memory_batch_stdin(&upgrade_batch_options(temp.path(), false), input)
            .map_err(|error| error.message())?;
        ensure(report.stored_count, 1, "stored typed batch row")?;
        ensure(report.failed_count, 0, "typed batch failures")?;

        let memory_id = report.results[0]
            .memory_id
            .as_deref()
            .ok_or_else(|| "typed batch memory id missing".to_owned())?;
        let connection = open_upgrade_test_db(temp.path())?;
        let stored = connection
            .get_memory_typed_fields_json(memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "typed batch sidecar missing".to_owned())?;
        let stored: serde_json::Value =
            serde_json::from_str(&stored).map_err(|error| error.to_string())?;
        ensure(
            stored["fields"]["chosen"].as_str(),
            Some("RCH remote"),
            "typed batch chosen",
        )?;
        ensure(
            stored["fields"]["options"].as_array().map(Vec::len),
            Some(2),
            "typed batch options",
        )
    }

    #[test]
    fn remember_batch_fields_enforce_registry_shapes_and_normalized_names() -> TestResult {
        let scalar_array = parse_remember_batch_line(
            r#"{"content":"Failure evidence.","kind":"failure","fields":{"family":["one"]}}"#,
        )
        .expect_err("scalar fields reject arrays even when they contain one item");
        ensure(
            scalar_array.code,
            TYPED_FIELD_INVALID_CODE,
            "scalar array error code",
        )?;

        let list_scalar = parse_remember_batch_line(
            r#"{"content":"Decision evidence.","kind":"decision","fields":{"options":"remote"}}"#,
        )
        .expect_err("list fields reject scalar strings");
        ensure(
            list_scalar.code,
            TYPED_FIELD_INVALID_CODE,
            "list scalar error code",
        )?;

        let empty_list = parse_remember_batch_line(
            r#"{"content":"Decision evidence.","kind":"decision","fields":{"options":[]}}"#,
        )
        .expect_err("list fields reject empty arrays");
        ensure(
            empty_list.code,
            TYPED_FIELD_INVALID_CODE,
            "empty list error code",
        )?;

        let all_null = parse_remember_batch_line(
            r#"{"content":"Failure evidence.","kind":"failure","fields":{"family":null}}"#,
        )
        .expect_err("null is not a typed-field write value");
        ensure(
            all_null.code,
            TYPED_FIELD_INVALID_CODE,
            "all-null field error code",
        )?;

        let mixed_null = parse_remember_batch_line(
            r#"{"content":"Failure evidence.","kind":"failure","fields":{"family":"prefetch","cause":null}}"#,
        )
        .expect_err("mixed valid and null fields reject the complete line");
        ensure(
            mixed_null.code,
            TYPED_FIELD_INVALID_CODE,
            "mixed-null field error code",
        )?;

        let unknown_null = parse_remember_batch_line(
            r#"{"content":"Failure evidence.","kind":"failure","fields":{"disposition":null}}"#,
        )
        .expect_err("unknown null fields cannot disappear before registry validation");
        ensure(
            unknown_null.code,
            TYPED_FIELD_UNKNOWN_CODE,
            "unknown-null field error code",
        )?;

        let smuggled_name = parse_remember_batch_line(
            r#"{"content":"Failure evidence.","kind":"failure","fields":{"family=prefix":"value"}}"#,
        )
        .expect_err("an equals sign in an object key cannot alter assignment parsing");
        ensure(
            smuggled_name.code,
            TYPED_FIELD_INVALID_CODE,
            "smuggled name error code",
        )?;

        let normalized = parse_remember_batch_line(
            r#"{"content":"Decision evidence.","kind":"decision","fields":{"revisit-by":"2026-09-15T00:00:00Z"}}"#,
        )
        .map_err(|error| error.message)?;
        ensure(
            normalized.typed_field_assignments,
            vec!["revisit_by=2026-09-15T00:00:00Z".to_owned()],
            "batch field name normalization",
        )
    }

    #[test]
    fn remember_batch_isolates_invalid_lines() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let input = concat!(
            "{\"content\":\"Batch line one survives the poisoned neighbor.\"}\n",
            "{\"level\":\"episodic\"}\n",
            "{\"content\":\"Batch line three survives the poisoned neighbor.\"}\n",
        );
        let report = remember_memory_batch_stdin(&upgrade_batch_options(temp.path(), false), input)
            .map_err(|error| error.message())?;

        ensure(report.line_count, 3, "line count")?;
        ensure(report.stored_count, 2, "stored count")?;
        ensure(report.failed_count, 1, "failed count")?;
        ensure(
            report.all_failed(),
            false,
            "partial success is not all_failed",
        )?;
        ensure(report.results[0].status, "stored", "line 1 status")?;
        ensure(report.results[1].status, "failed", "line 2 status")?;
        ensure(
            report.results[1].error_code,
            Some("remember_content_required"),
            "line 2 error code",
        )?;
        ensure(report.results[2].status, "stored", "line 3 status")?;

        let connection = open_upgrade_test_db(temp.path())?;
        for line in [&report.results[0], &report.results[2]] {
            let memory_id = line
                .memory_id
                .clone()
                .ok_or_else(|| format!("line {} memory id missing", line.line))?;
            ensure(
                connection
                    .get_memory(&memory_id)
                    .map_err(|error| error.to_string())?
                    .is_some(),
                true,
                "stored line persisted a row",
            )?;
        }
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_batch_rejects_cross_wire_and_preserves_custom_kind_canonicalization() -> TestResult
    {
        let temp = upgrade_test_workspace()?;
        let input = concat!(
            "{\"content\":\"Cross-wired batch row.\",\"kind\":\"episodic\",\"idempotencyKey\":\"cross-wire-batch\"}\n",
            "{\"content\":\"Custom kind batch row.\",\"kind\":\"EpisodicNote\"}\n",
        );
        let report = remember_memory_batch_stdin(&upgrade_batch_options(temp.path(), false), input)
            .map_err(|error| error.message())?;

        ensure(report.stored_count, 1, "one valid batch row stored")?;
        ensure(report.failed_count, 1, "cross-wired batch row failed")?;
        ensure(
            report.results[0].error_code,
            Some(REMEMBER_KIND_IS_LEVEL_CODE),
            "batch cross-wire error code",
        )?;
        ensure(
            report.results[0].memory_id.is_none(),
            true,
            "failed batch row has no memory id",
        )?;
        let custom_id = report.results[1]
            .memory_id
            .as_deref()
            .ok_or_else(|| "custom-kind batch row memory id missing".to_owned())?;
        let connection = open_upgrade_test_db(temp.path())?;
        let stored = connection
            .get_memory(custom_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "custom-kind batch row missing".to_owned())?;
        ensure(
            stored.kind,
            "episodic-note".to_owned(),
            "custom kind keeps established canonicalization",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_batch_all_failed_drives_exit_five_signal() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let input = "not-json\n{\"level\":\"episodic\"}\n";
        let report = remember_memory_batch_stdin(&upgrade_batch_options(temp.path(), false), input)
            .map_err(|error| error.message())?;

        ensure(report.line_count, 2, "line count")?;
        ensure(report.failed_count, 2, "failed count")?;
        ensure(
            report.all_failed(),
            true,
            "all_failed drives the exit-5 path",
        )?;
        ensure(
            report.results[0].error_code,
            Some("remember_invalid_json"),
            "line 1 error code",
        )
    }

    #[test]
    fn remember_batch_rejects_oversize_batches() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let input = "{\"content\":\"x\"}\n".repeat(REMEMBER_BATCH_MAX_LINES + 1);
        match remember_memory_batch_stdin(&upgrade_batch_options(temp.path(), false), &input) {
            Err(DomainError::Usage { message, .. }) => ensure(
                message.contains(&REMEMBER_BATCH_MAX_LINES.to_string()),
                true,
                "oversize error names the line cap",
            ),
            other => Err(format!(
                "expected usage error for oversize batch, got {other:?}"
            )),
        }
    }

    #[test]
    fn remember_idempotency_replay_returns_original_memory() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let content = "Idempotent lesson: pin the schema before regenerating goldens.";
        let controls = RememberWriteControls {
            reinforce: false,
            idempotency_key: Some("lesson-001"),
            defer_index_processing: false,
        };

        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.8, None, false),
                &controls,
            )
            .map_err(|error| error.message())?,
            "first write",
        )?;

        let replay = remember_memory_with_controls(
            &upgrade_remember_options(temp.path(), content, 0.8, None, false),
            &controls,
        )
        .map_err(|error| error.message())?;
        match replay {
            RememberOutcome::AlreadyRecorded(report) => {
                ensure(
                    report.memory_id,
                    created.memory_id.to_string(),
                    "replay returns the original memory id",
                )?;
                ensure(
                    report.idempotency_key,
                    "lesson-001".to_owned(),
                    "replay echoes the key",
                )?;
            }
            other => return Err(format!("expected already_recorded, got {other:?}")),
        }

        match remember_memory_with_controls(
            &upgrade_remember_options(
                temp.path(),
                "Different content under the same idempotency key.",
                0.8,
                None,
                false,
            ),
            &controls,
        ) {
            Err(DomainError::UsageCodeWithDetails { code, .. }) => ensure(
                code,
                REMEMBER_IDEMPOTENCY_CONFLICT_CODE,
                "same key with different content is a per-line usage error",
            ),
            other => Err(format!("expected idempotency conflict, got {other:?}")),
        }
    }

    #[test]
    fn remember_cross_wire_validation_precedes_keyed_replay() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let content = "Keyed replay must not bypass level-kind validation.";
        let controls = RememberWriteControls {
            reinforce: false,
            idempotency_key: Some("cross-wire-keyed-replay"),
            defer_index_processing: false,
        };
        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.8, None, false),
                &controls,
            )
            .map_err(|error| error.message())?,
            "keyed replay seed",
        )?;

        let mut cross_wired = upgrade_remember_options(temp.path(), content, 0.8, None, false);
        cross_wired.kind = "semantic";
        match remember_memory_with_controls(&cross_wired, &controls) {
            Err(error) => ensure(
                error.code(),
                REMEMBER_KIND_IS_LEVEL_CODE,
                "keyed replay cross-wire error code",
            )?,
            Ok(outcome) => {
                return Err(format!(
                    "cross-wired keyed replay unexpectedly succeeded: {outcome:?}"
                ));
            }
        }

        let connection = open_upgrade_test_db(temp.path())?;
        let memories = connection
            .list_memories(&created.workspace_id, None, true)
            .map_err(|error| error.to_string())?;
        ensure(memories.len(), 1, "keyed replay rejection keeps one row")?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_idempotency_identity_includes_explicit_typed_fields() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let content = "Remote verification won the storage decision.";
        let mut options = upgrade_remember_options(temp.path(), content, 0.8, None, false);
        options.kind = "decision";
        let controls = RememberWriteControls {
            reinforce: false,
            idempotency_key: Some("typed-decision-001"),
            defer_index_processing: false,
        };
        let original_fields = vec!["chosen=RCH remote".to_owned()];

        let created = upgrade_created_report(
            remember_memory_with_controls_and_typed_fields(&options, &controls, &original_fields)
                .map_err(|error| error.message())?,
            "typed first write",
        )?;
        let replay =
            remember_memory_with_controls_and_typed_fields(&options, &controls, &original_fields)
                .map_err(|error| error.message())?;
        match replay {
            RememberOutcome::AlreadyRecorded(report) => ensure(
                report.memory_id,
                created.memory_id.to_string(),
                "typed replay returns the original memory id",
            )?,
            other => return Err(format!("expected typed replay, got {other:?}")),
        }

        let changed_fields = vec!["chosen=local Cargo".to_owned()];
        match remember_memory_with_controls_and_typed_fields(&options, &controls, &changed_fields) {
            Err(DomainError::UsageCodeWithDetails { code, .. }) => ensure(
                code,
                REMEMBER_IDEMPOTENCY_CONFLICT_CODE,
                "same key and content with changed typed fields conflicts",
            ),
            other => Err(format!(
                "expected typed-field idempotency conflict, got {other:?}"
            )),
        }
    }

    #[test]
    fn remember_reinforce_above_threshold_strengthens_existing_memory() -> TestResult {
        let temp = upgrade_test_workspace()?;
        set_workspace_duplicate_similarity(temp.path(), 0.5)?;
        let content = "Reinforce target: always run the drift radar before pushing.";

        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.8, None, false),
                &RememberWriteControls::default(),
            )
            .map_err(|error| error.message())?,
            "seed write",
        )?;

        let report = upgrade_reinforced_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.8, None, false),
                &RememberWriteControls {
                    reinforce: true,
                    idempotency_key: None,
                    defer_index_processing: false,
                },
            )
            .map_err(|error| error.message())?,
            "reinforce write",
        )?;

        ensure(
            report.memory_id.clone(),
            created.memory_id.to_string(),
            "surviving memory id",
        )?;
        ensure(report.reinforced, true, "reinforced flag")?;
        ensure(report.persisted, true, "persisted flag")?;
        ensure(
            remember_reinforce_should_apply(report.similarity, 0.5),
            true,
            "similarity cleared the threshold",
        )?;

        let connection = open_upgrade_test_db(temp.path())?;
        let memories = connection
            .list_memories(&created.workspace_id, None, true)
            .map_err(|error| error.to_string())?;
        ensure(memories.len(), 1, "no new memory row was created")?;
        let spans = connection
            .list_evidence_spans_for_memory(&created.memory_id.to_string())
            .map_err(|error| error.to_string())?;
        ensure(spans.len(), 1, "evidence span attached to surviving memory")?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_cross_wire_validation_precedes_reinforce_without_mutation() -> TestResult {
        let temp = upgrade_test_workspace()?;
        set_workspace_duplicate_similarity(temp.path(), 0.5)?;
        let content = "Cross-wired reinforcement must leave its target untouched.";
        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.8, None, false),
                &RememberWriteControls::default(),
            )
            .map_err(|error| error.message())?,
            "reinforce guard seed",
        )?;

        let mut cross_wired = upgrade_remember_options(temp.path(), content, 0.8, None, false);
        cross_wired.kind = "semantic";
        match remember_memory_with_controls(
            &cross_wired,
            &RememberWriteControls {
                reinforce: true,
                idempotency_key: None,
                defer_index_processing: false,
            },
        ) {
            Err(error) => ensure(
                error.code(),
                REMEMBER_KIND_IS_LEVEL_CODE,
                "reinforce cross-wire error code",
            )?,
            Ok(outcome) => {
                return Err(format!(
                    "cross-wired reinforcement unexpectedly succeeded: {outcome:?}"
                ));
            }
        }

        let connection = open_upgrade_test_db(temp.path())?;
        let memory = connection
            .get_memory(&created.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "reinforce guard seed missing".to_owned())?;
        ensure(
            (memory.confidence - 0.8).abs() <= 1e-5,
            true,
            "cross-wire rejection leaves confidence unchanged",
        )?;
        ensure(
            connection
                .list_evidence_spans_for_memory(&created.memory_id.to_string())
                .map_err(|error| error.to_string())?
                .len(),
            0,
            "cross-wire rejection writes no evidence span",
        )?;
        ensure(
            connection
                .list_audit_by_action(audit_actions::MEMORY_REINFORCE, None)
                .map_err(|error| error.to_string())?
                .len(),
            0,
            "cross-wire rejection writes no reinforcement audit",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_reinforce_below_threshold_creates_new_memory() -> TestResult {
        let temp = upgrade_test_workspace()?;
        set_workspace_duplicate_similarity(temp.path(), 0.99)?;

        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(
                    temp.path(),
                    "Run cargo fmt --check before cutting a release tag.",
                    0.8,
                    None,
                    false,
                ),
                &RememberWriteControls::default(),
            )
            .map_err(|error| error.message())?,
            "seed write",
        )?;

        let second = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(
                    temp.path(),
                    "Journal retention sweeps tombstone undistilled entries after ninety days.",
                    0.8,
                    None,
                    false,
                ),
                &RememberWriteControls {
                    reinforce: true,
                    idempotency_key: None,
                    defer_index_processing: false,
                },
            )
            .map_err(|error| error.message())?,
            "below-threshold write falls through to create",
        )?;

        let connection = open_upgrade_test_db(temp.path())?;
        let memories = connection
            .list_memories(&created.workspace_id, None, true)
            .map_err(|error| error.to_string())?;
        ensure(memories.len(), 2, "below threshold created a second row")?;
        ensure(
            second.memory_id == created.memory_id,
            false,
            "second row has its own id",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_reinforce_exactly_at_threshold_counts_as_reinforce() -> TestResult {
        ensure(
            remember_reinforce_should_apply(0.92, 0.92),
            true,
            "exactly-at threshold reinforces (>=)",
        )?;
        ensure(
            remember_reinforce_should_apply(0.9199, 0.92),
            false,
            "below threshold falls through",
        )?;
        ensure(
            remember_reinforce_should_apply(0.93, 0.92),
            true,
            "above threshold reinforces",
        )
    }

    #[test]
    fn remember_reinforce_audit_row_shape() -> TestResult {
        let temp = upgrade_test_workspace()?;
        set_workspace_duplicate_similarity(temp.path(), 0.5)?;
        let content = "Audit shape target: verify the reinforce details payload.";

        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.8, None, false),
                &RememberWriteControls::default(),
            )
            .map_err(|error| error.message())?,
            "seed write",
        )?;
        let report = upgrade_reinforced_report(
            remember_memory_with_controls(
                &upgrade_remember_options(
                    temp.path(),
                    content,
                    0.8,
                    Some("file://docs/notes.md#L1"),
                    false,
                ),
                &RememberWriteControls {
                    reinforce: true,
                    idempotency_key: None,
                    defer_index_processing: false,
                },
            )
            .map_err(|error| error.message())?,
            "reinforce write",
        )?;

        let connection = open_upgrade_test_db(temp.path())?;
        let audits = connection
            .list_audit_by_action(audit_actions::MEMORY_REINFORCE, None)
            .map_err(|error| error.to_string())?;
        ensure(audits.len(), 1, "one memory.reinforce audit row")?;
        let audit = &audits[0];
        ensure(
            audit.action.clone(),
            audit_actions::MEMORY_REINFORCE.to_owned(),
            "audit action",
        )?;
        ensure(
            audit.target_type.clone(),
            Some("memory".to_owned()),
            "audit target type",
        )?;
        ensure(
            audit.target_id.clone(),
            Some(created.memory_id.to_string()),
            "audit target id",
        )?;
        let details: serde_json::Value = serde_json::from_str(
            audit
                .details
                .as_deref()
                .ok_or_else(|| "audit details missing".to_owned())?,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            details["schema"].as_str(),
            Some(REMEMBER_REINFORCE_AUDIT_SCHEMA_V1),
            "details schema",
        )?;
        ensure(
            details["similarity"].is_number(),
            true,
            "details similarity",
        )?;
        let source_uris = details["sourceUris"]
            .as_array()
            .ok_or_else(|| "details sourceUris missing".to_owned())?;
        ensure(source_uris.len(), 1, "one source uri folded in")?;
        ensure(
            source_uris[0]
                .as_str()
                .is_some_and(|uri| uri.contains("notes.md")),
            true,
            "source uri content",
        )?;
        ensure(
            details["evidenceSpanId"].as_str(),
            report.evidence_span_id.as_deref(),
            "details evidence span id",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_reinforce_confidence_bump_is_bounded_and_monotonic() -> TestResult {
        let temp = upgrade_test_workspace()?;
        set_workspace_duplicate_similarity(temp.path(), 0.5)?;
        let content = "Confidence bound target: the bump must never exceed one.";

        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.95, None, false),
                &RememberWriteControls::default(),
            )
            .map_err(|error| error.message())?,
            "seed write",
        )?;

        let mut last_confidence = 0.95_f32;
        for round in 0..3 {
            let report = upgrade_reinforced_report(
                remember_memory_with_controls(
                    &upgrade_remember_options(temp.path(), content, 0.95, None, false),
                    &RememberWriteControls {
                        reinforce: true,
                        idempotency_key: None,
                        defer_index_processing: false,
                    },
                )
                .map_err(|error| error.message())?,
                "reinforce write",
            )?;
            ensure(
                report.confidence_after <= 1.0,
                true,
                &format!("round {round}: confidence stays bounded"),
            )?;
            ensure(
                report.confidence_after >= report.confidence_before,
                true,
                &format!("round {round}: bump never decreases within a write"),
            )?;
            ensure(
                report.confidence_after >= last_confidence,
                true,
                &format!("round {round}: repeated reinforce is monotonic non-decreasing"),
            )?;
            last_confidence = report.confidence_after;
        }

        let connection = open_upgrade_test_db(temp.path())?;
        let memory = connection
            .get_memory(&created.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "reinforced memory missing".to_owned())?;
        ensure(
            (memory.confidence - last_confidence).abs() <= 1e-5,
            true,
            "row confidence matches the last reported bump",
        )?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn remember_batch_dry_run_writes_nothing() -> TestResult {
        let temp = upgrade_test_workspace()?;
        let input = concat!(
            "{\"content\":\"Dry-run batch line one.\"}\n",
            "{\"content\":\"Dry-run batch line two.\"}\n",
        );
        let report = remember_memory_batch_stdin(&upgrade_batch_options(temp.path(), true), input)
            .map_err(|error| error.message())?;

        ensure(report.status, "dry_run", "batch status")?;
        ensure(report.dry_run, true, "dry run flag")?;
        ensure(report.stored_count, 2, "previewed line count")?;
        ensure(
            report.results[0].status,
            "would_store",
            "line 1 preview status",
        )?;
        ensure(
            report.results[1].status,
            "would_store",
            "line 2 preview status",
        )?;
        ensure(
            temp.path().join(".ee").join("ee.db").exists(),
            false,
            "dry run never creates the database",
        )
    }

    #[test]
    fn remember_reinforce_dry_run_reports_without_writes() -> TestResult {
        let temp = upgrade_test_workspace()?;
        set_workspace_duplicate_similarity(temp.path(), 0.5)?;
        let content = "Dry-run reinforce target: report only, write nothing.";

        let created = upgrade_created_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.9, None, false),
                &RememberWriteControls::default(),
            )
            .map_err(|error| error.message())?,
            "seed write",
        )?;

        let report = upgrade_reinforced_report(
            remember_memory_with_controls(
                &upgrade_remember_options(temp.path(), content, 0.9, None, true),
                &RememberWriteControls {
                    reinforce: true,
                    idempotency_key: None,
                    defer_index_processing: false,
                },
            )
            .map_err(|error| error.message())?,
            "dry-run reinforce",
        )?;

        ensure(report.dry_run, true, "dry run flag")?;
        ensure(report.persisted, false, "nothing persisted")?;
        ensure(report.evidence_span_id, None, "no evidence span id")?;
        ensure(report.audit_id, None, "no audit id")?;
        ensure(
            report.memory_id.clone(),
            created.memory_id.to_string(),
            "preview names the surviving memory",
        )?;

        let connection = open_upgrade_test_db(temp.path())?;
        let memories = connection
            .list_memories(&created.workspace_id, None, true)
            .map_err(|error| error.to_string())?;
        ensure(memories.len(), 1, "memory row count unchanged")?;
        ensure(
            connection
                .count_evidence_spans_for_workspace(&created.workspace_id)
                .map_err(|error| error.to_string())?,
            0,
            "no evidence spans written",
        )?;
        ensure(
            connection
                .list_audit_by_action(audit_actions::MEMORY_REINFORCE, None)
                .map_err(|error| error.to_string())?
                .len(),
            0,
            "no reinforce audit rows written",
        )?;
        let memory = connection
            .get_memory(&created.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "seed memory missing".to_owned())?;
        ensure(
            (memory.confidence - 0.9).abs() <= 1e-5,
            true,
            "confidence unchanged by dry run",
        )?;
        connection.close().map_err(|error| error.to_string())
    }
}
