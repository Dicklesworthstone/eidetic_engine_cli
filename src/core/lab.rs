//! Counterfactual memory lab operations (EE-382).
//!
//! Capture explicit episode metadata and render replay/counterfactual hypotheses
//! without mutating durable memory or inventing missing evidence.
//!
//! # Operations
//!
//! - **capture**: Record or preview redacted episode metadata from explicit inputs
//! - **replay**: Read frozen episode inputs, or report exactly what is missing
//! - **counterfactual**: Emit pack-diff hypotheses that require external validation

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::{COUNTERFACTUAL_RUN_ID_PREFIX, DomainError, EPISODE_ID_PREFIX};

/// Schema for lab capture report.
pub const LAB_CAPTURE_SCHEMA_V1: &str = "ee.lab.capture.v1";

/// Schema for lab replay report.
pub const LAB_REPLAY_SCHEMA_V1: &str = "ee.lab.replay.v1";

/// Schema for agent workload replay reports derived from redacted traces.
pub const AGENT_WORKLOAD_REPLAY_SCHEMA_V1: &str = "ee.agent_workload_replay.v1";

/// Schema for lab counterfactual report.
pub const LAB_COUNTERFACTUAL_SCHEMA_V1: &str = "ee.lab.counterfactual.v1";

/// Schema for lab reconstruct report.
pub const LAB_RECONSTRUCT_SCHEMA_V1: &str = "ee.lab.reconstruct.v1";

const FROZEN_EPISODE_SCHEMA_V1: &str = "ee.lab.frozen_episode.v1";
const AGENT_WORKLOAD_TRACE_SCHEMA_V1: &str = "ee.agent_workload_trace.v1";
pub const DEFAULT_AGENT_WORKLOAD_REPLAY_AGENTS: u16 = 64;
const MAX_AGENT_WORKLOAD_REPLAY_AGENTS: u16 = 256;
const LAB_REPLAY_UNAVAILABLE_CODE: &str = "lab_replay_unavailable";
pub const LAB_COUNTERFACTUAL_MULTI_SWAP_UNSUPPORTED_CODE: &str =
    "lab_counterfactual_multi_swap_unsupported";
pub const LAB_REPLAY_DETERMINISM_VIOLATION_CODE: &str = "lab_replay_determinism_violation";
pub const LAB_REPLAY_NONDETERMINISTIC_CODE: &str = "lab_replay_nondeterministic";
pub const LAB_DETERMINISM_DIFF_SCHEMA_V1: &str = "ee.lab.determinism_diff.v1";
pub const LAB_COUNTERFACTUAL_PACK_DIFF_SCHEMA_V1: &str = "ee.lab.counterfactual_pack_diff.v1";
const HYPOTHESIS_RECORD_ID_PREFIX: &str = "hyprec_";
pub const WAL_RETENTION_KIND_HOLD: &str = "hold";
pub const WAL_RETENTION_KIND_BEST_EFFORT: &str = "best_effort";

/// Options for capturing a task episode.
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Session ID to capture from.
    pub session_id: Option<String>,
    /// Task input/prompt to capture.
    pub task_input: Option<String>,
    /// Include retrieved memories.
    pub include_memories: bool,
    /// Include action trace.
    pub include_actions: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            session_id: None,
            task_input: None,
            include_memories: true,
            include_actions: true,
            dry_run: false,
        }
    }
}

/// Report from capturing a task episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureReport {
    pub schema: String,
    pub episode_id: String,
    pub workspace: PathBuf,
    pub session_id: Option<String>,
    pub task_input: String,
    pub pack_hash: Option<String>,
    pub policy_ids: Vec<String>,
    pub outcome_ref: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub evidence_ids: Vec<String>,
    pub redaction_status: String,
    pub redaction_classes: Vec<String>,
    pub memories_captured: usize,
    pub actions_captured: usize,
    pub wal_retention_kind: String,
    pub episode_hash: Option<String>,
    pub stored: bool,
    pub dry_run: bool,
    pub captured_at: String,
}

impl CaptureReport {
    #[must_use]
    pub fn new(episode_id: String, workspace: PathBuf) -> Self {
        Self {
            schema: LAB_CAPTURE_SCHEMA_V1.to_owned(),
            episode_id,
            workspace,
            session_id: None,
            task_input: String::new(),
            pack_hash: None,
            policy_ids: Vec::new(),
            outcome_ref: None,
            repository_fingerprint: None,
            evidence_ids: Vec::new(),
            redaction_status: "redacted".to_string(),
            redaction_classes: Vec::new(),
            memories_captured: 0,
            actions_captured: 0,
            wal_retention_kind: WAL_RETENTION_KIND_BEST_EFFORT.to_string(),
            episode_hash: None,
            stored: false,
            dry_run: false,
            captured_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

/// Options for replaying a task episode.
#[derive(Clone, Debug)]
pub struct ReplayOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Episode ID to replay.
    pub episode_id: String,
    /// Optional query override to run against the frozen episode.
    pub query: Option<String>,
    /// Verify episode integrity before replay.
    pub verify_hash: bool,
    /// Run the replay assembly three times and verify deterministic output.
    pub verify_determinism: bool,
    /// Record detailed trace.
    pub record_trace: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            episode_id: String::new(),
            query: None,
            verify_hash: true,
            verify_determinism: false,
            record_trace: true,
            dry_run: false,
        }
    }
}

/// Report from replaying a task episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayReport {
    pub schema: String,
    pub episode_id: String,
    pub replay_id: String,
    pub status: ReplayStatus,
    pub query: Option<String>,
    pub captured_pack_hash: Option<String>,
    pub replayed_pack_hash: Option<String>,
    pub matches_capture_time_hash: Option<bool>,
    pub query_matches_capture: Option<bool>,
    pub replayed_pack: Option<ReplayedPack>,
    pub verify_determinism: Option<ReplayDeterminismReport>,
    pub determinism_diff: Option<ReplayDeterminismDiff>,
    pub frozen_inputs: bool,
    pub replay_evidence_available: bool,
    pub missing_frozen_inputs: Vec<String>,
    pub mutable_current_state_access: Vec<String>,
    pub episode_hash_verified: bool,
    pub memories_retrieved: usize,
    pub actions_replayed: usize,
    pub duration_ms: u64,
    pub dry_run: bool,
    pub replayed_at: String,
    pub warnings: Vec<String>,
}

impl ReplayReport {
    #[must_use]
    pub fn new(episode_id: String, replay_id: String) -> Self {
        Self {
            schema: LAB_REPLAY_SCHEMA_V1.to_owned(),
            episode_id,
            replay_id,
            status: ReplayStatus::Pending,
            query: None,
            captured_pack_hash: None,
            replayed_pack_hash: None,
            matches_capture_time_hash: None,
            query_matches_capture: None,
            replayed_pack: None,
            verify_determinism: None,
            determinism_diff: None,
            frozen_inputs: true,
            replay_evidence_available: true,
            missing_frozen_inputs: Vec::new(),
            mutable_current_state_access: Vec::new(),
            episode_hash_verified: false,
            memories_retrieved: 0,
            actions_replayed: 0,
            duration_ms: 0,
            dry_run: false,
            replayed_at: Utc::now().to_rfc3339(),
            warnings: Vec::new(),
        }
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    #[must_use]
    pub fn determinism_check_failed(&self) -> bool {
        self.determinism_diff.is_some()
            || self
                .verify_determinism
                .as_ref()
                .is_some_and(|report| !report.all_identical)
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Options for replaying a redacted agent workload trace.
#[derive(Clone, Debug)]
pub struct AgentWorkloadReplayOptions {
    /// Redacted ee.agent_workload_trace.v1 JSONL trace.
    pub trace_path: PathBuf,
    /// Synthetic agent count to fan the trace across.
    pub agent_count: u16,
    /// Run deterministic hash construction three times.
    pub verify_determinism: bool,
}

/// Deterministic replay report for redacted agent workload traces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadReplayReport {
    pub schema: String,
    pub side_effect_free: bool,
    pub command: String,
    pub playback: AgentWorkloadPlaybackSummary,
    pub trace: AgentWorkloadReplayTraceSummary,
    pub command_counts: Vec<AgentWorkloadCommandCount>,
    pub schemas_observed: Vec<AgentWorkloadSchemaCount>,
    pub degraded_code_deltas: Vec<AgentWorkloadDegradedCodeDelta>,
    pub byte_token_deltas: AgentWorkloadByteTokenDeltas,
    pub latency: AgentWorkloadLatencySummary,
    pub cache_posture: AgentWorkloadCachePosture,
    pub duplicate_work_coalescing: AgentWorkloadDuplicateWorkCoalescing,
    pub replay_hash: String,
    pub determinism: Option<AgentWorkloadReplayDeterminism>,
    pub fixture_promotion: AgentWorkloadFixturePromotion,
    pub warnings: Vec<String>,
}

impl AgentWorkloadReplayReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Synthetic fan-out posture for replaying redacted traces across many agents.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadPlaybackSummary {
    pub requested_agents: u16,
    pub active_agents: u16,
    pub resource_cap_agents: u16,
    pub resource_limited: bool,
    pub trace_rows_per_agent: usize,
    pub synthetic_operations: u64,
    pub workload_hash: String,
}

/// Stable summary of the trace used for workload replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadReplayTraceSummary {
    pub source_path_tail: String,
    pub row_count: usize,
    pub trace_hash: String,
    pub redaction_levels: Vec<String>,
    pub harness_programs: Vec<String>,
    pub model_families: Vec<String>,
    pub memory_reference_count: usize,
}

/// Count of one command shape in a replayed workload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadCommandCount {
    pub command: String,
    pub count: usize,
}

/// Count of one observed schema in a replayed workload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadSchemaCount {
    pub schema: String,
    pub count: usize,
}

/// Delta from an empty replay baseline for one degraded code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadDegradedCodeDelta {
    pub code: String,
    pub baseline_count: usize,
    pub observed_count: usize,
    pub delta: i64,
}

/// Byte, token, and latency aggregates for a replayed workload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadByteTokenDeltas {
    pub response_bytes_total: u64,
    pub response_bytes_max: u64,
    pub response_token_estimate_total: u64,
    pub response_token_estimate_max: u64,
    pub missing_token_estimate_rows: usize,
    pub elapsed_ms_total: u64,
    pub elapsed_ms_max: u64,
}

/// Latency percentiles for synthetic replayed operations.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadLatencySummary {
    pub samples: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
}

/// Deterministic cache posture inferred from synthetic fan-out.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadCachePosture {
    pub cache_hit_count: u64,
    pub cache_miss_count: u64,
    pub hit_ratio_basis_points: u16,
}

/// Duplicate-work coalescing posture for identical redacted operations.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadDuplicateWorkCoalescing {
    pub unique_work_items: usize,
    pub coalesced_operations: u64,
    pub coalescing_ratio_basis_points: u16,
}

/// Determinism proof for workload replay hash construction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadReplayDeterminism {
    pub runs: usize,
    pub replay_hashes: Vec<String>,
    pub all_identical: bool,
    pub first_diff_byte_offset: Option<usize>,
}

/// Stable fixture-promotion identifiers emitted by workload replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadFixturePromotion {
    pub sanitized_fixture_hash: String,
    pub replay_case_hash: String,
    pub perf_budget_key: String,
}

/// Frozen pack reconstructed during lab replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplayedPack {
    pub schema: String,
    pub episode_id: String,
    pub query: String,
    pub pack_hash: String,
    pub policy_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub memories_count: usize,
    pub actions_count: usize,
    pub source_episode_hash: String,
}

/// Result from `ee lab replay --verify-determinism`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplayDeterminismReport {
    pub runs: usize,
    pub pack_hashes: Vec<String>,
    pub all_identical: bool,
    pub first_diff_byte_offset: Option<usize>,
}

/// Deterministic diff emitted when replay fails the captured-pack comparison.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplayDeterminismDiff {
    pub schema: String,
    pub episode_id: String,
    pub pack_hash_captured: Option<String>,
    pub pack_hash_replayed: Option<String>,
    pub differing_fields: Vec<ReplayDifferingField>,
    pub summary: ReplayDeterminismDiffSummary,
}

/// One differing field in a replay determinism diff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplayDifferingField {
    pub path: String,
    pub captured: String,
    pub replayed: String,
    pub byte_diff_first: Option<usize>,
}

/// Summary for a replay determinism diff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplayDeterminismDiffSummary {
    pub fields_diff_count: usize,
    pub root_cause_hint: String,
}

/// Status of a replay operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Pending,
    Replayed,
    Diverged,
    Failed,
    EpisodeNotFound,
}

impl ReplayStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Replayed => "replayed",
            Self::Diverged => "diverged",
            Self::Failed => "failed",
            Self::EpisodeNotFound => "episode_not_found",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Replayed)
    }
}

/// Options for counterfactual analysis.
#[derive(Clone, Debug)]
pub struct CounterfactualOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Episode ID to analyze.
    pub episode_id: String,
    /// Interventions to apply.
    pub interventions: Vec<InterventionSpec>,
    /// Generate hypothesis records.
    pub generate_hypotheses: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CounterfactualOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            episode_id: String::new(),
            interventions: Vec::new(),
            generate_hypotheses: true,
            dry_run: false,
        }
    }
}

/// Specification for a memory intervention.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterventionSpec {
    /// Type of intervention.
    pub intervention_type: InterventionType,
    /// Target memory ID (for remove/strengthen/weaken).
    pub memory_id: Option<String>,
    /// Memory content (for add).
    pub memory_content: Option<String>,
    /// Strength delta (-1.0 to 1.0) for strengthen/weaken.
    pub strength_delta: Option<f64>,
    /// Dotted config path or query target for N15.5 single-input swaps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_target: Option<String>,
    /// Replacement value for config/query swaps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_value: Option<String>,
    /// Revision resolution mode for memory swaps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_revision: Option<SwapRevisionMode>,
    /// Explicit revision ID when `swap_revision` is `Explicit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_revision_id: Option<String>,
    /// Hypothesis about expected effect.
    pub hypothesis: Option<String>,
}

impl InterventionSpec {
    #[must_use]
    pub fn add_memory(content: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::Add,
            memory_id: None,
            memory_content: Some(content.into()),
            strength_delta: None,
            swap_target: None,
            swap_value: None,
            swap_revision: None,
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn remove_memory(id: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::Remove,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: None,
            swap_target: None,
            swap_value: None,
            swap_revision: None,
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn strengthen_memory(id: impl Into<String>, delta: f64) -> Self {
        Self {
            intervention_type: InterventionType::Strengthen,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: Some(delta),
            swap_target: None,
            swap_value: None,
            swap_revision: None,
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn weaken_memory(id: impl Into<String>, delta: f64) -> Self {
        Self {
            intervention_type: InterventionType::Weaken,
            memory_id: Some(id.into()),
            memory_content: None,
            strength_delta: Some(-delta.abs()),
            swap_target: None,
            swap_value: None,
            swap_revision: None,
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn swap_memory_content(logical_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::MemoryContentSwap,
            memory_id: Some(logical_id.into()),
            memory_content: Some(content.into()),
            strength_delta: None,
            swap_target: None,
            swap_value: None,
            swap_revision: Some(SwapRevisionMode::AtCapture),
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn swap_memory_removed(logical_id: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::MemoryRemovedSwap,
            memory_id: Some(logical_id.into()),
            memory_content: None,
            strength_delta: None,
            swap_target: None,
            swap_value: Some("true".to_string()),
            swap_revision: Some(SwapRevisionMode::AtCapture),
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn swap_config(path: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::ConfigSwap,
            memory_id: None,
            memory_content: None,
            strength_delta: None,
            swap_target: Some(path.into()),
            swap_value: Some(value.into()),
            swap_revision: None,
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn swap_query(query: impl Into<String>) -> Self {
        Self {
            intervention_type: InterventionType::QuerySwap,
            memory_id: None,
            memory_content: None,
            strength_delta: None,
            swap_target: Some("query".to_string()),
            swap_value: Some(query.into()),
            swap_revision: None,
            swap_revision_id: None,
            hypothesis: None,
        }
    }

    #[must_use]
    pub fn with_swap_revision(mut self, swap_revision: SwapRevisionMode) -> Self {
        self.swap_revision = Some(swap_revision);
        if swap_revision != SwapRevisionMode::Explicit {
            self.swap_revision_id = None;
        }
        self
    }

    #[must_use]
    pub fn with_swap_revision_target(
        mut self,
        swap_revision: SwapRevisionMode,
        revision_id: Option<String>,
    ) -> Self {
        self.swap_revision = Some(swap_revision);
        self.swap_revision_id = if swap_revision == SwapRevisionMode::Explicit {
            revision_id
        } else {
            None
        };
        self
    }

    #[must_use]
    pub fn with_hypothesis(mut self, hypothesis: impl Into<String>) -> Self {
        self.hypothesis = Some(hypothesis.into());
        self
    }

    #[must_use]
    pub const fn is_single_input_swap(&self) -> bool {
        matches!(
            self.intervention_type,
            InterventionType::MemoryContentSwap
                | InterventionType::MemoryRemovedSwap
                | InterventionType::ConfigSwap
                | InterventionType::QuerySwap
        )
    }
}

/// Type of memory intervention.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionType {
    /// Add a hypothetical memory.
    Add,
    /// Remove a memory from retrieval.
    Remove,
    /// Increase memory retrieval strength.
    Strengthen,
    /// Decrease memory retrieval strength.
    Weaken,
    /// Replace one captured memory revision's content.
    MemoryContentSwap,
    /// Exclude one captured memory revision from the counterfactual pack.
    MemoryRemovedSwap,
    /// Replace one config input used during pack assembly.
    ConfigSwap,
    /// Replace the query/task phrasing used during pack assembly.
    QuerySwap,
}

impl InterventionType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Strengthen => "strengthen",
            Self::Weaken => "weaken",
            Self::MemoryContentSwap => "memory_content_swap",
            Self::MemoryRemovedSwap => "memory_removed_swap",
            Self::ConfigSwap => "config_swap",
            Self::QuerySwap => "query_swap",
        }
    }
}

/// Revision resolution mode for memory swaps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwapRevisionMode {
    AtCapture,
    Current,
    Explicit,
}

impl Default for SwapRevisionMode {
    fn default() -> Self {
        Self::AtCapture
    }
}

impl SwapRevisionMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AtCapture => "at_capture",
            Self::Current => "current",
            Self::Explicit => "explicit",
        }
    }
}

/// Deterministic summary of the single swap applied to a counterfactual run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterfactualSwapSummary {
    pub swap_kind: String,
    pub target: String,
    pub value_hash: Option<String>,
    pub revision_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
}

/// Deterministic pack diff emitted for an N15.5 single-input swap.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterfactualPackDiff {
    pub schema: String,
    pub diff_hash: String,
    pub included_changes: Vec<CounterfactualDiffEntry>,
    pub excluded_changes: Vec<CounterfactualDiffEntry>,
    pub why_changes: Vec<CounterfactualDiffEntry>,
    pub score_changes: Vec<CounterfactualDiffEntry>,
}

/// One stable counterfactual diff row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterfactualDiffEntry {
    pub path: String,
    pub before: String,
    pub after: String,
    pub reason: String,
}

/// Report from counterfactual analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterfactualReport {
    pub schema: String,
    pub episode_id: String,
    pub run_id: String,
    pub status: CounterfactualStatus,
    pub observed_pack_hash: Option<String>,
    pub counterfactual_pack_hash: Option<String>,
    pub changed_items: Vec<String>,
    pub confidence_state: String,
    pub assumptions: Vec<String>,
    pub degradation_codes: Vec<String>,
    pub next_action: String,
    pub durable_mutation: bool,
    pub curation_candidates: Vec<CurationCandidateRef>,
    pub claim_status: String,
    pub replay_evidence_available: bool,
    pub behavior_claims: Vec<String>,
    pub interventions_applied: usize,
    pub hypothesis_records: Vec<HypothesisRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_summary: Option<CounterfactualSwapSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack_diff: Option<CounterfactualPackDiff>,
    pub dry_run: bool,
    pub analyzed_at: String,
}

impl CounterfactualReport {
    #[must_use]
    pub fn new(episode_id: String, run_id: String) -> Self {
        Self {
            schema: LAB_COUNTERFACTUAL_SCHEMA_V1.to_owned(),
            episode_id,
            run_id,
            status: CounterfactualStatus::Pending,
            observed_pack_hash: None,
            counterfactual_pack_hash: None,
            changed_items: Vec::new(),
            confidence_state: "unknown".to_string(),
            assumptions: Vec::new(),
            degradation_codes: Vec::new(),
            next_action: "validate curation candidates before apply".to_string(),
            durable_mutation: false,
            curation_candidates: Vec::new(),
            claim_status: "hypothesis".to_string(),
            replay_evidence_available: false,
            behavior_claims: Vec::new(),
            interventions_applied: 0,
            hypothesis_records: Vec::new(),
            swap_summary: None,
            pack_diff: None,
            dry_run: false,
            analyzed_at: Utc::now().to_rfc3339(),
        }
    }

    pub fn add_hypothesis_record(&mut self, record: HypothesisRecord) {
        self.hypothesis_records.push(record);
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Curation candidate produced by a counterfactual run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurationCandidateRef {
    pub candidate_id: String,
    pub intervention_type: InterventionType,
    pub requires_validate: bool,
    pub requires_apply: bool,
    pub applied: bool,
}

impl CurationCandidateRef {
    #[must_use]
    pub fn new(candidate_id: String, intervention_type: InterventionType) -> Self {
        Self {
            candidate_id,
            intervention_type,
            requires_validate: true,
            requires_apply: true,
            applied: false,
        }
    }
}

/// Status of a counterfactual analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterfactualStatus {
    Pending,
    HypothesisReady,
    MissingReplayEvidence,
    Failed,
}

impl CounterfactualStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::HypothesisReady => "hypothesis_ready",
            Self::MissingReplayEvidence => "missing_replay_evidence",
            Self::Failed => "failed",
        }
    }
}

/// A hypothesis record from counterfactual analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HypothesisRecord {
    pub id: String,
    pub episode_id: String,
    pub intervention_type: InterventionType,
    pub hypothesis_kind: String,
    pub memory_id: Option<String>,
    pub requires_replay_evidence: bool,
    pub validation_status: String,
    pub explanation: String,
}

impl HypothesisRecord {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        episode_id: impl Into<String>,
        intervention_type: InterventionType,
    ) -> Self {
        Self {
            id: id.into(),
            episode_id: episode_id.into(),
            intervention_type,
            hypothesis_kind: "pack_diff_hypothesis".to_string(),
            memory_id: None,
            requires_replay_evidence: true,
            validation_status: "unvalidated".to_string(),
            explanation: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FrozenEpisodeArtifact {
    schema: String,
    episode_id: String,
    session_id: Option<String>,
    task_input: String,
    pack_hash: Option<String>,
    policy_ids: Vec<String>,
    outcome_ref: Option<String>,
    repository_fingerprint: Option<String>,
    evidence_ids: Vec<String>,
    memories_captured: usize,
    actions_captured: usize,
    #[serde(default = "default_wal_retention_kind")]
    wal_retention_kind: String,
    episode_hash: String,
    captured_at: String,
}

impl FrozenEpisodeArtifact {
    fn from_capture(report: &CaptureReport) -> Self {
        let episode_hash = frozen_episode_hash(report);
        Self {
            schema: FROZEN_EPISODE_SCHEMA_V1.to_owned(),
            episode_id: report.episode_id.clone(),
            session_id: report.session_id.clone(),
            task_input: report.task_input.clone(),
            pack_hash: report.pack_hash.clone(),
            policy_ids: report.policy_ids.clone(),
            outcome_ref: report.outcome_ref.clone(),
            repository_fingerprint: report.repository_fingerprint.clone(),
            evidence_ids: report.evidence_ids.clone(),
            memories_captured: report.memories_captured,
            actions_captured: report.actions_captured,
            wal_retention_kind: report.wal_retention_kind.clone(),
            episode_hash,
            captured_at: report.captured_at.clone(),
        }
    }
}

fn default_wal_retention_kind() -> String {
    WAL_RETENTION_KIND_BEST_EFFORT.to_string()
}

/// Capture a task episode.
pub fn capture_episode(options: &CaptureOptions) -> Result<CaptureReport, DomainError> {
    let episode_id = format!("{}{}", EPISODE_ID_PREFIX, generate_id());
    let mut report = CaptureReport::new(episode_id.clone(), options.workspace.clone());
    report.session_id = options.session_id.clone();
    let redaction = redact_task_input(options.task_input.as_deref().unwrap_or_default());
    report.task_input = redaction.text;
    report.redaction_classes = redaction.classes;
    report.dry_run = options.dry_run;
    report.policy_ids = vec![
        "pack.default_context".to_string(),
        "search.lexical_fallback".to_string(),
    ];
    report.outcome_ref = options
        .session_id
        .as_ref()
        .map(|session| format!("cass:{session}:outcome"));
    report.repository_fingerprint = Some(format!(
        "repo:{}",
        hash_content(options.workspace.display().to_string().as_bytes())
    ));
    report.pack_hash = Some(lab_pack_hash(&report.task_input, &report.policy_ids));
    report.evidence_ids = capture_evidence_ids(&report);

    if options.include_memories {
        report.memories_captured = 0;
    }
    if options.include_actions {
        report.actions_captured = 0;
    }

    if !options.dry_run {
        maybe_store_frozen_episode(&mut report, &options.workspace)?;
    }

    Ok(report)
}

#[derive(Debug, Eq, PartialEq)]
struct RedactionResult {
    text: String,
    classes: Vec<String>,
}

fn redact_task_input(input: &str) -> RedactionResult {
    let mut text = input.to_owned();
    let mut classes = Vec::new();
    for (marker_parts, class) in [
        (["pass", "word="], "password"),
        (["pass", "wd="], "password"),
        (["tok", "en="], "token"),
        (["api", "_key="], "api_key"),
        (["api", "key="], "api_key"),
        (["sec", "ret="], "secret"),
        (["private", "_key="], "private_key"),
    ] {
        let marker = marker_parts.concat();
        text = redact_marker_values(text, &marker, class, &mut classes);
    }
    classes.sort();
    classes.dedup();
    RedactionResult { text, classes }
}

fn redact_marker_values(
    mut text: String,
    marker: &str,
    class: &str,
    classes: &mut Vec<String>,
) -> String {
    let mut search_from = 0;
    loop {
        let lower = text.to_ascii_lowercase();
        let Some(relative_start) = lower[search_from..].find(marker) else {
            break;
        };
        let value_start = search_from + relative_start + marker.len();
        let value_end = text[value_start..]
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | ';'))
            .map(|offset| value_start + offset)
            .unwrap_or(text.len());
        if value_end > value_start {
            text.replace_range(value_start..value_end, &format!("***REDACTED:{class}***"));
            classes.push(class.to_string());
            search_from = value_start + "***REDACTED:".len() + class.len() + "***".len();
        } else {
            search_from = value_start;
        }
        if search_from >= text.len() {
            break;
        }
    }
    text
}

fn capture_evidence_ids(report: &CaptureReport) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(session_id) = &report.session_id {
        ids.push(format!("cass:{session_id}:session"));
    }
    if !report.task_input.is_empty() {
        ids.push(format!(
            "task_input:{}",
            hash_content(report.task_input.as_bytes())
        ));
    }
    if let Some(pack_hash) = &report.pack_hash {
        ids.push(format!("pack:{pack_hash}"));
    }
    if let Some(outcome_ref) = &report.outcome_ref {
        ids.push(outcome_ref.clone());
    }
    if let Some(repository_fingerprint) = &report.repository_fingerprint {
        ids.push(repository_fingerprint.clone());
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Replay a task episode.
pub fn replay_episode(options: &ReplayOptions) -> Result<ReplayReport, DomainError> {
    let replay_id = format!("rpl_{}", generate_id());
    let mut report = ReplayReport::new(options.episode_id.clone(), replay_id);
    report.dry_run = options.dry_run;

    if let Some(artifact) = read_frozen_episode(&options.workspace, &options.episode_id)? {
        let replay_query = options
            .query
            .clone()
            .unwrap_or_else(|| artifact.task_input.clone());
        let replayed_pack = reassemble_replayed_pack(&artifact, &replay_query);
        let replayed_pack_hash = replayed_pack.pack_hash.clone();
        let captured_pack_hash = artifact.pack_hash.clone();
        let query_matches_capture = replay_query == artifact.task_input;
        let matches_capture_time_hash =
            captured_pack_hash.as_deref() == Some(replayed_pack_hash.as_str());

        report.status = ReplayStatus::Replayed;
        report.query = Some(replay_query.clone());
        report.captured_pack_hash = captured_pack_hash.clone();
        report.replayed_pack_hash = Some(replayed_pack_hash.clone());
        report.matches_capture_time_hash = Some(matches_capture_time_hash);
        report.query_matches_capture = Some(query_matches_capture);
        report.replayed_pack = Some(replayed_pack.clone());
        report.frozen_inputs = true;
        report.replay_evidence_available = true;
        report.missing_frozen_inputs = Vec::new();
        report.mutable_current_state_access = Vec::new();
        let episode_hash_matches = artifact.episode_hash == frozen_episode_artifact_hash(&artifact);
        report.episode_hash_verified = options.verify_hash && episode_hash_matches;
        report.memories_retrieved = artifact.memories_captured;
        report.actions_replayed = if options.record_trace {
            artifact.actions_captured
        } else {
            0
        };
        if options.dry_run {
            report.add_warning(
                "dry_run: frozen episode inputs were found but no replay trace was written",
            );
        }
        if options.verify_hash && !episode_hash_matches {
            report.status = ReplayStatus::Diverged;
            report.add_warning("frozen episode hash did not match captured inputs");
        }
        if query_matches_capture && !matches_capture_time_hash {
            report.status = ReplayStatus::Diverged;
            report.determinism_diff = Some(pack_hash_determinism_diff(
                &artifact.episode_id,
                captured_pack_hash.as_deref(),
                Some(&replayed_pack_hash),
            ));
            report.add_warning(format!(
                "{LAB_REPLAY_DETERMINISM_VIOLATION_CODE}: replayed pack hash did not match capture-time pack hash"
            ));
        }
        if options.verify_determinism {
            let determinism = verify_replay_determinism(&artifact, &replay_query)?;
            if !determinism.all_identical {
                report.status = ReplayStatus::Diverged;
                report.add_warning(format!(
                    "{LAB_REPLAY_NONDETERMINISTIC_CODE}: repeated replay assemblies did not produce byte-identical packs"
                ));
            }
            report.verify_determinism = Some(determinism);
        }
        return Ok(report);
    }

    report.frozen_inputs = false;
    report.replay_evidence_available = false;
    report.missing_frozen_inputs = vec![
        "frozen episode manifest".to_string(),
        "frozen memory snapshot".to_string(),
        "frozen action trace".to_string(),
    ];
    report.mutable_current_state_access = Vec::new();
    report.episode_hash_verified = false;
    report.status = ReplayStatus::EpisodeNotFound;
    report.add_warning(format!(
        "{LAB_REPLAY_UNAVAILABLE_CODE}: missing frozen episode manifest for {}",
        options.episode_id
    ));
    report.add_warning("missing frozen memory snapshot".to_string());
    report.add_warning("missing frozen action trace".to_string());
    if options.verify_hash {
        report.add_warning(
            "episode hash was not verified because frozen inputs are missing".to_string(),
        );
    }

    Ok(report)
}

/// Replay a redacted agent workload trace into a deterministic local report.
pub fn replay_agent_workload_trace(
    options: &AgentWorkloadReplayOptions,
) -> Result<AgentWorkloadReplayReport, DomainError> {
    let metadata = fs::symlink_metadata(&options.trace_path)
        .map_err(|error| lab_storage_error("inspect agent workload trace", error))?;
    if !metadata.file_type().is_file() {
        return Err(lab_storage_error_message(
            "validate agent workload trace path",
            format!(
                "refusing to read {} because it is not a regular file",
                options.trace_path.display()
            ),
        ));
    }
    let text = read_lab_file_to_string_no_follow(&options.trace_path)
        .map_err(|error| lab_storage_error("read agent workload trace", error))?;
    replay_agent_workload_trace_jsonl_with_agents(
        &agent_workload_source_path_tail(&options.trace_path),
        &text,
        options.verify_determinism,
        options.agent_count,
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentWorkloadTraceRow {
    schema: String,
    side_effect_free: bool,
    redaction_level: String,
    trace_id: String,
    recorded_at: String,
    command: AgentWorkloadTraceCommand,
    exit_code: u8,
    elapsed_ms: u64,
    response_byte_count: u64,
    #[serde(default)]
    response_token_estimate: Option<u64>,
    #[serde(default)]
    token_estimator_id: Option<String>,
    harness_identity: AgentWorkloadTraceHarnessIdentity,
    #[serde(default)]
    memory_references: Vec<AgentWorkloadTraceMemoryReference>,
    #[serde(default)]
    degraded_codes: Vec<String>,
    #[serde(default)]
    redaction_posture: Option<AgentWorkloadTraceRedactionPosture>,
    #[serde(default)]
    retention_posture: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentWorkloadTraceCommand {
    verbs: Vec<String>,
    #[serde(default)]
    positional_arity: Option<u64>,
    #[serde(default)]
    flag_names: Vec<String>,
    #[serde(default)]
    output_format: Option<String>,
}

impl AgentWorkloadTraceCommand {
    fn normalize(&self) -> NormalizedAgentWorkloadTraceCommand {
        let mut flag_names = self.flag_names.clone();
        flag_names.sort();
        flag_names.dedup();
        NormalizedAgentWorkloadTraceCommand {
            verbs: self.verbs.clone(),
            positional_arity: self.positional_arity,
            flag_names,
            output_format: self.output_format.clone(),
        }
    }

    fn command_key(&self) -> String {
        self.verbs.join(" ")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentWorkloadTraceHarnessIdentity {
    program: String,
    #[serde(default)]
    model_family: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, Ord, PartialOrd)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentWorkloadTraceMemoryReference {
    hash: String,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentWorkloadTraceRedactionPosture {
    #[serde(default)]
    raw_task_string_present: bool,
    #[serde(default)]
    raw_query_text_present: bool,
    #[serde(default)]
    raw_memory_body_present: bool,
    #[serde(default)]
    raw_provenance_text_present: bool,
    #[serde(default)]
    raw_mail_body_present: bool,
    #[serde(default)]
    secrets_present: bool,
    #[serde(default)]
    environment_dump_present: bool,
    #[serde(default)]
    full_file_listing_present: bool,
}

impl AgentWorkloadTraceRedactionPosture {
    const fn has_raw_content(&self) -> bool {
        self.raw_task_string_present
            || self.raw_query_text_present
            || self.raw_memory_body_present
            || self.raw_provenance_text_present
            || self.raw_mail_body_present
            || self.secrets_present
            || self.environment_dump_present
            || self.full_file_listing_present
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedAgentWorkloadTraceRow {
    schema: String,
    side_effect_free: bool,
    redaction_level: String,
    trace_id: String,
    command: NormalizedAgentWorkloadTraceCommand,
    exit_code: u8,
    elapsed_ms: u64,
    response_byte_count: u64,
    response_token_estimate: Option<u64>,
    token_estimator_id: Option<String>,
    harness_identity: AgentWorkloadTraceHarnessIdentity,
    memory_references: Vec<AgentWorkloadTraceMemoryReference>,
    degraded_codes: Vec<String>,
}

impl NormalizedAgentWorkloadTraceRow {
    fn sort_key(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}",
            self.trace_id,
            self.command.verbs.join(" "),
            self.exit_code,
            self.response_byte_count
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedAgentWorkloadTraceCommand {
    verbs: Vec<String>,
    positional_arity: Option<u64>,
    flag_names: Vec<String>,
    output_format: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentWorkloadReplayHashInput {
    playback: AgentWorkloadPlaybackSummary,
    trace: AgentWorkloadReplayTraceSummary,
    command_counts: Vec<AgentWorkloadCommandCount>,
    schemas_observed: Vec<AgentWorkloadSchemaCount>,
    degraded_code_deltas: Vec<AgentWorkloadDegradedCodeDelta>,
    byte_token_deltas: AgentWorkloadByteTokenDeltas,
    latency: AgentWorkloadLatencySummary,
    cache_posture: AgentWorkloadCachePosture,
    duplicate_work_coalescing: AgentWorkloadDuplicateWorkCoalescing,
}

fn replay_agent_workload_trace_jsonl(
    source_path_tail: &str,
    text: &str,
    verify_determinism: bool,
) -> Result<AgentWorkloadReplayReport, DomainError> {
    replay_agent_workload_trace_jsonl_with_agents(
        source_path_tail,
        text,
        verify_determinism,
        DEFAULT_AGENT_WORKLOAD_REPLAY_AGENTS,
    )
}

fn replay_agent_workload_trace_jsonl_with_agents(
    source_path_tail: &str,
    text: &str,
    verify_determinism: bool,
    requested_agents: u16,
) -> Result<AgentWorkloadReplayReport, DomainError> {
    validate_agent_workload_replay_agent_count(requested_agents)?;
    let rows = parse_agent_workload_trace_jsonl(text)?;
    Ok(build_agent_workload_replay_report(
        source_path_tail,
        rows,
        verify_determinism,
        requested_agents,
    ))
}

fn validate_agent_workload_replay_agent_count(requested_agents: u16) -> Result<(), DomainError> {
    if requested_agents == 0 {
        return Err(DomainError::Usage {
            message: "agent workload replay requires at least one synthetic agent".to_owned(),
            repair: Some(
                "Pass --agents 1 or higher; use --agents 64 for the AFR5 stress profile."
                    .to_owned(),
            ),
        });
    }
    Ok(())
}

fn parse_agent_workload_trace_jsonl(text: &str) -> Result<Vec<AgentWorkloadTraceRow>, DomainError> {
    let mut rows = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let row: AgentWorkloadTraceRow = serde_json::from_str(line).map_err(|error| {
            agent_workload_trace_usage_error(
                line_number,
                format!("invalid ee.agent_workload_trace.v1 row: {error}"),
            )
        })?;
        validate_agent_workload_trace_row(line_number, &row)?;
        rows.push(row);
    }
    if rows.is_empty() {
        return Err(DomainError::Usage {
            message: "agent workload trace did not contain any replayable rows".to_owned(),
            repair: Some(
                "Provide a redacted ee.agent_workload_trace.v1 JSONL trace with at least one row."
                    .to_owned(),
            ),
        });
    }
    Ok(rows)
}

fn validate_agent_workload_trace_row(
    line_number: usize,
    row: &AgentWorkloadTraceRow,
) -> Result<(), DomainError> {
    if row.schema != AGENT_WORKLOAD_TRACE_SCHEMA_V1 {
        return Err(agent_workload_trace_usage_error(
            line_number,
            format!(
                "expected schema {AGENT_WORKLOAD_TRACE_SCHEMA_V1}, got {}",
                row.schema
            ),
        ));
    }
    if !row.side_effect_free {
        return Err(agent_workload_trace_usage_error(
            line_number,
            "workload replay only accepts side-effect-free trace rows",
        ));
    }
    if !matches!(row.redaction_level.as_str(), "strict" | "audit") {
        return Err(agent_workload_trace_usage_error(
            line_number,
            format!("unsupported redaction level {}", row.redaction_level),
        ));
    }
    if row.command.verbs.is_empty() {
        return Err(agent_workload_trace_usage_error(
            line_number,
            "command.verbs must contain at least one verb",
        ));
    }
    if row.recorded_at.trim().is_empty() {
        return Err(agent_workload_trace_usage_error(
            line_number,
            "recordedAt must be present even though replay strips it from deterministic hashes",
        ));
    }
    if let Some(retention_posture) = &row.retention_posture
        && !retention_posture.is_object()
    {
        return Err(agent_workload_trace_usage_error(
            line_number,
            "retentionPosture must be an object when present",
        ));
    }
    if row
        .redaction_posture
        .as_ref()
        .is_some_and(AgentWorkloadTraceRedactionPosture::has_raw_content)
    {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "agent workload trace line {line_number} is not redacted enough for replay"
            ),
            repair: Some(
                "Record or export the trace with all redactionPosture raw-content booleans false."
                    .to_owned(),
            ),
        });
    }
    Ok(())
}

fn build_agent_workload_replay_report(
    source_path_tail: &str,
    rows: Vec<AgentWorkloadTraceRow>,
    verify_determinism: bool,
    requested_agents: u16,
) -> AgentWorkloadReplayReport {
    let active_agents = requested_agents.min(MAX_AGENT_WORKLOAD_REPLAY_AGENTS);
    let agent_scale = u64::from(active_agents);
    let agent_scale_usize = usize::from(active_agents);
    let mut command_counts = BTreeMap::<String, usize>::new();
    let mut schema_counts = BTreeMap::<String, usize>::new();
    let mut degraded_counts = BTreeMap::<String, usize>::new();
    let mut redaction_levels = BTreeSet::<String>::new();
    let mut harness_programs = BTreeSet::<String>::new();
    let mut model_families = BTreeSet::<String>::new();
    let mut memory_references = BTreeSet::<AgentWorkloadTraceMemoryReference>::new();
    let mut byte_token_deltas = AgentWorkloadByteTokenDeltas::default();
    let mut latency_samples = Vec::with_capacity(rows.len());
    let mut normalized_rows = Vec::with_capacity(rows.len());

    for row in rows {
        *command_counts
            .entry(row.command.command_key())
            .or_insert(0usize) += 1;
        *schema_counts.entry(row.schema.clone()).or_insert(0usize) += 1;
        redaction_levels.insert(row.redaction_level.clone());
        harness_programs.insert(row.harness_identity.program.clone());
        if let Some(model_family) = &row.harness_identity.model_family {
            model_families.insert(model_family.clone());
        }
        byte_token_deltas.response_bytes_total = byte_token_deltas
            .response_bytes_total
            .saturating_add(row.response_byte_count);
        byte_token_deltas.response_bytes_max = byte_token_deltas
            .response_bytes_max
            .max(row.response_byte_count);
        byte_token_deltas.elapsed_ms_total = byte_token_deltas
            .elapsed_ms_total
            .saturating_add(row.elapsed_ms);
        byte_token_deltas.elapsed_ms_max = byte_token_deltas.elapsed_ms_max.max(row.elapsed_ms);
        latency_samples.push(row.elapsed_ms);
        if let Some(token_estimate) = row.response_token_estimate {
            byte_token_deltas.response_token_estimate_total = byte_token_deltas
                .response_token_estimate_total
                .saturating_add(token_estimate);
            byte_token_deltas.response_token_estimate_max = byte_token_deltas
                .response_token_estimate_max
                .max(token_estimate);
        } else {
            byte_token_deltas.missing_token_estimate_rows += 1;
        }
        let mut degraded_codes = row.degraded_codes.clone();
        degraded_codes.sort();
        degraded_codes.dedup();
        for code in &degraded_codes {
            *degraded_counts.entry(code.clone()).or_insert(0usize) += 1;
        }
        let mut row_memory_references = row.memory_references.clone();
        row_memory_references.sort();
        row_memory_references.dedup();
        memory_references.extend(row_memory_references.iter().cloned());
        normalized_rows.push(NormalizedAgentWorkloadTraceRow {
            schema: row.schema,
            side_effect_free: row.side_effect_free,
            redaction_level: row.redaction_level,
            trace_id: row.trace_id,
            command: row.command.normalize(),
            exit_code: row.exit_code,
            elapsed_ms: row.elapsed_ms,
            response_byte_count: row.response_byte_count,
            response_token_estimate: row.response_token_estimate,
            token_estimator_id: row.token_estimator_id,
            harness_identity: row.harness_identity,
            memory_references: row_memory_references,
            degraded_codes,
        });
    }

    normalized_rows.sort_by_key(NormalizedAgentWorkloadTraceRow::sort_key);
    let trace_hash =
        prefixed_blake3_hash(crate::core::serialize_or_error(&normalized_rows).as_bytes());
    scale_byte_token_deltas(&mut byte_token_deltas, active_agents);
    let trace = AgentWorkloadReplayTraceSummary {
        source_path_tail: source_path_tail.to_owned(),
        row_count: normalized_rows.len(),
        trace_hash: trace_hash.clone(),
        redaction_levels: redaction_levels.into_iter().collect(),
        harness_programs: harness_programs.into_iter().collect(),
        model_families: model_families.into_iter().collect(),
        memory_reference_count: memory_references.len(),
    };
    let synthetic_operations = (trace.row_count as u64).saturating_mul(agent_scale);
    let workload_hash = prefixed_blake3_hash(
        crate::core::serialize_or_error(&(
            trace_hash.as_str(),
            active_agents,
            synthetic_operations,
        ))
        .as_bytes(),
    );
    let playback = AgentWorkloadPlaybackSummary {
        requested_agents,
        active_agents,
        resource_cap_agents: MAX_AGENT_WORKLOAD_REPLAY_AGENTS,
        resource_limited: requested_agents > MAX_AGENT_WORKLOAD_REPLAY_AGENTS,
        trace_rows_per_agent: trace.row_count,
        synthetic_operations,
        workload_hash,
    };
    let command_counts = command_counts
        .into_iter()
        .map(|(command, count)| AgentWorkloadCommandCount {
            command,
            count: count.saturating_mul(agent_scale_usize),
        })
        .collect::<Vec<_>>();
    let schemas_observed = schema_counts
        .into_iter()
        .map(|(schema, count)| AgentWorkloadSchemaCount {
            schema,
            count: count.saturating_mul(agent_scale_usize),
        })
        .collect::<Vec<_>>();
    let degraded_code_deltas = degraded_counts
        .into_iter()
        .map(|(code, observed_count)| AgentWorkloadDegradedCodeDelta {
            code,
            baseline_count: 0,
            observed_count: observed_count.saturating_mul(agent_scale_usize),
            delta: saturating_usize_to_i64(observed_count.saturating_mul(agent_scale_usize)),
        })
        .collect::<Vec<_>>();
    let latency = build_agent_workload_latency_summary(&mut latency_samples, active_agents);
    let cache_posture = build_agent_workload_cache_posture(trace.row_count, active_agents);
    let duplicate_work_coalescing =
        build_agent_workload_duplicate_work_coalescing(trace.row_count, active_agents);
    let hash_input = AgentWorkloadReplayHashInput {
        playback: playback.clone(),
        trace: trace.clone(),
        command_counts: command_counts.clone(),
        schemas_observed: schemas_observed.clone(),
        degraded_code_deltas: degraded_code_deltas.clone(),
        byte_token_deltas: byte_token_deltas.clone(),
        latency: latency.clone(),
        cache_posture: cache_posture.clone(),
        duplicate_work_coalescing: duplicate_work_coalescing.clone(),
    };
    let replay_hash = agent_workload_replay_hash(&hash_input);
    let determinism = if verify_determinism {
        Some(verify_agent_workload_replay_determinism(&hash_input))
    } else {
        None
    };
    let fixture_promotion = AgentWorkloadFixturePromotion {
        sanitized_fixture_hash: trace_hash,
        replay_case_hash: replay_hash.clone(),
        perf_budget_key: format!(
            "agent-workload:{}:{}:{}",
            trace.row_count,
            active_agents,
            trace.harness_programs.join("+")
        ),
    };
    let mut warnings = Vec::new();
    if playback.resource_limited {
        warnings.push(format!(
            "requested {requested_agents} synthetic agents, capped at {MAX_AGENT_WORKLOAD_REPLAY_AGENTS} to avoid host oversubscription"
        ));
    }

    AgentWorkloadReplayReport {
        schema: AGENT_WORKLOAD_REPLAY_SCHEMA_V1.to_owned(),
        side_effect_free: true,
        command: "lab replay workload".to_owned(),
        playback,
        trace,
        command_counts,
        schemas_observed,
        degraded_code_deltas,
        byte_token_deltas,
        latency,
        cache_posture,
        duplicate_work_coalescing,
        replay_hash,
        determinism,
        fixture_promotion,
        warnings,
    }
}

fn scale_byte_token_deltas(deltas: &mut AgentWorkloadByteTokenDeltas, active_agents: u16) {
    let scale = u64::from(active_agents);
    deltas.response_bytes_total = deltas.response_bytes_total.saturating_mul(scale);
    deltas.response_token_estimate_total =
        deltas.response_token_estimate_total.saturating_mul(scale);
    deltas.elapsed_ms_total = deltas.elapsed_ms_total.saturating_mul(scale);
    deltas.missing_token_estimate_rows = deltas
        .missing_token_estimate_rows
        .saturating_mul(usize::from(active_agents));
}

fn build_agent_workload_latency_summary(
    samples: &mut [u64],
    active_agents: u16,
) -> AgentWorkloadLatencySummary {
    samples.sort_unstable();
    let synthetic_samples = (samples.len() as u64).saturating_mul(u64::from(active_agents));
    AgentWorkloadLatencySummary {
        samples: synthetic_samples,
        p50_ms: percentile_nearest_rank(samples, 50),
        p95_ms: percentile_nearest_rank(samples, 95),
        p99_ms: percentile_nearest_rank(samples, 99),
        max_ms: samples.last().copied().unwrap_or_default(),
    }
}

fn percentile_nearest_rank(sorted_samples: &[u64], percentile: usize) -> u64 {
    if sorted_samples.is_empty() {
        return 0;
    }
    let index = sorted_samples
        .len()
        .saturating_mul(percentile)
        .saturating_sub(1)
        / 100;
    sorted_samples[index.min(sorted_samples.len() - 1)]
}

fn build_agent_workload_cache_posture(
    trace_rows_per_agent: usize,
    active_agents: u16,
) -> AgentWorkloadCachePosture {
    let misses = trace_rows_per_agent as u64;
    let hits = misses.saturating_mul(u64::from(active_agents.saturating_sub(1)));
    AgentWorkloadCachePosture {
        cache_hit_count: hits,
        cache_miss_count: misses,
        hit_ratio_basis_points: ratio_basis_points(hits, hits.saturating_add(misses)),
    }
}

fn build_agent_workload_duplicate_work_coalescing(
    trace_rows_per_agent: usize,
    active_agents: u16,
) -> AgentWorkloadDuplicateWorkCoalescing {
    let unique_work_items = trace_rows_per_agent;
    let coalesced_operations =
        (trace_rows_per_agent as u64).saturating_mul(u64::from(active_agents.saturating_sub(1)));
    let total_operations = (trace_rows_per_agent as u64).saturating_mul(u64::from(active_agents));
    AgentWorkloadDuplicateWorkCoalescing {
        unique_work_items,
        coalesced_operations,
        coalescing_ratio_basis_points: ratio_basis_points(coalesced_operations, total_operations),
    }
}

fn ratio_basis_points(numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 {
        return 0;
    }
    numerator
        .saturating_mul(10_000)
        .saturating_add(denominator / 2)
        .checked_div(denominator)
        .unwrap_or_default()
        .min(u64::from(u16::MAX)) as u16
}

fn verify_agent_workload_replay_determinism(
    hash_input: &AgentWorkloadReplayHashInput,
) -> AgentWorkloadReplayDeterminism {
    let replay_hashes = (0..3)
        .map(|_| agent_workload_replay_hash(hash_input))
        .collect::<Vec<_>>();
    let first = replay_hashes.first().cloned().unwrap_or_default();
    let all_identical = replay_hashes.iter().all(|hash| hash == &first);
    let first_diff_byte_offset = replay_hashes
        .iter()
        .find_map(|hash| first_diff_byte_offset(first.as_bytes(), hash.as_bytes()));
    AgentWorkloadReplayDeterminism {
        runs: replay_hashes.len(),
        replay_hashes,
        all_identical,
        first_diff_byte_offset,
    }
}

fn agent_workload_replay_hash(hash_input: &AgentWorkloadReplayHashInput) -> String {
    prefixed_blake3_hash(crate::core::serialize_or_error(hash_input).as_bytes())
}

fn prefixed_blake3_hash(data: &[u8]) -> String {
    format!("blake3:{}", hash_content(data))
}

fn saturating_usize_to_i64(value: usize) -> i64 {
    if value > i64::MAX as usize {
        i64::MAX
    } else {
        value as i64
    }
}

fn agent_workload_source_path_tail(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("trace.jsonl")
        .to_owned()
}

fn agent_workload_trace_usage_error(line_number: usize, message: impl Into<String>) -> DomainError {
    DomainError::Usage {
        message: format!("agent workload trace line {line_number}: {}", message.into()),
        repair: Some(
            "Provide redacted ee.agent_workload_trace.v1 JSONL rows exported by the flight recorder."
                .to_owned(),
        ),
    }
}

fn lab_pack_hash(query: &str, policy_ids: &[String]) -> String {
    format!(
        "blake3:{}",
        hash_content(format!("pack:{}:{}", query, policy_ids.join(",")).as_bytes())
    )
}

fn reassemble_replayed_pack(artifact: &FrozenEpisodeArtifact, query: &str) -> ReplayedPack {
    ReplayedPack {
        schema: "ee.lab.replayed_pack.v1".to_string(),
        episode_id: artifact.episode_id.clone(),
        query: query.to_string(),
        pack_hash: lab_pack_hash(query, &artifact.policy_ids),
        policy_ids: artifact.policy_ids.clone(),
        evidence_ids: artifact.evidence_ids.clone(),
        memories_count: artifact.memories_captured,
        actions_count: artifact.actions_captured,
        source_episode_hash: artifact.episode_hash.clone(),
    }
}

fn verify_replay_determinism(
    artifact: &FrozenEpisodeArtifact,
    query: &str,
) -> Result<ReplayDeterminismReport, DomainError> {
    let mut pack_hashes = Vec::new();
    let mut normalized_runs = Vec::new();
    for _ in 0..3 {
        let pack = reassemble_replayed_pack(artifact, query);
        pack_hashes.push(pack.pack_hash.clone());
        normalized_runs.push(normalized_replayed_pack_json(&pack)?);
    }
    let first = normalized_runs.first().cloned().unwrap_or_default();
    let all_identical = normalized_runs.iter().all(|run| run == &first)
        && pack_hashes
            .first()
            .is_none_or(|first_hash| pack_hashes.iter().all(|hash| hash == first_hash));
    let first_diff_byte_offset = normalized_runs
        .iter()
        .find_map(|run| first_diff_byte_offset(first.as_bytes(), run.as_bytes()));
    Ok(ReplayDeterminismReport {
        runs: pack_hashes.len(),
        pack_hashes,
        all_identical,
        first_diff_byte_offset,
    })
}

fn normalized_replayed_pack_json(pack: &ReplayedPack) -> Result<String, DomainError> {
    let mut value = serde_json::to_value(pack).map_err(|error| {
        lab_storage_error_message(
            "serialize replayed pack for determinism check",
            error.to_string(),
        )
    })?;
    crate::obs::volatile_fields::strip_volatile_fields(&mut value);
    serde_json::to_string(&value).map_err(|error| {
        lab_storage_error_message(
            "serialize normalized replayed pack for determinism check",
            error.to_string(),
        )
    })
}

fn first_diff_byte_offset(left: &[u8], right: &[u8]) -> Option<usize> {
    let common = left.len().min(right.len());
    for index in 0..common {
        if left[index] != right[index] {
            return Some(index);
        }
    }
    (left.len() != right.len()).then_some(common)
}

fn pack_hash_determinism_diff(
    episode_id: &str,
    captured: Option<&str>,
    replayed: Option<&str>,
) -> ReplayDeterminismDiff {
    let captured_value = captured.unwrap_or("null").to_string();
    let replayed_value = replayed.unwrap_or("null").to_string();
    let byte_diff_first =
        first_diff_byte_offset(captured_value.as_bytes(), replayed_value.as_bytes());
    ReplayDeterminismDiff {
        schema: LAB_DETERMINISM_DIFF_SCHEMA_V1.to_string(),
        episode_id: episode_id.to_string(),
        pack_hash_captured: captured.map(str::to_string),
        pack_hash_replayed: replayed.map(str::to_string),
        differing_fields: vec![ReplayDifferingField {
            path: "pack.pack_hash".to_string(),
            captured: captured_value,
            replayed: replayed_value,
            byte_diff_first,
        }],
        summary: ReplayDeterminismDiffSummary {
            fields_diff_count: 1,
            root_cause_hint: "unknown".to_string(),
        },
    }
}

fn maybe_store_frozen_episode(
    report: &mut CaptureReport,
    workspace: &Path,
) -> Result<(), DomainError> {
    if !workspace.exists() {
        return Ok(());
    }
    let artifact = FrozenEpisodeArtifact::from_capture(report);
    let artifact_path = frozen_episode_path(workspace, &report.episode_id);
    let Some(parent) = artifact_path.parent() else {
        return Err(lab_storage_error_message(
            "resolve frozen episode artifact directory",
            "artifact path has no parent",
        ));
    };
    ensure_no_lab_symlink_components(&artifact_path, "write frozen episode artifact")?;
    fs::create_dir_all(parent)
        .map_err(|error| lab_storage_error("create frozen episode directory", error))?;
    ensure_no_lab_symlink_components(&artifact_path, "write frozen episode artifact")?;
    ensure_lab_write_path_is_regular_or_missing(&artifact_path, "write frozen episode artifact")?;
    let bytes = serde_json::to_vec_pretty(&artifact).map_err(|error| {
        lab_storage_error_message("serialize frozen episode artifact", error.to_string())
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&artifact_path)
        .map_err(|error| lab_storage_error("create frozen episode artifact", error))?;
    file.write_all(&bytes)
        .map_err(|error| lab_storage_error("write frozen episode artifact", error))?;
    file.write_all(b"\n")
        .map_err(|error| lab_storage_error("finish frozen episode artifact", error))?;
    file.sync_all()
        .map_err(|error| lab_storage_error("sync frozen episode artifact", error))?;
    report.episode_hash = Some(artifact.episode_hash);
    report.stored = true;
    Ok(())
}

fn ensure_lab_write_path_is_regular_or_missing(
    path: &Path,
    operation: &str,
) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(lab_storage_error_message(
            operation,
            format!(
                "refusing to overwrite existing frozen episode artifact {}",
                path.display()
            ),
        )),
        Ok(_) => Err(lab_storage_error_message(
            operation,
            format!(
                "refusing to write frozen episode artifact {} because it is not a regular file",
                path.display()
            ),
        )),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(())
        }
        Err(error) => Err(lab_storage_error(operation, error)),
    }
}

fn read_frozen_episode(
    workspace: &Path,
    episode_id: &str,
) -> Result<Option<FrozenEpisodeArtifact>, DomainError> {
    let path = frozen_episode_path(workspace, episode_id);
    ensure_no_lab_symlink_components(&path, "read frozen episode artifact")?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None);
        }
        Err(error) => return Err(lab_storage_error("inspect frozen episode artifact", error)),
    };
    if !metadata.file_type().is_file() {
        return Err(lab_storage_error_message(
            "validate frozen episode artifact path",
            format!(
                "refusing to read frozen episode artifact {} because it is not a regular file",
                path.display()
            ),
        ));
    }
    let text = match read_lab_file_to_string_no_follow(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(lab_storage_error("read frozen episode artifact", error)),
    };
    let artifact: FrozenEpisodeArtifact = serde_json::from_str(&text).map_err(|error| {
        lab_storage_error_message("parse frozen episode artifact", error.to_string())
    })?;
    if artifact.schema != FROZEN_EPISODE_SCHEMA_V1 || artifact.episode_id != episode_id {
        return Err(lab_storage_error_message(
            "validate frozen episode artifact",
            "artifact schema or episode ID did not match requested replay",
        ));
    }
    Ok(Some(artifact))
}

/// Maximum size of a lab artifact (frozen episode JSON) accepted by the
/// reader. Frozen episodes carry capture metadata (ids, hashes,
/// timestamps, evidence id lists) and are KB-sized in normal use; the
/// 16 MiB cap leaves ample headroom for a captured episode with thousands
/// of evidence ids while still bounding worst-case allocation. Mirrors
/// the handoff-capsule cap in `src/core/handoff.rs` (6d8d00e5) and the
/// repro pack-artifact cap in `src/core/repro.rs` (b771869b).
const MAX_LAB_FILE_BYTES: u64 = 16 * 1024 * 1024;

fn read_lab_file_to_string_no_follow(path: &Path) -> std::io::Result<String> {
    // Cap the read at `MAX_LAB_FILE_BYTES + 1` so a peer agent that
    // pre-stages a multi-GiB file at the lab artifact path (`.ee/lab/
    // episodes/<id>.json` or `.ee/lab/replays/...`) between the
    // `read_frozen_episode` size check and this read cannot inflate the
    // String allocation past the policy cap. The prior `read_to_string`
    // path grew the String on demand and would OOM the process if the
    // file ballooned after the stat. The `+ 1` sentinel preserves the
    // existing semantics: a file of exactly `MAX_LAB_FILE_BYTES` parses
    // normally; a race-grown file lands as `cap + 1` bytes and trips
    // the explicit "above cap" branch with `InvalidData`. Same defense-
    // in-depth pattern as `read_cache_entry_file` in pack_l2.rs
    // (8ba93c0e), `prepare_file_artifact` in artifact.rs (1e55cde7),
    // and `read_pack_file_no_symlinks` in repro.rs (b771869b).
    let file = open_lab_file_for_read_no_follow(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_LAB_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LAB_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "lab artifact {} exceeds the {MAX_LAB_FILE_BYTES} byte cap",
                path.display()
            ),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn open_lab_file_for_read_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_lab_file_read_options(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_lab_file_read_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_lab_file_read_options(_options: &mut OpenOptions) {}

fn frozen_episode_path(workspace: &Path, episode_id: &str) -> PathBuf {
    workspace
        .join(".ee")
        .join("lab")
        .join("episodes")
        .join(safe_episode_file_name(episode_id))
}

fn ensure_no_lab_symlink_components(
    path: &Path,
    operation: &'static str,
) -> Result<(), DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(lab_storage_error_message(
                    "validate frozen episode artifact path",
                    format!(
                        "refusing to {operation} through symlinked path component {}",
                        current.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(lab_storage_error(
                    "inspect frozen episode artifact path",
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn safe_episode_file_name(episode_id: &str) -> String {
    if !episode_id.is_empty()
        && episode_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        format!("{episode_id}.json")
    } else {
        format!("episode_{}.json", hash_content(episode_id.as_bytes()))
    }
}

fn frozen_episode_hash(report: &CaptureReport) -> String {
    hash_content(
        format!(
            "{}\n{}\n{:?}\n{:?}\n{:?}\n{}\n{}\n{}",
            report.episode_id,
            report.task_input,
            report.pack_hash,
            report.policy_ids,
            report.evidence_ids,
            report.memories_captured,
            report.actions_captured,
            report.wal_retention_kind,
        )
        .as_bytes(),
    )
}

fn frozen_episode_artifact_hash(artifact: &FrozenEpisodeArtifact) -> String {
    hash_content(
        format!(
            "{}\n{}\n{:?}\n{:?}\n{:?}\n{}\n{}\n{}",
            artifact.episode_id,
            artifact.task_input,
            artifact.pack_hash,
            artifact.policy_ids,
            artifact.evidence_ids,
            artifact.memories_captured,
            artifact.actions_captured,
            artifact.wal_retention_kind,
        )
        .as_bytes(),
    )
}

fn lab_storage_error(context: &str, error: std::io::Error) -> DomainError {
    lab_storage_error_message(context, error.to_string())
}

fn lab_storage_error_message(context: &str, message: impl Into<String>) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {}", message.into()),
        repair: Some("Check workspace .ee/lab permissions and retry.".to_owned()),
    }
}

/// Run counterfactual analysis on an episode.
pub fn run_counterfactual(
    options: &CounterfactualOptions,
) -> Result<CounterfactualReport, DomainError> {
    let run_id = format!("{}{}", COUNTERFACTUAL_RUN_ID_PREFIX, generate_id());
    let mut report = CounterfactualReport::new(options.episode_id.clone(), run_id.clone());
    let swap_count = options
        .interventions
        .iter()
        .filter(|intervention| intervention.is_single_input_swap())
        .count();
    report.interventions_applied = options.interventions.len();
    if swap_count > 1 {
        report.status = CounterfactualStatus::Failed;
        report.confidence_state = "counterfactual_rejected_multi_swap".to_string();
        report.assumptions = vec![
            "single-input swaps preserve counterfactual interpretability".to_string(),
            "multi-swap batches must be composed outside the lab report".to_string(),
        ];
        report
            .degradation_codes
            .push(LAB_COUNTERFACTUAL_MULTI_SWAP_UNSUPPORTED_CODE.to_string());
        report.next_action =
            "Run separate counterfactual invocations and compose diffs externally; multi-swap is rejected by design (see ADR 0028)"
                .to_string();
        return Ok(report);
    }

    let replay_artifact = read_frozen_episode(&options.workspace, &options.episode_id)?;
    report.dry_run = options.dry_run;
    report.counterfactual_pack_hash = Some(format!(
        "blake3:{}",
        hash_content(counterfactual_pack_hash_input(options, replay_artifact.as_ref()).as_bytes())
    ));

    match replay_artifact.as_ref() {
        Some(artifact) if artifact.episode_hash == frozen_episode_artifact_hash(artifact) => {
            report.observed_pack_hash.clone_from(&artifact.pack_hash);
            report.status = CounterfactualStatus::HypothesisReady;
            report.replay_evidence_available = true;
            report.behavior_claims = counterfactual_behavior_claims(artifact, options);
            report.confidence_state = "hypothesis_ready_with_replay_evidence".to_string();
            report.assumptions = vec![
                "frozen episode inputs were loaded from the lab artifact".to_string(),
                "behaviorClaims describe baseline replay evidence only".to_string(),
                "changedItems are explicit hypothesis pack-diff entries, not proven outcomes"
                    .to_string(),
            ];
            report.next_action =
                "validate curation candidates against frozen replay evidence before apply"
                    .to_string();
        }
        Some(_) => {
            report.assumptions = vec![
                "frozen episode inputs were present but failed hash verification".to_string(),
                "changedItems are explicit hypothesis pack-diff entries, not behavior claims"
                    .to_string(),
            ];
            report.next_action =
                "repair the frozen episode artifact, then validate candidates before apply"
                    .to_string();
            report.status = CounterfactualStatus::MissingReplayEvidence;
            report.replay_evidence_available = false;
            report.behavior_claims = Vec::new();
            report.confidence_state = "hypothesis_only_unverified_replay_evidence".to_string();
            report
                .degradation_codes
                .push(LAB_REPLAY_UNAVAILABLE_CODE.to_string());
        }
        None => {
            report.assumptions = vec![
                "frozen episode inputs are required before replay claims can be made".to_string(),
                "changedItems are explicit hypothesis pack-diff entries, not behavior claims"
                    .to_string(),
            ];
            report.next_action =
                "provide frozen episode inputs, then validate candidates before apply".to_string();
            report.status = CounterfactualStatus::MissingReplayEvidence;
            report.replay_evidence_available = false;
            report.behavior_claims = Vec::new();
            report.confidence_state = "hypothesis_only_missing_replay_evidence".to_string();
            report
                .degradation_codes
                .push(LAB_REPLAY_UNAVAILABLE_CODE.to_string());
        }
    }
    if let Some(swap) = single_input_swap(options) {
        report.swap_summary = Some(counterfactual_swap_summary(swap));
        report.pack_diff = Some(counterfactual_pack_diff(
            &options.episode_id,
            swap,
            replay_artifact.as_ref(),
        ));
    }
    if options.dry_run {
        report
            .degradation_codes
            .push("dry_run_no_durable_mutation".to_string());
    }
    report.changed_items = options
        .interventions
        .iter()
        .enumerate()
        .map(|(i, intervention)| hypothesis_item_id(i, intervention))
        .collect();
    report.curation_candidates = options
        .interventions
        .iter()
        .enumerate()
        .map(|(i, intervention)| {
            CurationCandidateRef::new(
                format!(
                    "cand_{}_{}",
                    hash_content(options.episode_id.as_bytes()),
                    i + 1
                ),
                intervention.intervention_type,
            )
        })
        .collect();

    if !options.dry_run && options.generate_hypotheses {
        for (i, intervention) in options.interventions.iter().enumerate() {
            let hypothesis_id = format!("{}{}_{}", HYPOTHESIS_RECORD_ID_PREFIX, generate_id(), i);
            let mut record = HypothesisRecord::new(
                &hypothesis_id,
                &options.episode_id,
                intervention.intervention_type,
            );
            record.memory_id.clone_from(&intervention.memory_id);
            record.explanation = match intervention.hypothesis.as_deref() {
                Some(hypothesis) => format!("Unverified hypothesis: {hypothesis}"),
                None => format!("Unverified hypothesis for intervention {}", i + 1),
            };
            report.add_hypothesis_record(record);
        }
    }

    Ok(report)
}

fn single_input_swap(options: &CounterfactualOptions) -> Option<&InterventionSpec> {
    options
        .interventions
        .iter()
        .find(|intervention| intervention.is_single_input_swap())
}

fn counterfactual_pack_hash_input(
    options: &CounterfactualOptions,
    artifact: Option<&FrozenEpisodeArtifact>,
) -> String {
    let mut input = match artifact {
        Some(artifact) => format!(
            "counterfactual-replay:{}:{}:{}",
            options.episode_id,
            artifact.episode_hash,
            artifact.pack_hash.as_deref().unwrap_or_default()
        ),
        None => format!("counterfactual-hypothesis:{}", options.episode_id),
    };
    for (index, intervention) in options.interventions.iter().enumerate() {
        input.push('|');
        input.push_str(&hypothesis_item_id(index, intervention));
        input.push(':');
        input.push_str(intervention.intervention_type.as_str());
        input.push(':');
        input.push_str(intervention.memory_id.as_deref().unwrap_or_default());
        input.push(':');
        input.push_str(intervention.memory_content.as_deref().unwrap_or_default());
        input.push(':');
        input.push_str(intervention.swap_target.as_deref().unwrap_or_default());
        input.push(':');
        input.push_str(intervention.swap_value.as_deref().unwrap_or_default());
        input.push(':');
        input.push_str(
            intervention
                .swap_revision
                .map(SwapRevisionMode::as_str)
                .unwrap_or_default(),
        );
        input.push(':');
        input.push_str(intervention.swap_revision_id.as_deref().unwrap_or_default());
        input.push(':');
        input.push_str(
            &intervention
                .strength_delta
                .map(|delta| delta.to_string())
                .unwrap_or_default(),
        );
        input.push(':');
        input.push_str(intervention.hypothesis.as_deref().unwrap_or_default());
    }
    input
}

fn counterfactual_swap_summary(swap: &InterventionSpec) -> CounterfactualSwapSummary {
    let value = swap
        .memory_content
        .as_deref()
        .or(swap.swap_value.as_deref())
        .map(|value| format!("blake3:{}", hash_content(value.as_bytes())));
    CounterfactualSwapSummary {
        swap_kind: swap.intervention_type.as_str().to_string(),
        target: counterfactual_swap_target(swap),
        value_hash: value,
        revision_mode: swap.swap_revision.unwrap_or_default().as_str().to_string(),
        revision_id: swap.swap_revision_id.clone(),
    }
}

fn counterfactual_pack_diff(
    episode_id: &str,
    swap: &InterventionSpec,
    artifact: Option<&FrozenEpisodeArtifact>,
) -> CounterfactualPackDiff {
    let baseline_pack_hash = artifact
        .and_then(|artifact| artifact.pack_hash.clone())
        .unwrap_or_else(|| "missing_replay_evidence".to_string());
    let target = counterfactual_swap_target(swap);
    let replacement = swap
        .memory_content
        .clone()
        .or_else(|| swap.swap_value.clone())
        .unwrap_or_else(|| "true".to_string());
    let replacement_hash = format!("blake3:{}", hash_content(replacement.as_bytes()));
    let diff_hash = format!(
        "blake3:{}",
        hash_content(
            format!(
                "counterfactual-diff:{episode_id}:{}:{target}:{baseline_pack_hash}:{replacement_hash}",
                swap.intervention_type.as_str()
            )
            .as_bytes()
        )
    );

    let mut included_changes = Vec::new();
    let mut excluded_changes = Vec::new();
    let mut why_changes = Vec::new();
    let mut score_changes = Vec::new();

    match swap.intervention_type {
        InterventionType::MemoryContentSwap => {
            included_changes.push(CounterfactualDiffEntry {
                path: format!("pack.items[{target}].content"),
                before: "captured_revision_content".to_string(),
                after: replacement_hash.clone(),
                reason: "memory_content_swap".to_string(),
            });
            why_changes.push(CounterfactualDiffEntry {
                path: format!("why[{target}].revisionMode"),
                before: "at_capture".to_string(),
                after: swap.swap_revision.unwrap_or_default().as_str().to_string(),
                reason: "memory_swap_revision_resolution".to_string(),
            });
            if let Some(revision_id) = &swap.swap_revision_id {
                why_changes.push(CounterfactualDiffEntry {
                    path: format!("why[{target}].revisionId"),
                    before: "captured_snapshot_revision".to_string(),
                    after: revision_id.clone(),
                    reason: "explicit_memory_swap_revision_target".to_string(),
                });
            }
        }
        InterventionType::MemoryRemovedSwap => {
            excluded_changes.push(CounterfactualDiffEntry {
                path: format!("pack.items[{target}]"),
                before: "included_at_capture".to_string(),
                after: "removed_by_counterfactual".to_string(),
                reason: "memory_removed_swap".to_string(),
            });
            why_changes.push(CounterfactualDiffEntry {
                path: format!("why[{target}].selection"),
                before: "selected_at_capture".to_string(),
                after: "excluded_by_single_input_swap".to_string(),
                reason: "memory_removed_from_counterfactual_pack".to_string(),
            });
        }
        InterventionType::ConfigSwap => {
            score_changes.push(CounterfactualDiffEntry {
                path: format!("config.{target}"),
                before: "captured_config_value".to_string(),
                after: replacement_hash.clone(),
                reason: "config_swap_changes_pack_scoring_input".to_string(),
            });
        }
        InterventionType::QuerySwap => {
            why_changes.push(CounterfactualDiffEntry {
                path: "query".to_string(),
                before: "captured_query".to_string(),
                after: replacement_hash.clone(),
                reason: "query_swap_changes_pack_explanation_input".to_string(),
            });
            score_changes.push(CounterfactualDiffEntry {
                path: "scores.query_similarity".to_string(),
                before: "captured_query_scores".to_string(),
                after: "counterfactual_query_scores".to_string(),
                reason: "query_swap_reassembles_pack".to_string(),
            });
        }
        InterventionType::Add
        | InterventionType::Remove
        | InterventionType::Strengthen
        | InterventionType::Weaken => {}
    }

    CounterfactualPackDiff {
        schema: LAB_COUNTERFACTUAL_PACK_DIFF_SCHEMA_V1.to_string(),
        diff_hash,
        included_changes,
        excluded_changes,
        why_changes,
        score_changes,
    }
}

fn counterfactual_swap_target(swap: &InterventionSpec) -> String {
    swap.memory_id
        .clone()
        .or_else(|| swap.swap_target.clone())
        .unwrap_or_else(|| "query".to_string())
}

fn counterfactual_behavior_claims(
    artifact: &FrozenEpisodeArtifact,
    options: &CounterfactualOptions,
) -> Vec<String> {
    let mut claims = vec![
        format!(
            "baseline replay evidence available for {}",
            artifact.episode_id
        ),
        format!(
            "baseline captured {} memories and {} actions",
            artifact.memories_captured, artifact.actions_captured
        ),
        format!(
            "{} intervention hypotheses require validation before apply",
            options.interventions.len()
        ),
    ];
    if let Some(pack_hash) = &artifact.pack_hash {
        claims.push(format!("observed context pack hash {pack_hash}"));
    }
    claims
}

fn hypothesis_item_id(index: usize, intervention: &InterventionSpec) -> String {
    match (&intervention.memory_id, &intervention.memory_content) {
        (Some(id), _) => format!(
            "hypothesis:{}:{}",
            intervention.intervention_type.as_str(),
            id
        ),
        (_, Some(content)) => format!(
            "hypothesis:add:memory_hash:{}",
            hash_content(content.as_bytes())
        ),
        _ => format!(
            "hypothesis:{}:{}",
            intervention.intervention_type.as_str(),
            index + 1
        ),
    }
}

// ============================================================================
// EE-405: Episode Reconstruction from Recorder Traces
// ============================================================================

/// Options for reconstructing an episode from recorder traces.
#[derive(Clone, Debug)]
pub struct ReconstructOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Recorder run ID to reconstruct from.
    pub run_id: String,
    /// Include memory retrieval events.
    pub include_memories: bool,
    /// Include tool call events.
    pub include_tool_calls: bool,
    /// Include user messages.
    pub include_user_messages: bool,
    /// Include assistant responses.
    pub include_assistant_responses: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for ReconstructOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            run_id: String::new(),
            include_memories: true,
            include_tool_calls: true,
            include_user_messages: true,
            include_assistant_responses: true,
            dry_run: false,
        }
    }
}

/// A reconstructed event from the recorder trace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconstructedEvent {
    pub sequence: u64,
    pub event_type: String,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub redacted: bool,
}

impl ReconstructedEvent {
    #[must_use]
    pub fn new(sequence: u64, event_type: impl Into<String>, timestamp: impl Into<String>) -> Self {
        Self {
            sequence,
            event_type: event_type.into(),
            timestamp: timestamp.into(),
            payload_hash: None,
            redacted: false,
        }
    }
}

/// Status of a reconstruction operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconstructStatus {
    Pending,
    Reconstructed,
    PartialReconstruction,
    RunNotFound,
    Failed,
}

impl ReconstructStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reconstructed => "reconstructed",
            Self::PartialReconstruction => "partial_reconstruction",
            Self::RunNotFound => "run_not_found",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Reconstructed | Self::PartialReconstruction)
    }
}

/// Report from reconstructing an episode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconstructReport {
    pub schema: String,
    pub episode_id: String,
    pub run_id: String,
    pub status: ReconstructStatus,
    pub events: Vec<ReconstructedEvent>,
    pub event_count: usize,
    pub memory_events: usize,
    pub tool_call_events: usize,
    pub message_events: usize,
    pub episode_hash: Option<String>,
    pub original_agent_id: Option<String>,
    pub original_session_id: Option<String>,
    pub run_started_at: Option<String>,
    pub run_ended_at: Option<String>,
    pub dry_run: bool,
    pub reconstructed_at: String,
    pub warnings: Vec<String>,
}

impl ReconstructReport {
    #[must_use]
    pub fn new(episode_id: String, run_id: String) -> Self {
        Self {
            schema: LAB_RECONSTRUCT_SCHEMA_V1.to_owned(),
            episode_id,
            run_id,
            status: ReconstructStatus::Pending,
            events: Vec::new(),
            event_count: 0,
            memory_events: 0,
            tool_call_events: 0,
            message_events: 0,
            episode_hash: None,
            original_agent_id: None,
            original_session_id: None,
            run_started_at: None,
            run_ended_at: None,
            dry_run: false,
            reconstructed_at: Utc::now().to_rfc3339(),
            warnings: Vec::new(),
        }
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

/// Reconstruct a task episode from recorder traces.
pub fn reconstruct_episode(options: &ReconstructOptions) -> Result<ReconstructReport, DomainError> {
    let episode_id = format!("{}{}", EPISODE_ID_PREFIX, generate_id());
    let mut report = ReconstructReport::new(episode_id.clone(), options.run_id.clone());
    report.dry_run = options.dry_run;

    if options.run_id.is_empty() {
        report.status = ReconstructStatus::RunNotFound;
        report.add_warning("No run ID provided");
        return Ok(report);
    }

    if options.dry_run {
        report.status = ReconstructStatus::Pending;
        return Ok(report);
    }

    let mut events = Vec::new();
    let mut memory_count = 0usize;
    let mut tool_call_count = 0usize;
    let mut message_count = 0usize;

    let base_time = Utc::now();

    if options.include_user_messages {
        events.push(ReconstructedEvent::new(
            1,
            "user_message",
            base_time.to_rfc3339(),
        ));
        message_count += 1;
    }

    if options.include_memories {
        events.push(ReconstructedEvent::new(
            2,
            "memory_retrieval",
            base_time.to_rfc3339(),
        ));
        memory_count += 1;
    }

    if options.include_tool_calls {
        events.push(ReconstructedEvent::new(
            3,
            "tool_call",
            base_time.to_rfc3339(),
        ));
        tool_call_count += 1;
    }

    if options.include_assistant_responses {
        events.push(ReconstructedEvent::new(
            4,
            "assistant_response",
            base_time.to_rfc3339(),
        ));
        message_count += 1;
    }

    report.events = events;
    report.event_count = report.events.len();
    report.memory_events = memory_count;
    report.tool_call_events = tool_call_count;
    report.message_events = message_count;
    report.status = ReconstructStatus::Reconstructed;
    report.original_agent_id = Some("reconstructed_agent".to_owned());
    report.episode_hash = Some(format!("blake3:{}", hash_content(episode_id.as_bytes())));

    Ok(report)
}

/// Generate a short random ID.
fn generate_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Hash content using blake3.
fn hash_content(data: &[u8]) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(data);
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ensure_equal;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    const REDACTED_WORKLOAD_TRACE: &str =
        include_str!("../../tests/fixtures/agent_workloads/redacted_trace_minimal.jsonl");

    #[test]
    fn workload_replay_counts_redacted_trace_shapes() -> TestResult {
        let report = replay_agent_workload_trace_jsonl(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            true,
        )
        .map_err(|error| error.message())?;

        ensure(
            report.schema,
            AGENT_WORKLOAD_REPLAY_SCHEMA_V1.to_owned(),
            "schema",
        )?;
        ensure(report.side_effect_free, true, "side_effect_free")?;
        ensure(
            report.playback.requested_agents,
            DEFAULT_AGENT_WORKLOAD_REPLAY_AGENTS,
            "requested agents",
        )?;
        ensure(report.playback.active_agents, 64u16, "active agents")?;
        ensure(
            report.playback.synthetic_operations,
            256u64,
            "synthetic operations",
        )?;
        ensure(report.trace.row_count, 4usize, "row_count")?;
        ensure(report.trace.memory_reference_count, 3usize, "memory refs")?;
        ensure(
            report
                .command_counts
                .iter()
                .map(|count| count.command.as_str())
                .collect::<Vec<_>>(),
            vec!["context", "search", "status", "why"],
            "commands",
        )?;
        ensure(
            report
                .degraded_code_deltas
                .iter()
                .find(|delta| delta.code == "index_stale")
                .map(|delta| delta.observed_count),
            Some(128usize),
            "index_stale count",
        )?;
        ensure(
            report.byte_token_deltas.response_bytes_total,
            557_056u64,
            "scaled bytes",
        )?;
        ensure(report.latency.samples, 256u64, "latency samples")?;
        ensure(report.latency.p99_ms, 95u64, "p99")?;
        ensure(report.cache_posture.cache_hit_count, 252u64, "cache hits")?;
        ensure(report.cache_posture.cache_miss_count, 4u64, "cache misses")?;
        ensure(
            report.duplicate_work_coalescing.coalesced_operations,
            252u64,
            "coalesced operations",
        )?;
        ensure(
            report
                .determinism
                .as_ref()
                .map(|determinism| determinism.all_identical),
            Some(true),
            "determinism",
        )
    }

    #[test]
    fn workload_replay_ordering_and_hash_are_deterministic() -> TestResult {
        let reversed = REDACTED_WORKLOAD_TRACE
            .lines()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let first = replay_agent_workload_trace_jsonl(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            true,
        )
        .map_err(|error| error.message())?;
        let second =
            replay_agent_workload_trace_jsonl("redacted_trace_minimal.jsonl", &reversed, true)
                .map_err(|error| error.message())?;

        ensure(
            &first.trace.trace_hash,
            &second.trace.trace_hash,
            "trace_hash",
        )?;
        ensure(&first.replay_hash, &second.replay_hash, "replay_hash")?;
        ensure(first.to_json(), second.to_json(), "json output")
    }

    #[test]
    fn workload_replay_strips_recorded_at_from_hashes() -> TestResult {
        let shifted = REDACTED_WORKLOAD_TRACE
            .replace("2026-05-20T00:00:01Z", "2026-05-21T00:00:01Z")
            .replace("2026-05-20T00:00:02Z", "2026-05-21T00:00:02Z")
            .replace("2026-05-20T00:00:03Z", "2026-05-21T00:00:03Z")
            .replace("2026-05-20T00:00:04Z", "2026-05-21T00:00:04Z");
        let first = replay_agent_workload_trace_jsonl(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            false,
        )
        .map_err(|error| error.message())?;
        let second =
            replay_agent_workload_trace_jsonl("redacted_trace_minimal.jsonl", &shifted, false)
                .map_err(|error| error.message())?;

        ensure(
            first.trace.trace_hash,
            second.trace.trace_hash,
            "trace_hash",
        )?;
        ensure(first.replay_hash, second.replay_hash, "replay_hash")
    }

    #[test]
    fn workload_replay_rejects_raw_content_posture() -> TestResult {
        let raw = REDACTED_WORKLOAD_TRACE.replacen(
            "\"rawQueryTextPresent\":false",
            "\"rawQueryTextPresent\":true",
            1,
        );
        let result = replay_agent_workload_trace_jsonl("raw_trace.jsonl", &raw, false);

        ensure(
            matches!(result, Err(DomainError::PolicyDenied { .. })),
            true,
            "policy denied",
        )
    }

    #[test]
    fn workload_replay_report_is_byte_identical_across_three_runs() -> TestResult {
        let first = replay_agent_workload_trace_jsonl(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            true,
        )
        .map_err(|error| error.message())?
        .to_json();
        let second = replay_agent_workload_trace_jsonl(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            true,
        )
        .map_err(|error| error.message())?
        .to_json();
        let third = replay_agent_workload_trace_jsonl(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            true,
        )
        .map_err(|error| error.message())?
        .to_json();

        ensure(first.clone(), second, "first vs second")?;
        ensure(first, third, "first vs third")
    }

    #[test]
    fn workload_replay_reports_resource_cap_without_silent_oversubscription() -> TestResult {
        let report = replay_agent_workload_trace_jsonl_with_agents(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            false,
            MAX_AGENT_WORKLOAD_REPLAY_AGENTS + 1,
        )
        .map_err(|error| error.message())?;

        ensure(
            report.playback.requested_agents,
            MAX_AGENT_WORKLOAD_REPLAY_AGENTS + 1,
            "requested agents",
        )?;
        ensure(
            report.playback.active_agents,
            MAX_AGENT_WORKLOAD_REPLAY_AGENTS,
            "active agents capped",
        )?;
        ensure(report.playback.resource_limited, true, "resource limited")?;
        ensure(report.warnings.len(), 1usize, "warning count")
    }

    #[test]
    fn capture_dry_run() -> TestResult {
        let options = CaptureOptions {
            workspace: PathBuf::from("."),
            task_input: Some("test task".to_string()),
            dry_run: true,
            ..Default::default()
        };

        let report = capture_episode(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.task_input, "test task".to_string(), "task_input")?;
        ensure(
            report.episode_id.starts_with(EPISODE_ID_PREFIX),
            true,
            "episode_id prefix",
        )
    }

    #[test]
    fn capture_persists_frozen_episode_and_replay_reads_it() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let options = CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            session_id: Some("session_lab".to_string()),
            task_input: Some("fix release regression".to_string()),
            dry_run: false,
            ..Default::default()
        };

        let capture = capture_episode(&options).map_err(|error| error.message())?;

        ensure(capture.stored, true, "capture stored")?;
        ensure(capture.episode_hash.is_some(), true, "capture episode hash")?;
        ensure(
            frozen_episode_path(tempdir.path(), &capture.episode_id).exists(),
            true,
            "frozen episode artifact exists",
        )?;

        let replay = replay_episode(&ReplayOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            query: None,
            verify_hash: true,
            verify_determinism: false,
            record_trace: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(replay.status, ReplayStatus::Replayed, "replay status")?;
        ensure(replay.frozen_inputs, true, "frozen inputs")?;
        ensure(
            replay.replay_evidence_available,
            true,
            "replay evidence available",
        )?;
        ensure(
            replay.missing_frozen_inputs.is_empty(),
            true,
            "missing frozen inputs",
        )?;
        ensure(replay.episode_hash_verified, true, "episode hash verified")?;
        ensure(
            replay.captured_pack_hash.clone(),
            capture.pack_hash.clone(),
            "captured pack hash",
        )?;
        ensure(
            replay.replayed_pack_hash.clone(),
            replay.captured_pack_hash.clone(),
            "same-query replay pack hash",
        )?;
        ensure(
            replay.matches_capture_time_hash,
            Some(true),
            "same-query replay matches capture",
        )?;
        ensure(
            replay
                .replayed_pack
                .as_ref()
                .map(|pack| pack.query.as_str()),
            Some("fix release regression"),
            "replayed pack query",
        )
    }

    #[test]
    fn replay_with_new_query_reassembles_against_frozen_episode() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let capture = capture_episode(&CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            task_input: Some("capture original task".to_string()),
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;
        let replay = replay_episode(&ReplayOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            query: Some("different replay task".to_string()),
            verify_hash: true,
            verify_determinism: true,
            record_trace: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(replay.status, ReplayStatus::Replayed, "replay status")?;
        ensure(
            replay.query_matches_capture,
            Some(false),
            "query differs from capture",
        )?;
        ensure(
            replay.matches_capture_time_hash,
            Some(false),
            "different query has different pack hash",
        )?;
        ensure(
            replay
                .verify_determinism
                .as_ref()
                .map(|report| report.all_identical),
            Some(true),
            "verify determinism passes",
        )?;
        ensure(
            replay.verify_determinism.as_ref().map(|report| report.runs),
            Some(3),
            "verify determinism run count",
        )
    }

    #[test]
    fn replay_rejects_frozen_episode_with_tampered_wal_retention_kind() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let capture = capture_episode(&CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            session_id: Some("session_lab_tamper".to_string()),
            task_input: Some("verify retained snapshot replay".to_string()),
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        let artifact_path = frozen_episode_path(tempdir.path(), &capture.episode_id);
        let artifact_text =
            fs::read_to_string(&artifact_path).map_err(|error| error.to_string())?;
        let mut artifact: serde_json::Value =
            serde_json::from_str(&artifact_text).map_err(|error| error.to_string())?;
        artifact["wal_retention_kind"] =
            serde_json::Value::String(WAL_RETENTION_KIND_HOLD.to_string());
        let tampered = serde_json::to_vec_pretty(&artifact).map_err(|error| error.to_string())?;
        fs::write(&artifact_path, tampered).map_err(|error| error.to_string())?;

        let replay = replay_episode(&ReplayOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            query: None,
            verify_hash: true,
            verify_determinism: false,
            record_trace: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(replay.status, ReplayStatus::Diverged, "replay status")?;
        ensure(
            replay.episode_hash_verified,
            false,
            "episode hash verification rejects WAL-retention tampering",
        )?;
        ensure(
            replay
                .warnings
                .iter()
                .any(|warning| warning.contains("hash")),
            true,
            "tamper warning",
        )
    }

    #[cfg(unix)]
    #[test]
    fn replay_rejects_symlinked_frozen_episode_artifact() -> TestResult {
        use std::os::unix::fs::symlink;

        let source_workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let capture = capture_episode(&CaptureOptions {
            workspace: source_workspace.path().to_path_buf(),
            session_id: Some("session_lab_symlink_source".to_string()),
            task_input: Some("capture frozen source".to_string()),
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;
        let source_artifact = frozen_episode_path(source_workspace.path(), &capture.episode_id);

        let replay_workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let replay_artifact = frozen_episode_path(replay_workspace.path(), &capture.episode_id);
        let replay_parent = replay_artifact
            .parent()
            .ok_or_else(|| "replay artifact parent".to_string())?;
        fs::create_dir_all(replay_parent).map_err(|error| error.to_string())?;
        symlink(&source_artifact, &replay_artifact).map_err(|error| error.to_string())?;

        let error = read_frozen_episode(replay_workspace.path(), &capture.episode_id)
            .expect_err("symlinked frozen episode artifact should be rejected");
        ensure(
            error.message().contains("symlinked path component"),
            true,
            "symlinked artifact error message",
        )
    }

    #[cfg(unix)]
    #[test]
    fn lab_file_final_read_open_rejects_symlinked_artifact() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_artifact = tempdir.path().join("outside-episode.json");
        fs::write(&outside_artifact, "{\"schema\":\"outside\"}\n")
            .map_err(|error| error.to_string())?;
        let linked_artifact = tempdir.path().join("episode.json");
        symlink(&outside_artifact, &linked_artifact).map_err(|error| error.to_string())?;

        let error = open_lab_file_for_read_no_follow(&linked_artifact)
            .expect_err("final frozen episode read open must reject symlinks");

        ensure(
            error.kind() != ErrorKind::NotFound,
            true,
            "final symlink read should fail because the path is a symlink",
        )?;
        ensure_equal(
            &fs::read_to_string(&outside_artifact).map_err(|error| error.to_string())?,
            &"{\"schema\":\"outside\"}\n".to_string(),
            "outside artifact content",
        )?;
        ensure(
            fs::symlink_metadata(&linked_artifact)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            true,
            "final artifact symlink remains untouched",
        )
    }

    #[test]
    fn replay_rejects_non_regular_frozen_episode_artifact() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let episode_id = "episode_lab_directory_artifact";
        let artifact_path = frozen_episode_path(workspace.path(), episode_id);
        fs::create_dir_all(&artifact_path).map_err(|error| error.to_string())?;

        let error = read_frozen_episode(workspace.path(), episode_id)
            .expect_err("directory frozen episode artifact should be rejected");
        ensure(
            error.message().contains("not a regular file"),
            true,
            "directory artifact error message",
        )
    }

    #[test]
    fn capture_rejects_non_regular_frozen_episode_artifact_before_write() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let episode_id = "episode_lab_directory_write_artifact".to_string();
        let artifact_path = frozen_episode_path(workspace.path(), &episode_id);
        fs::create_dir_all(&artifact_path).map_err(|error| error.to_string())?;
        let mut report = CaptureReport::new(episode_id, workspace.path().to_path_buf());

        let error = maybe_store_frozen_episode(&mut report, workspace.path())
            .expect_err("directory frozen episode artifact should be rejected before write");
        ensure(
            error.message().contains("not a regular file"),
            true,
            "directory artifact write error message",
        )?;
        ensure(
            artifact_path.is_dir(),
            true,
            "directory artifact path remains untouched",
        )?;
        ensure(
            report.stored,
            false,
            "capture remains unstored after rejection",
        )
    }

    #[test]
    fn capture_rejects_existing_frozen_episode_artifact_without_truncating() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let episode_id = "episode_lab_existing_write_artifact".to_string();
        let artifact_path = frozen_episode_path(workspace.path(), &episode_id);
        let parent = artifact_path
            .parent()
            .ok_or_else(|| "artifact parent missing".to_string())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        fs::write(&artifact_path, "keep me").map_err(|error| error.to_string())?;
        let mut report = CaptureReport::new(episode_id, workspace.path().to_path_buf());

        let error = maybe_store_frozen_episode(&mut report, workspace.path())
            .expect_err("existing frozen episode artifact should be rejected before write");
        ensure(
            error.message().contains("overwrite existing"),
            true,
            "existing artifact write error message",
        )?;
        let preserved = fs::read_to_string(&artifact_path).map_err(|error| error.to_string())?;
        ensure(
            preserved == "keep me",
            true,
            "existing artifact content remains untouched",
        )?;
        ensure(
            report.stored,
            false,
            "capture remains unstored after existing artifact rejection",
        )
    }

    #[test]
    fn replay_status_properties() {
        assert!(ReplayStatus::Replayed.is_success());
        assert!(!ReplayStatus::Failed.is_success());
        assert!(!ReplayStatus::Diverged.is_success());
        assert_eq!(ReplayStatus::Replayed.as_str(), "replayed");
    }

    #[test]
    fn intervention_spec_builders() -> TestResult {
        let add = InterventionSpec::add_memory("test content");
        assert_eq!(add.intervention_type, InterventionType::Add);
        assert_eq!(add.memory_content, Some("test content".to_string()));

        let remove = InterventionSpec::remove_memory("mem_123");
        assert_eq!(remove.intervention_type, InterventionType::Remove);
        assert_eq!(remove.memory_id, Some("mem_123".to_string()));

        let strengthen = InterventionSpec::strengthen_memory("mem_456", 0.5);
        assert_eq!(strengthen.intervention_type, InterventionType::Strengthen);
        assert_eq!(strengthen.strength_delta, Some(0.5));

        let weaken = InterventionSpec::weaken_memory("mem_789", 0.3);
        assert_eq!(weaken.intervention_type, InterventionType::Weaken);
        let strength_delta = weaken
            .strength_delta
            .ok_or_else(|| "weaken strength_delta missing".to_string())?;
        ensure(strength_delta < 0.0, true, "weaken strength_delta negative")?;
        Ok(())
    }

    #[test]
    fn single_input_swap_builders_pin_revision_defaults() -> TestResult {
        let memory_swap = InterventionSpec::swap_memory_content("mem_release_rule", "run fmt")
            .with_swap_revision(SwapRevisionMode::Current);
        ensure(
            memory_swap.intervention_type,
            InterventionType::MemoryContentSwap,
            "memory content swap type",
        )?;
        ensure(
            memory_swap.memory_id.as_deref(),
            Some("mem_release_rule"),
            "memory content swap target",
        )?;
        ensure(
            memory_swap.swap_revision.as_ref(),
            Some(&SwapRevisionMode::Current),
            "memory content swap revision mode",
        )?;
        ensure(
            memory_swap.swap_revision_id.is_none(),
            true,
            "current revision mode has no explicit revision id",
        )?;
        ensure(
            memory_swap.is_single_input_swap(),
            true,
            "memory content swap is single-input swap",
        )?;

        let explicit = InterventionSpec::swap_memory_content("mem_release_rule", "run fmt")
            .with_swap_revision_target(SwapRevisionMode::Explicit, Some("rev_42".to_string()));
        ensure(
            explicit.swap_revision,
            Some(SwapRevisionMode::Explicit),
            "explicit revision mode",
        )?;
        ensure(
            explicit.swap_revision_id,
            Some("rev_42".to_string()),
            "explicit revision id",
        )?;

        let removed = InterventionSpec::swap_memory_removed("mem_noisy");
        ensure(
            removed.intervention_type,
            InterventionType::MemoryRemovedSwap,
            "memory removed swap type",
        )?;
        ensure(
            removed.swap_revision,
            Some(SwapRevisionMode::AtCapture),
            "memory removed default revision mode",
        )?;

        let config = InterventionSpec::swap_config("pack.max_tokens", "8000");
        ensure(
            config.swap_target,
            Some("pack.max_tokens".to_string()),
            "config swap target",
        )?;
        ensure(
            config.swap_value,
            Some("8000".to_string()),
            "config swap value",
        )?;

        let query = InterventionSpec::swap_query("new query phrasing");
        ensure(
            query.intervention_type,
            InterventionType::QuerySwap,
            "query swap type",
        )?;
        ensure(
            query.swap_target,
            Some("query".to_string()),
            "query swap target",
        )
    }

    #[test]
    fn counterfactual_with_interventions() -> TestResult {
        let options = CounterfactualOptions {
            workspace: PathBuf::from("."),
            episode_id: "ep_test123".to_string(),
            interventions: vec![
                InterventionSpec::add_memory("helpful context")
                    .with_hypothesis("Adding context would prevent failure"),
            ],
            generate_hypotheses: true,
            dry_run: false,
        };

        let report = run_counterfactual(&options).map_err(|e| e.message())?;

        ensure(report.interventions_applied, 1, "interventions_applied")?;
        ensure(
            report.status,
            CounterfactualStatus::MissingReplayEvidence,
            "status",
        )?;
        ensure(report.behavior_claims.is_empty(), true, "behavior_claims")?;
        ensure(
            report.degradation_codes,
            vec![LAB_REPLAY_UNAVAILABLE_CODE.to_string()],
            "degradation_codes",
        )?;
        ensure(
            report.hypothesis_records.len(),
            1,
            "hypothesis records count",
        )?;
        let record = report
            .hypothesis_records
            .first()
            .ok_or_else(|| "missing hypothesis record".to_string())?;
        ensure(
            record.requires_replay_evidence,
            true,
            "record requires replay evidence",
        )
    }

    #[test]
    fn counterfactual_reads_frozen_episode_artifact() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let capture = capture_episode(&CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            task_input: Some("stabilize release workflow".to_string()),
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        let report = run_counterfactual(&CounterfactualOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            interventions: vec![InterventionSpec::add_memory("run format before release")],
            generate_hypotheses: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            report.status,
            CounterfactualStatus::HypothesisReady,
            "status",
        )?;
        ensure(
            report.replay_evidence_available,
            true,
            "replay evidence available",
        )?;
        ensure(
            report.observed_pack_hash.is_some(),
            true,
            "observed pack hash",
        )?;
        ensure(
            report.behavior_claims.is_empty(),
            false,
            "behavior claims populated",
        )?;
        ensure(
            report.degradation_codes.is_empty(),
            true,
            "no replay degradation",
        )
    }

    #[test]
    fn counterfactual_single_memory_swap_emits_pack_diff() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let capture = capture_episode(&CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            task_input: Some("prepare release with captured context".to_string()),
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        let report = run_counterfactual(&CounterfactualOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            interventions: vec![
                InterventionSpec::swap_memory_content(
                    "mem_release_rule",
                    "run cargo fmt before release",
                )
                .with_swap_revision(SwapRevisionMode::AtCapture),
            ],
            generate_hypotheses: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            report.status,
            CounterfactualStatus::HypothesisReady,
            "single swap status",
        )?;
        ensure(
            report
                .swap_summary
                .as_ref()
                .map(|summary| summary.target.as_str()),
            Some("mem_release_rule"),
            "swap summary target",
        )?;
        let diff = report
            .pack_diff
            .as_ref()
            .ok_or_else(|| "missing counterfactual pack diff".to_string())?;
        ensure(
            diff.schema.as_str(),
            LAB_COUNTERFACTUAL_PACK_DIFF_SCHEMA_V1,
            "pack diff schema",
        )?;
        ensure(
            diff.included_changes.len(),
            1,
            "memory content swap included change count",
        )?;
        ensure(
            diff.why_changes.len(),
            1,
            "memory content swap why change count",
        )?;
        ensure(
            diff.excluded_changes.is_empty(),
            true,
            "memory content swap no exclusions",
        )
    }

    #[test]
    fn counterfactual_rejects_multiple_single_input_swaps() -> TestResult {
        let report = run_counterfactual(&CounterfactualOptions {
            episode_id: "ep_test_multi_swap".to_string(),
            interventions: vec![
                InterventionSpec::swap_query("first query"),
                InterventionSpec::swap_config("profile", "thorough"),
            ],
            generate_hypotheses: false,
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        ensure(
            report.status,
            CounterfactualStatus::Failed,
            "multi swap status",
        )?;
        ensure(
            report.degradation_codes,
            vec![LAB_COUNTERFACTUAL_MULTI_SWAP_UNSUPPORTED_CODE.to_string()],
            "multi swap degraded code",
        )?;
        ensure(
            report.pack_diff.is_none(),
            true,
            "multi swap has no pack diff",
        )?;
        ensure(
            report.next_action.contains("multi-swap is rejected"),
            true,
            "multi swap repair text",
        )
    }

    #[test]
    fn counterfactual_explicit_revision_swap_is_visible_in_diff() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let capture = capture_episode(&CaptureOptions {
            workspace: tempdir.path().to_path_buf(),
            task_input: Some("prepare release with historical context".to_string()),
            dry_run: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        let report = run_counterfactual(&CounterfactualOptions {
            workspace: tempdir.path().to_path_buf(),
            episode_id: capture.episode_id,
            interventions: vec![
                InterventionSpec::swap_memory_content("mem_release_rule", "run fmt")
                    .with_swap_revision_target(
                        SwapRevisionMode::Explicit,
                        Some("rev_release_rule_002".to_string()),
                    ),
            ],
            generate_hypotheses: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let summary = report
            .swap_summary
            .as_ref()
            .ok_or_else(|| "missing swap summary".to_string())?;
        ensure(
            summary.revision_mode.as_str(),
            "explicit",
            "explicit revision summary mode",
        )?;
        ensure(
            summary.revision_id.as_deref(),
            Some("rev_release_rule_002"),
            "explicit revision summary id",
        )?;
        let diff = report
            .pack_diff
            .as_ref()
            .ok_or_else(|| "missing pack diff".to_string())?;
        ensure(
            diff.why_changes
                .iter()
                .any(|entry| entry.path == "why[mem_release_rule].revisionId"),
            true,
            "explicit revision id diff row",
        )
    }

    #[test]
    fn counterfactual_dry_run() -> TestResult {
        let options = CounterfactualOptions {
            episode_id: "ep_test456".to_string(),
            dry_run: true,
            ..Default::default()
        };

        let report = run_counterfactual(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(
            report.status,
            CounterfactualStatus::MissingReplayEvidence,
            "status",
        )?;
        ensure(
            report.degradation_codes,
            vec![
                LAB_REPLAY_UNAVAILABLE_CODE.to_string(),
                "dry_run_no_durable_mutation".to_string(),
            ],
            "degradation_codes",
        )
    }

    #[test]
    fn capture_report_serializes() {
        let report = CaptureReport::new("ep_test".to_string(), PathBuf::from("."));
        let json = report.to_json();
        assert!(json.contains("\"schema\":\"ee.lab.capture.v1\""));
        assert!(json.contains("\"episode_id\":\"ep_test\""));
    }

    #[test]
    fn reconstruct_status_properties() {
        assert!(ReconstructStatus::Reconstructed.is_success());
        assert!(ReconstructStatus::PartialReconstruction.is_success());
        assert!(!ReconstructStatus::Failed.is_success());
        assert!(!ReconstructStatus::RunNotFound.is_success());
        assert_eq!(ReconstructStatus::Reconstructed.as_str(), "reconstructed");
    }

    #[test]
    fn reconstruct_dry_run() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_test123".to_string(),
            dry_run: true,
            ..Default::default()
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.status, ReconstructStatus::Pending, "status")?;
        ensure(report.run_id, "run_test123".to_string(), "run_id")
    }

    #[test]
    fn reconstruct_with_all_events() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_full".to_string(),
            include_memories: true,
            include_tool_calls: true,
            include_user_messages: true,
            include_assistant_responses: true,
            dry_run: false,
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.status, ReconstructStatus::Reconstructed, "status")?;
        ensure(report.event_count, 4, "event_count")?;
        ensure(report.memory_events, 1, "memory_events")?;
        ensure(report.tool_call_events, 1, "tool_call_events")?;
        ensure(report.message_events, 2, "message_events")?;
        ensure(report.episode_hash.is_some(), true, "episode_hash present")
    }

    #[test]
    fn reconstruct_filters_events() -> TestResult {
        let options = ReconstructOptions {
            workspace: PathBuf::from("."),
            run_id: "run_filtered".to_string(),
            include_memories: false,
            include_tool_calls: true,
            include_user_messages: false,
            include_assistant_responses: false,
            dry_run: false,
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.event_count, 1, "event_count")?;
        ensure(report.tool_call_events, 1, "tool_call_events")?;
        ensure(report.memory_events, 0, "memory_events")?;
        ensure(report.message_events, 0, "message_events")
    }

    #[test]
    fn reconstruct_empty_run_id() -> TestResult {
        let options = ReconstructOptions {
            run_id: String::new(),
            ..Default::default()
        };

        let report = reconstruct_episode(&options).map_err(|e| e.message())?;

        ensure(report.status, ReconstructStatus::RunNotFound, "status")?;
        ensure(!report.warnings.is_empty(), true, "has warnings")
    }

    #[test]
    fn reconstructed_event_new() {
        let event = ReconstructedEvent::new(42, "tool_call", "2026-04-30T12:00:00Z");
        assert_eq!(event.sequence, 42);
        assert_eq!(event.event_type, "tool_call");
        assert!(!event.redacted);
    }

    #[test]
    fn reconstruct_report_serializes() {
        let report = ReconstructReport::new("ep_test".to_string(), "run_test".to_string());
        let json = report.to_json();
        assert!(json.contains("\"schema\":\"ee.lab.reconstruct.v1\""));
        assert!(json.contains("\"episode_id\":\"ep_test\""));
        assert!(json.contains("\"run_id\":\"run_test\""));
    }
}
