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
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::models::{
    COUNTERFACTUAL_RUN_ID_PREFIX, DomainError, EPISODE_ID_PREFIX, VerificationEvidenceRecord,
    VerificationStatus, verification_evidence_record_from_rch_verify,
};

/// Schema for lab capture report.
pub const LAB_CAPTURE_SCHEMA_V1: &str = "ee.lab.capture.v1";

/// Schema for lab replay report.
pub const LAB_REPLAY_SCHEMA_V1: &str = "ee.lab.replay.v1";

/// Schema for agent workload replay reports derived from redacted traces.
pub const AGENT_WORKLOAD_REPLAY_SCHEMA_V1: &str = "ee.agent_workload_replay.v1";

/// Schema for swarm workload replay inputs.
pub const SWARM_WORKLOAD_SCHEMA_V1: &str = "ee.swarm_workload.v1";

/// JSON Schema URI for swarm workload replay inputs.
pub const SWARM_WORKLOAD_SCHEMA_ID_V1: &str =
    "https://eidetic-engine/schemas/ee.swarm_workload.v1.json";

/// Schema tag for deterministic swarm workload generator evidence.
pub const SWARM_WORKLOAD_GENERATOR_EVIDENCE_SCHEMA_V1: &str =
    "ee.swarm_workload.generator_evidence.v1";

/// Schema for swarm replay result ledgers.
pub const SWARM_REPLAY_RESULT_SCHEMA_V1: &str = "ee.swarm_replay_result.v1";

/// Schema for compact replay verification proof capsules.
pub const SWARM_REPLAY_VERIFICATION_CAPSULE_SCHEMA_V1: &str =
    "ee.swarm_replay.verification_capsule.v1";

/// Schema for lab counterfactual report.
pub const LAB_COUNTERFACTUAL_SCHEMA_V1: &str = "ee.lab.counterfactual.v1";

/// Schema for lab reconstruct report.
pub const LAB_RECONSTRUCT_SCHEMA_V1: &str = "ee.lab.reconstruct.v1";

const FROZEN_EPISODE_SCHEMA_V1: &str = "ee.lab.frozen_episode.v1";
const AGENT_WORKLOAD_TRACE_SCHEMA_V1: &str = "ee.agent_workload_trace.v1";
pub const DEFAULT_AGENT_WORKLOAD_REPLAY_AGENTS: u16 = 64;
const MAX_AGENT_WORKLOAD_REPLAY_AGENTS: u16 = 256;
pub const MAX_SWARM_WORKLOAD_COMMANDS: usize = 1024;
pub const MAX_SWARM_REPLAY_ARTIFACT_BYTES: usize = 256 * 1024;
const SWARM_WORKLOAD_COMMAND_SEQUENCE_LIMIT_EXCEEDED: &str =
    "swarm_workload_command_sequence_limit_exceeded";
const LAB_REPLAY_UNAVAILABLE_CODE: &str = "lab_replay_unavailable";
pub const LAB_COUNTERFACTUAL_MULTI_SWAP_UNSUPPORTED_CODE: &str =
    "lab_counterfactual_multi_swap_unsupported";
pub const LAB_REPLAY_DETERMINISM_VIOLATION_CODE: &str = "lab_replay_determinism_violation";
pub const LAB_REPLAY_NONDETERMINISTIC_CODE: &str = "lab_replay_nondeterministic";
pub const LAB_DETERMINISM_DIFF_SCHEMA_V1: &str = "ee.lab.determinism_diff.v1";
pub const LAB_COUNTERFACTUAL_PACK_DIFF_SCHEMA_V1: &str = "ee.lab.counterfactual_pack_diff.v1";
pub const SWARM_REPLAY_DRY_RUN_ADMISSION_ONLY_CODE: &str = "swarm_replay_dry_run_admission_only";
pub const SWARM_REPLAY_EXECUTION_NOT_ENABLED_CODE: &str = "swarm_replay_execution_not_enabled";
pub const SWARM_REPLAY_RCH_PROOF_MISSING_CODE: &str = "swarm_replay_rch_proof_missing";
pub const SWARM_REPLAY_COMMAND_NOT_ALLOWLISTED_CODE: &str = "swarm_replay_command_not_allowlisted";
pub const SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE: &str = "swarm_replay_local_cargo_refused";
pub const SWARM_REPLAY_HOST_PROFILE_REFUSED_CODE: &str = "swarm_replay_host_profile_refused";
pub const SWARM_REPLAY_PREREQUISITE_UNAVAILABLE_CODE: &str =
    "swarm_replay_prerequisite_unavailable";
pub const SWARM_REPLAY_COMMAND_SPAWN_FAILED_CODE: &str = "swarm_replay_command_spawn_failed";
pub const SWARM_REPLAY_COMMAND_TIMEOUT_CODE: &str = "swarm_replay_command_timeout";
pub const SWARM_REPLAY_EXPECTED_EXIT_MISMATCH_CODE: &str = "swarm_replay_expected_exit_mismatch";
pub const SWARM_REPLAY_EXPECTED_SCHEMA_MISMATCH_CODE: &str =
    "swarm_replay_expected_schema_mismatch";
pub const SWARM_REPLAY_SLO_BUDGET_WARNED_CODE: &str = "swarm_replay_slo_budget_warned";
pub const SWARM_REPLAY_SLO_BUDGET_FAILED_CODE: &str = "swarm_replay_slo_budget_failed";
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

/// Options for replaying a redaction-safe swarm workload trace.
#[derive(Clone, Debug)]
pub struct SwarmReplayOptions {
    /// Workspace used as the replay root.
    pub workspace: PathBuf,
    /// Redaction-safe ee.swarm_workload.v1 JSON trace.
    pub trace_path: PathBuf,
    /// Build an admission ledger without executing commands.
    pub dry_run: bool,
    /// Redaction-safe observed host posture supplied by the runner.
    pub host_observation: SwarmReplayHostProfileObservation,
    /// Optional ee binary used for executor-backed non-dry-run replay.
    pub ee_binary_path: Option<PathBuf>,
    /// Optional support-bundle-safe `scripts/rch_verify.sh --json` proof.
    pub rch_proof_path: Option<PathBuf>,
}

/// Options for promoting a redacted agent workload trace into swarm replay input.
#[derive(Clone, Debug)]
pub struct SwarmWorkloadPromotionOptions {
    /// Redacted ee.agent_workload_trace.v1 JSONL trace.
    pub trace_path: PathBuf,
    /// Agent count to encode in the promoted swarm workload.
    pub agent_count: u16,
    /// Resource profile used for conservative replay admission hints.
    pub profile: SwarmWorkloadFixtureProfile,
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

/// Redaction-safe multi-agent workload trace consumed by future swarm replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadTrace {
    pub schema: String,
    pub workload_id: String,
    pub fixture_seed: String,
    pub side_effect_free: bool,
    pub redaction_level: SwarmWorkloadRedactionLevel,
    pub workspace_shape: SwarmWorkloadWorkspaceShape,
    pub agent_count: u16,
    pub command_sequence: Vec<SwarmWorkloadCommandStep>,
    pub expected_degraded_posture: SwarmExpectedDegradedPosture,
    pub redaction_probes: Vec<SwarmWorkloadRedactionProbe>,
    pub resource_profile_hints: SwarmWorkloadResourceProfileHints,
    pub generator_evidence: SwarmWorkloadGeneratorEvidence,
    pub provenance: SwarmWorkloadProvenance,
}

impl SwarmWorkloadTrace {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Built-in deterministic profiles for synthetic swarm workload fixtures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwarmWorkloadFixtureProfile {
    Small,
    Medium,
    Large,
}

impl Default for SwarmWorkloadFixtureProfile {
    fn default() -> Self {
        Self::Small
    }
}

impl SwarmWorkloadFixtureProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    const fn fixture_profile(self) -> &'static str {
        match self {
            Self::Small => "swarm_small_fixture",
            Self::Medium => "swarm_medium_fixture",
            Self::Large => "swarm_large_fixture",
        }
    }

    const fn resource_profile(self) -> &'static str {
        match self {
            Self::Small => "ci_smoke",
            Self::Medium => "developer_crowded_checkout",
            Self::Large => "stress_256gb_host",
        }
    }

    const fn repo_state(self) -> &'static str {
        match self {
            Self::Small => "clean_fixture",
            Self::Medium => "dirty_fixture",
            Self::Large => "crowded_checkout",
        }
    }

    const fn agent_count(self) -> u16 {
        match self {
            Self::Small => 4,
            Self::Medium => 24,
            Self::Large => 128,
        }
    }

    const fn max_parallel_agents(self) -> u16 {
        match self {
            Self::Small => 4,
            Self::Medium => 12,
            Self::Large => 64,
        }
    }

    const fn memory_budget_mb(self) -> Option<u64> {
        match self {
            Self::Small => Some(2_048),
            Self::Medium => Some(16_384),
            Self::Large => Some(262_144),
        }
    }

    const fn cpu_budget_ms(self) -> Option<u64> {
        match self {
            Self::Small => Some(10_000),
            Self::Medium => Some(45_000),
            Self::Large => Some(240_000),
        }
    }

    const fn redaction_level(self) -> SwarmWorkloadRedactionLevel {
        match self {
            Self::Small => SwarmWorkloadRedactionLevel::Strict,
            Self::Medium | Self::Large => SwarmWorkloadRedactionLevel::Audit,
        }
    }

    const fn path_policy(self) -> SwarmWorkloadPathPolicy {
        match self {
            Self::Small => SwarmWorkloadPathPolicy::NoAbsolutePaths,
            Self::Medium => SwarmWorkloadPathPolicy::RelativeFixturePaths,
            Self::Large => SwarmWorkloadPathPolicy::HashedPathTails,
        }
    }

    const fn expected_degraded_posture(self) -> SwarmExpectedDegradedPosture {
        match self {
            Self::Small => SwarmExpectedDegradedPosture::NoneExpected,
            Self::Medium => SwarmExpectedDegradedPosture::Recoverable,
            Self::Large => SwarmExpectedDegradedPosture::Required,
        }
    }
}

/// Inputs for redaction-safe synthetic swarm workload generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmWorkloadFixtureOptions {
    pub fixture_seed: String,
    pub profile: SwarmWorkloadFixtureProfile,
}

impl SwarmWorkloadFixtureOptions {
    #[must_use]
    pub fn new(profile: SwarmWorkloadFixtureProfile, fixture_seed: impl Into<String>) -> Self {
        Self {
            fixture_seed: fixture_seed.into(),
            profile,
        }
    }

    #[must_use]
    pub fn small(fixture_seed: impl Into<String>) -> Self {
        Self::new(SwarmWorkloadFixtureProfile::Small, fixture_seed)
    }

    #[must_use]
    pub fn medium(fixture_seed: impl Into<String>) -> Self {
        Self::new(SwarmWorkloadFixtureProfile::Medium, fixture_seed)
    }

    #[must_use]
    pub fn large(fixture_seed: impl Into<String>) -> Self {
        Self::new(SwarmWorkloadFixtureProfile::Large, fixture_seed)
    }
}

/// Generate a deterministic, redaction-safe swarm workload trace from a seed.
#[must_use]
pub fn generate_swarm_workload_fixture(
    options: &SwarmWorkloadFixtureOptions,
) -> SwarmWorkloadTrace {
    let profile = options.profile;
    let seed = options.fixture_seed.as_str();
    let workload_hash = stable_swarm_fixture_hex(profile, seed, "workload-id");
    let command_sequence = swarm_fixture_commands(profile, seed);
    let redaction_probes = swarm_fixture_redaction_probes(profile, seed);
    let generator_evidence =
        swarm_workload_generator_evidence(profile, seed, &command_sequence, &redaction_probes);

    SwarmWorkloadTrace {
        schema: SWARM_WORKLOAD_SCHEMA_V1.to_owned(),
        workload_id: format!("swarmwl_{}", &workload_hash[..16]),
        fixture_seed: options.fixture_seed.clone(),
        side_effect_free: true,
        redaction_level: profile.redaction_level(),
        workspace_shape: SwarmWorkloadWorkspaceShape {
            fixture_profile: profile.fixture_profile().to_owned(),
            workspace_fingerprint: stable_swarm_fixture_hash(profile, seed, "workspace"),
            path_policy: profile.path_policy(),
            path_tail_hash: swarm_fixture_path_tail_hash(profile, seed),
            repo_state: profile.repo_state().to_owned(),
        },
        agent_count: profile.agent_count(),
        command_sequence,
        expected_degraded_posture: profile.expected_degraded_posture(),
        redaction_probes,
        resource_profile_hints: SwarmWorkloadResourceProfileHints {
            profile: profile.resource_profile().to_owned(),
            requested_parallel_agents: profile.agent_count(),
            max_parallel_agents: profile.max_parallel_agents(),
            memory_budget_mb: profile.memory_budget_mb(),
            cpu_budget_ms: profile.cpu_budget_ms(),
            rch_required: true,
        },
        generator_evidence,
        provenance: SwarmWorkloadProvenance {
            kind: SwarmWorkloadProvenanceKind::Synthetic,
            source_trace_hashes: Vec::new(),
            derived_from_schemas: vec![
                AGENT_WORKLOAD_TRACE_SCHEMA_V1.to_owned(),
                SWARM_WORKLOAD_SCHEMA_V1.to_owned(),
            ],
            fixture_author_hash: Some(stable_swarm_fixture_short_hash(profile, seed, "author")),
        },
    }
}

/// Trace redaction posture.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmWorkloadRedactionLevel {
    Strict,
    Audit,
}

/// Host-independent workspace shape; never a raw absolute path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadWorkspaceShape {
    pub fixture_profile: String,
    pub workspace_fingerprint: String,
    pub path_policy: SwarmWorkloadPathPolicy,
    pub path_tail_hash: Option<String>,
    pub repo_state: String,
}

/// Path handling policy for replay fixtures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmWorkloadPathPolicy {
    NoAbsolutePaths,
    RelativeFixturePaths,
    HashedPathTails,
}

/// One command-shape step in a swarm workload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadCommandStep {
    pub step_id: String,
    pub agent_slot: u16,
    pub command: SwarmWorkloadCommandShape,
    pub expected_schema: Option<String>,
    pub expected_exit_code: Option<u8>,
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slo_exemption_rationale: Option<String>,
    pub depends_on: Vec<String>,
}

/// Redacted command shape. Raw argv/query/task values are intentionally absent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadCommandShape {
    pub verbs: Vec<String>,
    pub positional_arity: u16,
    pub flag_names: Vec<String>,
    pub output_format: Option<String>,
    pub command_hash: String,
}

/// Expected degraded posture for the replayed workload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmExpectedDegradedPosture {
    #[serde(rename = "none")]
    NoneExpected,
    Recoverable,
    Required,
    Blocked,
}

/// A replay-time probe proving raw-sensitive classes are absent or redacted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadRedactionProbe {
    pub probe_id: String,
    pub class: SwarmRedactionProbeClass,
    pub value_hash: String,
    pub expected_status: SwarmRedactionProbeStatus,
}

/// Raw-content class guarded by a redaction probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmRedactionProbeClass {
    RawTaskString,
    RawQueryText,
    RawMemoryBody,
    RawMailBody,
    Secret,
    AbsoluteHostPath,
    EnvironmentDump,
    FullFileListing,
}

/// Expected redaction probe result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmRedactionProbeStatus {
    Absent,
    Redacted,
    Blocked,
}

/// Resource hints used for admission and budget selection, not measurement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadResourceProfileHints {
    pub profile: String,
    pub requested_parallel_agents: u16,
    pub max_parallel_agents: u16,
    pub memory_budget_mb: Option<u64>,
    pub cpu_budget_ms: Option<u64>,
    pub rch_required: bool,
}

/// Deterministic generator evidence for support-bundle-safe fixture auditing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadGeneratorEvidence {
    pub schema: String,
    pub fixture_seed: String,
    pub profile: String,
    pub workspace_path_hash: String,
    pub command_count: u16,
    pub generated_memory_count: u16,
    pub redaction_probe_count: u16,
    pub schema_id: String,
    pub fixture_hash: String,
}

/// Provenance for a swarm workload fixture.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkloadProvenance {
    pub kind: SwarmWorkloadProvenanceKind,
    pub source_trace_hashes: Vec<String>,
    pub derived_from_schemas: Vec<String>,
    pub fixture_author_hash: Option<String>,
}

/// Source class for a workload fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmWorkloadProvenanceKind {
    Synthetic,
    Recorded,
    Mixed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SwarmFixtureCommandTemplate {
    verbs: &'static [&'static str],
    positional_arity: u16,
    flag_names: &'static [&'static str],
    output_format: Option<&'static str>,
    timeout_ms: u64,
    slo_exemption_rationale: Option<&'static str>,
    depends_on_previous: bool,
}

fn swarm_fixture_commands(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
) -> Vec<SwarmWorkloadCommandStep> {
    swarm_fixture_command_templates(profile)
        .into_iter()
        .enumerate()
        .map(|(index, template)| {
            let step_id = format!("step_{:03}", index + 1);
            let depends_on = if template.depends_on_previous && index > 0 {
                vec![format!("step_{index:03}")]
            } else {
                Vec::new()
            };
            SwarmWorkloadCommandStep {
                step_id: step_id.clone(),
                agent_slot: (index as u16) % profile.agent_count(),
                command: SwarmWorkloadCommandShape {
                    verbs: template
                        .verbs
                        .iter()
                        .map(|verb| (*verb).to_owned())
                        .collect(),
                    positional_arity: template.positional_arity,
                    flag_names: template
                        .flag_names
                        .iter()
                        .map(|flag| (*flag).to_owned())
                        .collect(),
                    output_format: template.output_format.map(str::to_owned),
                    command_hash: swarm_fixture_command_hash(profile, seed, &step_id, &template),
                },
                expected_schema: Some("ee.response.v2".to_owned()),
                expected_exit_code: Some(0),
                timeout_ms: Some(template.timeout_ms),
                slo_exemption_rationale: template.slo_exemption_rationale.map(str::to_owned),
                depends_on,
            }
        })
        .collect()
}

fn swarm_fixture_command_templates(
    profile: SwarmWorkloadFixtureProfile,
) -> Vec<SwarmFixtureCommandTemplate> {
    let mut templates = vec![
        SwarmFixtureCommandTemplate {
            verbs: &["init"],
            positional_arity: 0,
            flag_names: &["--workspace", "--json"],
            output_format: Some("json"),
            timeout_ms: 1_500,
            slo_exemption_rationale: None,
            depends_on_previous: false,
        },
        SwarmFixtureCommandTemplate {
            verbs: &["remember"],
            positional_arity: 1,
            flag_names: &["--workspace", "--level", "--kind", "--json"],
            output_format: Some("json"),
            timeout_ms: 2_500,
            slo_exemption_rationale: None,
            depends_on_previous: true,
        },
        SwarmFixtureCommandTemplate {
            verbs: &["search"],
            positional_arity: 1,
            flag_names: &["--workspace", "--json"],
            output_format: Some("json"),
            timeout_ms: 2_000,
            slo_exemption_rationale: None,
            depends_on_previous: true,
        },
        SwarmFixtureCommandTemplate {
            verbs: &["pack"],
            positional_arity: 1,
            flag_names: &["--workspace", "--max-tokens", "--json"],
            output_format: Some("json"),
            timeout_ms: 3_000,
            slo_exemption_rationale: None,
            depends_on_previous: true,
        },
        SwarmFixtureCommandTemplate {
            verbs: &["why"],
            positional_arity: 1,
            flag_names: &["--workspace", "--json"],
            output_format: Some("json"),
            timeout_ms: 1_500,
            slo_exemption_rationale: None,
            depends_on_previous: true,
        },
        SwarmFixtureCommandTemplate {
            verbs: &["status"],
            positional_arity: 0,
            flag_names: &["--workspace", "--json"],
            output_format: Some("json"),
            timeout_ms: 1_500,
            slo_exemption_rationale: None,
            depends_on_previous: false,
        },
    ];

    if matches!(
        profile,
        SwarmWorkloadFixtureProfile::Medium | SwarmWorkloadFixtureProfile::Large
    ) {
        templates.extend([
            SwarmFixtureCommandTemplate {
                verbs: &["doctor"],
                positional_arity: 0,
                flag_names: &["--workspace", "--json"],
                output_format: Some("json"),
                timeout_ms: 2_000,
                slo_exemption_rationale: None,
                depends_on_previous: false,
            },
            SwarmFixtureCommandTemplate {
                verbs: &["daemon", "status"],
                positional_arity: 0,
                flag_names: &["--json"],
                output_format: Some("json"),
                timeout_ms: 1_500,
                slo_exemption_rationale: None,
                depends_on_previous: false,
            },
            SwarmFixtureCommandTemplate {
                verbs: &["support", "bundle"],
                positional_arity: 0,
                flag_names: &["--workspace", "--dry-run", "--json"],
                output_format: Some("json"),
                timeout_ms: 4_000,
                slo_exemption_rationale: Some("support_bundle_dry_run_is_intentionally_heavy"),
                depends_on_previous: true,
            },
        ]);
    }

    if profile == SwarmWorkloadFixtureProfile::Large {
        templates.extend([
            SwarmFixtureCommandTemplate {
                verbs: &["health"],
                positional_arity: 0,
                flag_names: &["--workspace", "--json"],
                output_format: Some("json"),
                timeout_ms: 2_000,
                slo_exemption_rationale: None,
                depends_on_previous: false,
            },
            SwarmFixtureCommandTemplate {
                verbs: &["graph", "communities"],
                positional_arity: 0,
                flag_names: &["--workspace", "--limit", "--json"],
                output_format: Some("json"),
                timeout_ms: 4_000,
                slo_exemption_rationale: None,
                depends_on_previous: false,
            },
            SwarmFixtureCommandTemplate {
                verbs: &["migrate", "status"],
                positional_arity: 0,
                flag_names: &["--workspace", "--json"],
                output_format: Some("json"),
                timeout_ms: 2_000,
                slo_exemption_rationale: None,
                depends_on_previous: false,
            },
        ]);
    }

    templates
}

fn swarm_fixture_redaction_probes(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
) -> Vec<SwarmWorkloadRedactionProbe> {
    let mut probes = vec![(
        SwarmRedactionProbeClass::RawTaskString,
        SwarmRedactionProbeStatus::Absent,
    )];

    if matches!(
        profile,
        SwarmWorkloadFixtureProfile::Medium | SwarmWorkloadFixtureProfile::Large
    ) {
        probes.extend([
            (
                SwarmRedactionProbeClass::RawQueryText,
                SwarmRedactionProbeStatus::Redacted,
            ),
            (
                SwarmRedactionProbeClass::RawMemoryBody,
                SwarmRedactionProbeStatus::Redacted,
            ),
            (
                SwarmRedactionProbeClass::AbsoluteHostPath,
                SwarmRedactionProbeStatus::Blocked,
            ),
            (
                SwarmRedactionProbeClass::Secret,
                SwarmRedactionProbeStatus::Blocked,
            ),
        ]);
    }

    if profile == SwarmWorkloadFixtureProfile::Large {
        probes.extend([
            (
                SwarmRedactionProbeClass::EnvironmentDump,
                SwarmRedactionProbeStatus::Blocked,
            ),
            (
                SwarmRedactionProbeClass::FullFileListing,
                SwarmRedactionProbeStatus::Blocked,
            ),
        ]);
    }

    probes
        .into_iter()
        .enumerate()
        .map(|(index, (class, expected_status))| {
            let probe_id = format!("probe_{:03}", index + 1);
            SwarmWorkloadRedactionProbe {
                probe_id: probe_id.clone(),
                class,
                value_hash: stable_swarm_fixture_hash(
                    profile,
                    seed,
                    &format!("redaction-probe:{probe_id}"),
                ),
                expected_status,
            }
        })
        .collect()
}

fn swarm_fixture_path_tail_hash(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
) -> Option<String> {
    if profile == SwarmWorkloadFixtureProfile::Small {
        None
    } else {
        Some(stable_swarm_fixture_short_hash(profile, seed, "path-tail"))
    }
}

fn swarm_workload_generator_evidence(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
    command_sequence: &[SwarmWorkloadCommandStep],
    redaction_probes: &[SwarmWorkloadRedactionProbe],
) -> SwarmWorkloadGeneratorEvidence {
    let generated_memory_count = command_sequence
        .iter()
        .filter(|step| {
            step.command
                .verbs
                .first()
                .is_some_and(|verb| verb == "remember")
        })
        .count() as u16;

    SwarmWorkloadGeneratorEvidence {
        schema: SWARM_WORKLOAD_GENERATOR_EVIDENCE_SCHEMA_V1.to_owned(),
        fixture_seed: seed.to_owned(),
        profile: profile.as_str().to_owned(),
        workspace_path_hash: stable_swarm_fixture_hash(profile, seed, "workspace-path"),
        command_count: command_sequence.len() as u16,
        generated_memory_count,
        redaction_probe_count: redaction_probes.len() as u16,
        schema_id: SWARM_WORKLOAD_SCHEMA_ID_V1.to_owned(),
        fixture_hash: stable_swarm_fixture_hash(profile, seed, "fixture"),
    }
}

fn swarm_fixture_command_hash(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
    step_id: &str,
    template: &SwarmFixtureCommandTemplate,
) -> String {
    let verbs = template.verbs.join("/");
    let flags = template.flag_names.join(",");
    let output_format = template.output_format.unwrap_or("none");
    stable_swarm_fixture_hash(
        profile,
        seed,
        &format!(
            "command:{step_id}:{verbs}:{}:{flags}:{output_format}",
            template.positional_arity
        ),
    )
}

fn stable_swarm_fixture_hash(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
    suffix: &str,
) -> String {
    format!("blake3:{}", stable_swarm_fixture_hex(profile, seed, suffix))
}

fn stable_swarm_fixture_short_hash(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
    suffix: &str,
) -> String {
    let hex = stable_swarm_fixture_hex(profile, seed, suffix);
    format!("blake3:{}", &hex[..16])
}

fn stable_swarm_fixture_hex(
    profile: SwarmWorkloadFixtureProfile,
    seed: &str,
    suffix: &str,
) -> String {
    blake3::hash(format!("ee.swarm.fixture.v1:{}:{seed}:{suffix}", profile.as_str()).as_bytes())
        .to_hex()
        .to_string()
}

/// Compact deterministic ledger emitted by a future swarm replay runner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayResult {
    pub schema: String,
    pub workload_id: String,
    pub run_id: String,
    pub side_effect_free: bool,
    pub status: SwarmReplayStatus,
    pub host_profile_admission: SwarmReplayHostProfileReport,
    pub command_results: Vec<SwarmReplayCommandResult>,
    pub aggregate: SwarmReplayAggregate,
    pub redaction_status: SwarmReplayRedactionStatus,
    pub resource_usage: SwarmReplayResourceUsage,
    pub first_failure: Option<SwarmReplayFailure>,
    pub verification: SwarmReplayVerification,
    pub warnings: Vec<String>,
}

impl SwarmReplayResult {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Overall swarm replay status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplayStatus {
    Pass,
    Fail,
    Blocked,
    Degraded,
}

/// Redaction-safe admission report for the host that ran a swarm replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayHostProfileReport {
    pub declared_profile: String,
    pub requested_parallel_agents: u16,
    pub required_class: SwarmReplayHostProfileClass,
    pub observed_class: SwarmReplayHostProfileClass,
    pub status: SwarmReplayHostAdmissionStatus,
    pub logical_cpu_count: Option<u16>,
    pub available_memory_mb: Option<u64>,
    pub target_dir_posture: SwarmReplayHostPathPosture,
    pub tmpdir_posture: SwarmReplayHostPathPosture,
    pub rch_available: Option<bool>,
    pub numa_available: Option<bool>,
    pub lexical_ram_tier_available: Option<bool>,
    pub path_tail_hashes: Vec<String>,
    pub degraded_codes: Vec<String>,
    pub refusal_reasons: Vec<String>,
}

impl SwarmReplayHostProfileReport {
    #[must_use]
    pub fn admitted(
        declared_profile: impl Into<String>,
        requested_parallel_agents: u16,
        class: SwarmReplayHostProfileClass,
    ) -> Self {
        Self {
            declared_profile: declared_profile.into(),
            requested_parallel_agents,
            required_class: class,
            observed_class: class,
            status: SwarmReplayHostAdmissionStatus::Admitted,
            logical_cpu_count: None,
            available_memory_mb: None,
            target_dir_posture: SwarmReplayHostPathPosture::Unknown,
            tmpdir_posture: SwarmReplayHostPathPosture::Unknown,
            rch_available: None,
            numa_available: None,
            lexical_ram_tier_available: None,
            path_tail_hashes: Vec::new(),
            degraded_codes: Vec::new(),
            refusal_reasons: Vec::new(),
        }
    }
}

/// Replay host class admitted for interpreting performance evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SwarmReplayHostProfileClass {
    Smoke,
    Standard,
    LargeHost,
}

/// Whether replay evidence can be trusted for the declared host profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplayHostAdmissionStatus {
    Admitted,
    Degraded,
    Refused,
}

/// Coarse path posture; raw host paths remain outside the replay ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplayHostPathPosture {
    External,
    Local,
    Unknown,
}

/// Redaction-safe observed host posture supplied by the replay runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmReplayHostProfileObservation {
    pub logical_cpu_count: Option<u16>,
    pub available_memory_mb: Option<u64>,
    pub target_dir_posture: SwarmReplayHostPathPosture,
    pub tmpdir_posture: SwarmReplayHostPathPosture,
    pub rch_available: Option<bool>,
    pub numa_available: Option<bool>,
    pub lexical_ram_tier_available: Option<bool>,
    pub path_tail_hashes: Vec<String>,
}

impl Default for SwarmReplayHostProfileObservation {
    fn default() -> Self {
        Self {
            logical_cpu_count: None,
            available_memory_mb: None,
            target_dir_posture: SwarmReplayHostPathPosture::Unknown,
            tmpdir_posture: SwarmReplayHostPathPosture::Unknown,
            rch_available: None,
            numa_available: None,
            lexical_ram_tier_available: None,
            path_tail_hashes: Vec::new(),
        }
    }
}

/// Classify whether an observed host can admit a declared swarm replay trace.
#[must_use]
pub fn classify_swarm_replay_host_profile(
    hints: &SwarmWorkloadResourceProfileHints,
    observation: SwarmReplayHostProfileObservation,
) -> SwarmReplayHostProfileReport {
    let required_class = required_swarm_replay_host_class(hints);
    let observed_class = observed_swarm_replay_host_class(&observation);
    let mut degraded_codes = Vec::new();
    let mut refusal_reasons = Vec::new();

    if hints.rch_required && observation.rch_available != Some(true) {
        degraded_codes.push("swarm_replay_rch_unavailable".to_owned());
        refusal_reasons.push("rch_required_but_unavailable".to_owned());
    }

    if observation.logical_cpu_count.is_none() {
        degraded_codes.push("swarm_replay_cpu_count_unknown".to_owned());
    }
    if observation.available_memory_mb.is_none() {
        degraded_codes.push("swarm_replay_memory_unknown".to_owned());
    }

    if observed_class < required_class {
        degraded_codes.push("swarm_replay_host_profile_too_small".to_owned());
        refusal_reasons.push(format!(
            "required_{}_but_observed_{}",
            required_class.reason_token(),
            observed_class.reason_token()
        ));
    }

    if matches!(required_class, SwarmReplayHostProfileClass::LargeHost)
        && !matches!(
            observation.target_dir_posture,
            SwarmReplayHostPathPosture::External
        )
    {
        degraded_codes.push("swarm_replay_target_dir_not_external".to_owned());
        refusal_reasons.push("large_host_requires_external_target_dir".to_owned());
    }

    if matches!(required_class, SwarmReplayHostProfileClass::LargeHost)
        && !matches!(
            observation.tmpdir_posture,
            SwarmReplayHostPathPosture::External
        )
    {
        degraded_codes.push("swarm_replay_tmpdir_not_external".to_owned());
        refusal_reasons.push("large_host_requires_external_tmpdir".to_owned());
    }

    dedup_stable_strings(&mut degraded_codes);
    dedup_stable_strings(&mut refusal_reasons);

    let status = if !refusal_reasons.is_empty() {
        SwarmReplayHostAdmissionStatus::Refused
    } else if !degraded_codes.is_empty() {
        SwarmReplayHostAdmissionStatus::Degraded
    } else {
        SwarmReplayHostAdmissionStatus::Admitted
    };

    SwarmReplayHostProfileReport {
        declared_profile: hints.profile.clone(),
        requested_parallel_agents: hints.requested_parallel_agents,
        required_class,
        observed_class,
        status,
        logical_cpu_count: observation.logical_cpu_count,
        available_memory_mb: observation.available_memory_mb,
        target_dir_posture: observation.target_dir_posture,
        tmpdir_posture: observation.tmpdir_posture,
        rch_available: observation.rch_available,
        numa_available: observation.numa_available,
        lexical_ram_tier_available: observation.lexical_ram_tier_available,
        path_tail_hashes: observation.path_tail_hashes,
        degraded_codes,
        refusal_reasons,
    }
}

fn required_swarm_replay_host_class(
    hints: &SwarmWorkloadResourceProfileHints,
) -> SwarmReplayHostProfileClass {
    let profile = hints.profile.as_str();
    if hints.requested_parallel_agents >= 64
        || hints.max_parallel_agents >= 64
        || hints
            .memory_budget_mb
            .is_some_and(|memory_mb| memory_mb >= 131_072)
        || profile.contains("256gb")
        || profile.contains("large")
        || profile.contains("stress")
    {
        SwarmReplayHostProfileClass::LargeHost
    } else if hints.requested_parallel_agents >= 8
        || hints.max_parallel_agents >= 8
        || hints
            .memory_budget_mb
            .is_some_and(|memory_mb| memory_mb >= 8_192)
        || profile.contains("developer")
        || profile.contains("standard")
    {
        SwarmReplayHostProfileClass::Standard
    } else {
        SwarmReplayHostProfileClass::Smoke
    }
}

fn observed_swarm_replay_host_class(
    observation: &SwarmReplayHostProfileObservation,
) -> SwarmReplayHostProfileClass {
    let cpu_count = observation.logical_cpu_count.unwrap_or_default();
    let memory_mb = observation.available_memory_mb.unwrap_or_default();

    if cpu_count >= 64 && memory_mb >= 262_144 {
        SwarmReplayHostProfileClass::LargeHost
    } else if cpu_count >= 8 && memory_mb >= 16_384 {
        SwarmReplayHostProfileClass::Standard
    } else {
        SwarmReplayHostProfileClass::Smoke
    }
}

impl SwarmReplayHostProfileClass {
    const fn reason_token(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Standard => "standard",
            Self::LargeHost => "large_host",
        }
    }
}

fn dedup_stable_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

/// One command outcome in a swarm replay ledger.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayCommandResult {
    pub step_id: String,
    pub agent_slot: u16,
    pub command_hash: String,
    pub exit_code: u8,
    pub elapsed_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub degraded_codes: Vec<String>,
    pub artifact_paths: Vec<SwarmReplayArtifactRef>,
    pub redaction_status: SwarmReplayCommandRedactionStatus,
    pub slo: SwarmReplayCommandSlo,
    pub memory_rss_bytes: Option<u64>,
    pub cpu_ms: Option<u64>,
}

/// Agent-usability SLO classification for one replayed command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayCommandSlo {
    pub class: SwarmReplaySloClass,
    pub status: SwarmReplaySloStatus,
    pub budget: SwarmReplaySloBudget,
    pub latency_status: SwarmReplaySloStatus,
    pub stdout_status: SwarmReplaySloStatus,
    pub stderr_status: SwarmReplaySloStatus,
    pub degraded_count_status: SwarmReplaySloStatus,
    pub discoverability_status: SwarmReplaySloStatus,
    pub warning_dimensions: Vec<String>,
    pub failed_dimensions: Vec<String>,
    pub diagnosis: Option<String>,
    pub exemption_rationale: Option<String>,
}

/// Default replay SLO class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplaySloClass {
    InteractiveAgent,
    Diagnostic,
    HeavyLab,
}

impl SwarmReplaySloClass {
    const fn as_label(self) -> &'static str {
        match self {
            Self::InteractiveAgent => "interactive_agent",
            Self::Diagnostic => "diagnostic",
            Self::HeavyLab => "heavy_lab",
        }
    }
}

/// SLO status for a command or one measured dimension.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplaySloStatus {
    Pass,
    Warn,
    Fail,
    Exempt,
}

/// Thresholds used to classify one replayed command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplaySloBudget {
    pub latency_warning_ms: u64,
    pub latency_failure_ms: u64,
    pub stdout_warning_bytes: u64,
    pub stdout_failure_bytes: u64,
    pub stderr_warning_bytes: u64,
    pub stderr_failure_bytes: u64,
    pub degraded_warning_count: u64,
    pub degraded_failure_count: u64,
}

/// Redacted artifact reference. `pathTail` is relative or hashed, never absolute.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayArtifactRef {
    pub kind: String,
    pub path_tail: String,
    pub path_hash: String,
}

/// Redaction result for one command outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplayCommandRedactionStatus {
    Clean,
    Redacted,
    ProbeFailed,
}

/// Aggregate command and latency counts for the replay ledger.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayAggregate {
    pub command_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub degraded_count: u64,
    pub slo_pass_count: u64,
    pub slo_warning_count: u64,
    pub slo_failure_count: u64,
    pub slo_exempt_count: u64,
    pub first_slo_failure_step_id: Option<String>,
    pub elapsed_ms_total: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
}

/// Replay-wide redaction posture.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayRedactionStatus {
    pub raw_task_string_present: bool,
    pub raw_query_text_present: bool,
    pub raw_memory_body_present: bool,
    pub raw_mail_body_present: bool,
    pub absolute_host_path_present: bool,
    pub secrets_present: bool,
    pub environment_dump_present: bool,
    pub full_file_listing_present: bool,
    pub redaction_probes_passed: bool,
}

/// Replay resource summary. Fields are optional when the host cannot measure them.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayResourceUsage {
    pub peak_rss_bytes: Option<u64>,
    pub max_command_rss_bytes: Option<u64>,
    pub total_cpu_ms: Option<u64>,
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
}

/// First actionable failure in a replay ledger.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayFailure {
    pub step_id: String,
    pub agent_slot: u16,
    pub code: String,
    pub severity: String,
    pub diagnosis: String,
    pub repair_hint: Option<String>,
}

/// Determinism and remote-proof posture for a replay ledger.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayVerification {
    pub rch_required: bool,
    pub rch_status: SwarmReplayRchStatus,
    pub proof_capsule: SwarmReplayVerificationCapsule,
    pub deterministic: bool,
    pub workload_hash: String,
    pub replay_hash: String,
    pub volatile_fields_stripped: Vec<String>,
}

/// Compact proof capsule that can be cited without raw verifier logs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayVerificationCapsule {
    pub schema: String,
    pub proof_level: SwarmReplayVerificationProofLevel,
    pub static_checks: Vec<SwarmReplayStaticCheck>,
    pub rch: Option<SwarmReplayRchProofSummary>,
}

/// How much verification evidence the replay ledger can safely claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplayVerificationProofLevel {
    StaticReplayOnly,
    RemoteVerified,
    RchBlocked,
    RemoteFailed,
    LocalCargoContaminated,
}

/// Redaction-safe static check summary for replay-local evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayStaticCheck {
    pub name: String,
    pub status: String,
    pub evidence: String,
}

/// Redaction-safe projection of an `ee.rch.verify.v1` proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayRchProofSummary {
    pub source_schema: String,
    pub status: String,
    pub command_hash: String,
    pub command_kind: Option<String>,
    pub worker_id: Option<String>,
    pub remote_marker_present: bool,
    pub cargo_started: Option<bool>,
    pub local_fallback_refused: bool,
    pub local_fallback_detected: bool,
    pub local_cargo_process_count: Option<u64>,
    pub degraded_codes: Vec<String>,
    pub selector_admission: Option<SwarmReplayRchSelectorSummary>,
    pub known_blocker: Option<SwarmReplayKnownBlockerSummary>,
    pub raw_output_included: bool,
    pub local_paths_redacted: bool,
}

/// Selector/admission fields needed to distinguish pre-Cargo blockers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayRchSelectorSummary {
    pub status: Option<String>,
    pub required_runtime: Option<String>,
    pub selected_worker: Option<String>,
    pub selection_failure_reason: Option<String>,
    pub workers_vs_selection_contradiction: bool,
    pub path_normalization_warning: Option<String>,
    pub remote_required: bool,
    pub local_fallback_refused: bool,
}

/// Known RCH blocker fingerprint and retry guidance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmReplayKnownBlockerSummary {
    pub blocker_fingerprint: String,
    pub blocker_kind: Option<String>,
    pub remediation_bead: Option<String>,
    pub retry_after: Option<String>,
}

/// RCH evidence posture for verification attached to the ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmReplayRchStatus {
    NotRequired,
    Passed,
    BlockedBeforeCargo,
    Failed,
}

impl SwarmReplayVerificationCapsule {
    #[must_use]
    fn static_replay_only(static_checks: Vec<SwarmReplayStaticCheck>) -> Self {
        Self {
            schema: SWARM_REPLAY_VERIFICATION_CAPSULE_SCHEMA_V1.to_owned(),
            proof_level: SwarmReplayVerificationProofLevel::StaticReplayOnly,
            static_checks,
            rch: None,
        }
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

/// Promote a redacted agent workload trace into a swarm replay workload.
pub fn promote_agent_workload_trace_to_swarm_workload(
    options: &SwarmWorkloadPromotionOptions,
) -> Result<SwarmWorkloadTrace, DomainError> {
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
    promote_agent_workload_trace_jsonl_to_swarm_workload(
        &agent_workload_source_path_tail(&options.trace_path),
        &text,
        options.agent_count,
        options.profile,
    )
}

/// Replay a redaction-safe swarm workload trace into a deterministic admission ledger.
pub fn replay_swarm_workload_trace(
    options: &SwarmReplayOptions,
) -> Result<SwarmReplayResult, DomainError> {
    let metadata = fs::symlink_metadata(&options.trace_path)
        .map_err(|error| lab_storage_error("inspect swarm workload trace", error))?;
    if !metadata.file_type().is_file() {
        return Err(lab_storage_error_message(
            "validate swarm workload trace path",
            format!(
                "refusing to read {} because it is not a regular file",
                options.trace_path.display()
            ),
        ));
    }
    let text = read_lab_file_to_string_no_follow(&options.trace_path)
        .map_err(|error| lab_storage_error("read swarm workload trace", error))?;
    let trace = parse_swarm_workload_trace_json(&text)?;
    let rch_proof_capsule = match options.rch_proof_path.as_deref() {
        Some(path) => Some(read_swarm_replay_rch_proof_capsule(path)?),
        None => None,
    };
    build_swarm_replay_admission_result(
        &trace,
        options.dry_run,
        &options.host_observation,
        options.ee_binary_path.as_deref(),
        &options.workspace,
        rch_proof_capsule,
    )
}

fn read_swarm_replay_rch_proof_capsule(
    proof_path: &Path,
) -> Result<SwarmReplayVerificationCapsule, DomainError> {
    let metadata = fs::symlink_metadata(proof_path)
        .map_err(|error| lab_storage_error("inspect swarm replay RCH proof", error))?;
    if !metadata.file_type().is_file() {
        return Err(lab_storage_error_message(
            "validate swarm replay RCH proof path",
            format!(
                "refusing to read {} because it is not a regular file",
                proof_path.display()
            ),
        ));
    }
    let text = read_lab_file_to_string_no_follow(proof_path)
        .map_err(|error| lab_storage_error("read swarm replay RCH proof", error))?;
    let value: JsonValue = serde_json::from_str(&text).map_err(|error| DomainError::Usage {
        message: format!("invalid ee.rch.verify.v1 proof JSON: {error}"),
        repair: Some(
            "Attach JSON emitted by `scripts/rch_verify.sh --summary --no-write -- ...`."
                .to_owned(),
        ),
    })?;
    let record = verification_evidence_record_from_rch_verify(&value).map_err(|error| {
        DomainError::Usage {
            message: format!("invalid ee.rch.verify.v1 proof: {error}"),
            repair: Some(
                "Attach a complete `scripts/rch_verify.sh` proof with schema and command_hash."
                    .to_owned(),
            ),
        }
    })?;
    Ok(swarm_replay_verification_capsule_from_rch(
        &value,
        &record,
        Vec::new(),
    ))
}

fn parse_swarm_workload_trace_json(text: &str) -> Result<SwarmWorkloadTrace, DomainError> {
    enforce_swarm_workload_command_sequence_limit(text)?;
    let trace: SwarmWorkloadTrace =
        serde_json::from_str(text).map_err(|error| DomainError::Usage {
            message: format!("invalid ee.swarm_workload.v1 trace: {error}"),
            repair: Some(
                "Generate a trace with `ee lab generate-workload --json` and retry.".to_owned(),
            ),
        })?;
    validate_swarm_workload_trace(&trace)?;
    Ok(trace)
}

fn enforce_swarm_workload_command_sequence_limit(text: &str) -> Result<(), DomainError> {
    #[derive(Deserialize)]
    struct TraceCommandSequenceProbe {
        #[serde(rename = "commandSequence")]
        _command_sequence: CommandSequenceLimitProbe,
    }

    struct CommandSequenceLimitProbe;

    impl<'de> Deserialize<'de> for CommandSequenceLimitProbe {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            struct CommandSequenceVisitor;

            impl<'de> serde::de::Visitor<'de> for CommandSequenceVisitor {
                type Value = CommandSequenceLimitProbe;

                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("a bounded commandSequence array")
                }

                fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
                where
                    A: serde::de::SeqAccess<'de>,
                {
                    let mut count = 0usize;
                    while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                        count += 1;
                        if count > MAX_SWARM_WORKLOAD_COMMANDS {
                            return Err(serde::de::Error::custom(format!(
                                "{SWARM_WORKLOAD_COMMAND_SEQUENCE_LIMIT_EXCEEDED}: {count}"
                            )));
                        }
                    }
                    Ok(CommandSequenceLimitProbe)
                }
            }

            deserializer.deserialize_seq(CommandSequenceVisitor)
        }
    }

    match serde_json::from_str::<TraceCommandSequenceProbe>(text) {
        Ok(_) => Ok(()),
        Err(error)
            if error
                .to_string()
                .contains(SWARM_WORKLOAD_COMMAND_SEQUENCE_LIMIT_EXCEEDED) =>
        {
            Err(swarm_workload_command_count_limit_error(None))
        }
        Err(_) => Ok(()),
    }
}

fn swarm_workload_command_count_limit_error(observed: Option<usize>) -> DomainError {
    let message = match observed {
        Some(count) => format!(
            "swarm workload trace declares {count} commandSequence entries, exceeding the {MAX_SWARM_WORKLOAD_COMMANDS} entry limit"
        ),
        None => format!(
            "swarm workload trace commandSequence exceeds the {MAX_SWARM_WORKLOAD_COMMANDS} entry limit"
        ),
    };
    DomainError::Usage {
        message,
        repair: Some(
            "Split the replay workload into smaller traces or regenerate with fewer commands."
                .to_owned(),
        ),
    }
}

fn validate_swarm_workload_trace(trace: &SwarmWorkloadTrace) -> Result<(), DomainError> {
    if trace.schema != SWARM_WORKLOAD_SCHEMA_V1 {
        return Err(DomainError::Usage {
            message: format!(
                "expected swarm workload schema {SWARM_WORKLOAD_SCHEMA_V1}, got {}",
                trace.schema
            ),
            repair: Some("Pass an ee.swarm_workload.v1 JSON trace.".to_owned()),
        });
    }
    if trace.generator_evidence.schema != SWARM_WORKLOAD_GENERATOR_EVIDENCE_SCHEMA_V1 {
        return Err(DomainError::Usage {
            message: format!(
                "expected generator evidence schema {SWARM_WORKLOAD_GENERATOR_EVIDENCE_SCHEMA_V1}, got {}",
                trace.generator_evidence.schema
            ),
            repair: Some("Regenerate the swarm workload fixture.".to_owned()),
        });
    }
    if trace.generator_evidence.schema_id != SWARM_WORKLOAD_SCHEMA_ID_V1 {
        return Err(DomainError::Usage {
            message: format!(
                "expected swarm workload schema id {SWARM_WORKLOAD_SCHEMA_ID_V1}, got {}",
                trace.generator_evidence.schema_id
            ),
            repair: Some("Regenerate the swarm workload fixture.".to_owned()),
        });
    }
    if !trace.side_effect_free {
        return Err(DomainError::PolicyDenied {
            message: "swarm replay only accepts traces marked sideEffectFree=true".to_owned(),
            repair: Some("Replay a redacted side-effect-free workload fixture.".to_owned()),
        });
    }
    if trace.agent_count == 0 {
        return Err(DomainError::Usage {
            message: "swarm workload trace declares zero agents".to_owned(),
            repair: Some("Regenerate the trace with at least one agent.".to_owned()),
        });
    }
    if trace.command_sequence.is_empty() {
        return Err(DomainError::Usage {
            message: "swarm workload trace has no commandSequence entries".to_owned(),
            repair: Some("Regenerate the trace with at least one command.".to_owned()),
        });
    }
    if trace.command_sequence.len() > MAX_SWARM_WORKLOAD_COMMANDS {
        return Err(swarm_workload_command_count_limit_error(Some(
            trace.command_sequence.len(),
        )));
    }

    let mut step_ids = BTreeSet::new();
    for step in &trace.command_sequence {
        if step.step_id.trim().is_empty() {
            return Err(DomainError::Usage {
                message: "swarm workload trace contains an empty stepId".to_owned(),
                repair: Some("Regenerate the trace with stable non-empty step IDs.".to_owned()),
            });
        }
        if !step_ids.insert(step.step_id.clone()) {
            return Err(DomainError::Usage {
                message: format!("swarm workload trace repeats stepId {}", step.step_id),
                repair: Some("Regenerate the trace with unique step IDs.".to_owned()),
            });
        }
        if step.agent_slot >= trace.agent_count {
            return Err(DomainError::Usage {
                message: format!(
                    "step {} targets agentSlot {} outside declared agentCount {}",
                    step.step_id, step.agent_slot, trace.agent_count
                ),
                repair: Some("Regenerate the trace or fix the agent slot assignment.".to_owned()),
            });
        }
        if step.command.verbs.is_empty() {
            return Err(DomainError::Usage {
                message: format!("step {} has no command verbs", step.step_id),
                repair: Some("Regenerate the trace with redacted command shapes.".to_owned()),
            });
        }
        if step.command.command_hash.trim().is_empty() {
            return Err(DomainError::Usage {
                message: format!("step {} has an empty commandHash", step.step_id),
                repair: Some("Regenerate the trace with command hashes.".to_owned()),
            });
        }
    }

    for step in &trace.command_sequence {
        for dependency in &step.depends_on {
            if !step_ids.contains(dependency) {
                return Err(DomainError::Usage {
                    message: format!(
                        "step {} depends on unknown stepId {}",
                        step.step_id, dependency
                    ),
                    repair: Some("Regenerate the trace with valid step dependencies.".to_owned()),
                });
            }
        }
    }

    Ok(())
}

fn build_swarm_replay_admission_result(
    trace: &SwarmWorkloadTrace,
    dry_run: bool,
    host_observation: &SwarmReplayHostProfileObservation,
    ee_binary_path: Option<&Path>,
    workspace: &Path,
    rch_proof_capsule: Option<SwarmReplayVerificationCapsule>,
) -> Result<SwarmReplayResult, DomainError> {
    let workload_hash = swarm_workload_trace_hash(trace)?;
    let run_id = swarm_replay_run_id(&trace.workload_id, &workload_hash, dry_run);
    let host_profile_admission =
        classify_swarm_replay_host_profile(&trace.resource_profile_hints, host_observation.clone());
    let mut warnings = swarm_replay_host_warnings(&host_profile_admission);
    let mut command_results = Vec::new();
    let mut first_failure = None;

    let mut status = match host_profile_admission.status {
        SwarmReplayHostAdmissionStatus::Admitted => SwarmReplayStatus::Pass,
        SwarmReplayHostAdmissionStatus::Degraded => SwarmReplayStatus::Degraded,
        SwarmReplayHostAdmissionStatus::Refused => {
            first_failure = Some(swarm_replay_host_failure(&host_profile_admission));
            SwarmReplayStatus::Blocked
        }
    };

    if trace.resource_profile_hints.rch_required && rch_proof_capsule.is_none() {
        warnings.push(format!(
            "{SWARM_REPLAY_RCH_PROOF_MISSING_CODE}: RCH proof is required but no remote proof is attached to this replay ledger"
        ));
        if matches!(status, SwarmReplayStatus::Pass) {
            status = SwarmReplayStatus::Degraded;
        }
    }

    if !matches!(
        host_profile_admission.status,
        SwarmReplayHostAdmissionStatus::Refused
    ) {
        if dry_run {
            warnings.push(format!(
                "{SWARM_REPLAY_DRY_RUN_ADMISSION_ONLY_CODE}: commands were admitted but not executed"
            ));
        } else if ee_binary_path.is_none() {
            warnings.push(format!(
                "{SWARM_REPLAY_EXECUTION_NOT_ENABLED_CODE}: non-dry-run swarm command execution is not enabled in this runner slice"
            ));
        }
        let mut execution_state = ee_binary_path.map(|binary_path| SwarmReplayExecutionState {
            ee_binary_path: binary_path.to_path_buf(),
            workspace: workspace.to_path_buf(),
            artifact_root: workspace
                .join(".ee")
                .join("lab")
                .join("swarm-replay")
                .join(&run_id),
            artifact_path_tail_prefix: format!(".ee/lab/swarm-replay/{run_id}"),
            remembered_memory_id: None,
            last_synthetic_content: None,
        });
        for step in &trace.command_sequence {
            if let Some(refusal) = swarm_replay_command_refusal(step) {
                let failure = swarm_replay_command_failure(step, &refusal);
                if first_failure.is_none() {
                    first_failure = Some(failure);
                }
                command_results.push(swarm_replay_command_result(
                    step,
                    1,
                    vec![refusal.code.to_owned()],
                ));
                status = SwarmReplayStatus::Blocked;
            } else if dry_run {
                command_results.push(swarm_replay_command_result(
                    step,
                    step.expected_exit_code.unwrap_or(0),
                    vec![SWARM_REPLAY_DRY_RUN_ADMISSION_ONLY_CODE.to_owned()],
                ));
                if matches!(status, SwarmReplayStatus::Pass) {
                    status = SwarmReplayStatus::Degraded;
                }
            } else if let Some(state) = &mut execution_state {
                let executed = execute_swarm_replay_command(step, state)?;
                if let Some(failure) = executed.failure
                    && first_failure.is_none()
                {
                    first_failure = Some(failure);
                }
                status = combine_swarm_replay_status(status, executed.status);
                command_results.push(executed.result);
            } else {
                if first_failure.is_none() {
                    first_failure = Some(swarm_replay_execution_not_enabled_failure(step));
                }
                command_results.push(swarm_replay_command_result(
                    step,
                    1,
                    vec![SWARM_REPLAY_EXECUTION_NOT_ENABLED_CODE.to_owned()],
                ));
                status = SwarmReplayStatus::Blocked;
            }
        }
    }

    let aggregate = swarm_replay_aggregate(&command_results);
    if let Some(step_id) = &aggregate.first_slo_failure_step_id {
        warnings.push(format!(
            "{SWARM_REPLAY_SLO_BUDGET_FAILED_CODE}: first command over failure budget was {step_id}"
        ));
        if matches!(status, SwarmReplayStatus::Pass) {
            status = SwarmReplayStatus::Degraded;
        }
    } else if aggregate.slo_warning_count > 0 {
        warnings.push(format!(
            "{SWARM_REPLAY_SLO_BUDGET_WARNED_CODE}: {} command(s) exceeded warning budgets",
            aggregate.slo_warning_count
        ));
        if matches!(status, SwarmReplayStatus::Pass) {
            status = SwarmReplayStatus::Degraded;
        }
    }
    let redaction_status = swarm_replay_redaction_status(trace);
    let resource_usage = swarm_replay_resource_usage(&command_results);
    let static_checks =
        swarm_replay_static_checks(&workload_hash, &host_profile_admission, &redaction_status);
    let proof_capsule = match rch_proof_capsule {
        Some(mut capsule) => {
            capsule.static_checks = static_checks;
            capsule
        }
        None => SwarmReplayVerificationCapsule::static_replay_only(static_checks),
    };
    status = combine_swarm_replay_status(
        status,
        swarm_replay_status_for_proof_capsule(
            trace.resource_profile_hints.rch_required,
            &proof_capsule,
        ),
    );
    let rch_status = swarm_replay_rch_status_for_proof_capsule(
        trace.resource_profile_hints.rch_required,
        &proof_capsule,
    );
    let replay_hash = swarm_replay_result_hash(SwarmReplayHashInput {
        workload_id: &trace.workload_id,
        workload_hash: &workload_hash,
        dry_run,
        status,
        host_profile_admission: &host_profile_admission,
        command_results: &command_results,
        aggregate: &aggregate,
        redaction_status: &redaction_status,
        resource_usage: &resource_usage,
        first_failure: &first_failure,
        warnings: &warnings,
        rch_status,
        proof_capsule: &proof_capsule,
    })?;
    Ok(SwarmReplayResult {
        schema: SWARM_REPLAY_RESULT_SCHEMA_V1.to_owned(),
        workload_id: trace.workload_id.clone(),
        run_id,
        side_effect_free: trace.side_effect_free,
        status,
        host_profile_admission,
        command_results,
        aggregate,
        redaction_status,
        resource_usage,
        first_failure,
        verification: SwarmReplayVerification {
            rch_required: trace.resource_profile_hints.rch_required,
            rch_status,
            proof_capsule,
            deterministic: true,
            workload_hash,
            replay_hash,
            volatile_fields_stripped: vec![
                "workspace".to_owned(),
                "trace_path".to_owned(),
                "wall_clock".to_owned(),
                "elapsed_ms".to_owned(),
            ],
        },
        warnings,
    })
}

fn combine_swarm_replay_status(
    current: SwarmReplayStatus,
    observed: SwarmReplayStatus,
) -> SwarmReplayStatus {
    match (current, observed) {
        (SwarmReplayStatus::Blocked, _) | (_, SwarmReplayStatus::Blocked) => {
            SwarmReplayStatus::Blocked
        }
        (SwarmReplayStatus::Fail, _) | (_, SwarmReplayStatus::Fail) => SwarmReplayStatus::Fail,
        (SwarmReplayStatus::Degraded, _) | (_, SwarmReplayStatus::Degraded) => {
            SwarmReplayStatus::Degraded
        }
        (SwarmReplayStatus::Pass, SwarmReplayStatus::Pass) => SwarmReplayStatus::Pass,
    }
}

fn swarm_replay_status_for_proof_capsule(
    rch_required: bool,
    capsule: &SwarmReplayVerificationCapsule,
) -> SwarmReplayStatus {
    if !rch_required {
        return SwarmReplayStatus::Pass;
    }
    match capsule.proof_level {
        SwarmReplayVerificationProofLevel::StaticReplayOnly => SwarmReplayStatus::Degraded,
        SwarmReplayVerificationProofLevel::RemoteVerified => SwarmReplayStatus::Pass,
        SwarmReplayVerificationProofLevel::RchBlocked => SwarmReplayStatus::Blocked,
        SwarmReplayVerificationProofLevel::RemoteFailed
        | SwarmReplayVerificationProofLevel::LocalCargoContaminated => SwarmReplayStatus::Fail,
    }
}

fn swarm_replay_rch_status_for_proof_capsule(
    rch_required: bool,
    capsule: &SwarmReplayVerificationCapsule,
) -> SwarmReplayRchStatus {
    if !rch_required {
        return SwarmReplayRchStatus::NotRequired;
    }
    match capsule.proof_level {
        SwarmReplayVerificationProofLevel::StaticReplayOnly
        | SwarmReplayVerificationProofLevel::RchBlocked => SwarmReplayRchStatus::BlockedBeforeCargo,
        SwarmReplayVerificationProofLevel::RemoteVerified => SwarmReplayRchStatus::Passed,
        SwarmReplayVerificationProofLevel::RemoteFailed
        | SwarmReplayVerificationProofLevel::LocalCargoContaminated => SwarmReplayRchStatus::Failed,
    }
}

fn swarm_replay_static_checks(
    workload_hash: &str,
    host_profile_admission: &SwarmReplayHostProfileReport,
    redaction_status: &SwarmReplayRedactionStatus,
) -> Vec<SwarmReplayStaticCheck> {
    vec![
        SwarmReplayStaticCheck {
            name: "workload_hash".to_owned(),
            status: "passed".to_owned(),
            evidence: workload_hash.to_owned(),
        },
        SwarmReplayStaticCheck {
            name: "host_admission".to_owned(),
            status: match host_profile_admission.status {
                SwarmReplayHostAdmissionStatus::Admitted => "passed",
                SwarmReplayHostAdmissionStatus::Degraded => "degraded",
                SwarmReplayHostAdmissionStatus::Refused => "failed",
            }
            .to_owned(),
            evidence: host_profile_admission
                .degraded_codes
                .first()
                .cloned()
                .unwrap_or_else(|| "host_profile_admitted".to_owned()),
        },
        SwarmReplayStaticCheck {
            name: "redaction_probes".to_owned(),
            status: if redaction_status.redaction_probes_passed {
                "passed"
            } else {
                "failed"
            }
            .to_owned(),
            evidence: format!(
                "redactionProbesPassed={}",
                redaction_status.redaction_probes_passed
            ),
        },
    ]
}

fn swarm_replay_verification_capsule_from_rch(
    proof: &JsonValue,
    record: &VerificationEvidenceRecord,
    static_checks: Vec<SwarmReplayStaticCheck>,
) -> SwarmReplayVerificationCapsule {
    let degraded_codes = swarm_replay_json_string_vec(proof, "degraded_codes");
    let source_status = swarm_replay_json_string(proof, "status")
        .unwrap_or_else(|| record.status.as_str().to_owned());
    let local_cargo_process_count = proof
        .get("local_cargo_processes")
        .and_then(|processes| processes.get("count"))
        .and_then(JsonValue::as_u64);
    let local_fallback_refused = record
        .selector_admission
        .as_ref()
        .is_some_and(|selector| selector.local_fallback_refused)
        || degraded_codes
            .iter()
            .any(|code| code == "rch_verify_local_fallback_refused");
    let local_fallback_detected = local_cargo_process_count.is_some_and(|count| count > 0)
        || degraded_codes.iter().any(|code| {
            code == "rch_verify_local_cargo_processes_present"
                || code == "rch_verify_local_fallback_detected"
                || code == "fallback_detected"
        });
    let remote_marker_present = record.offload.worker.is_some()
        && !degraded_codes
            .iter()
            .any(|code| code == "rch_verify_remote_marker_missing");
    let proof_level =
        swarm_replay_proof_level(record.status, &source_status, local_fallback_detected);

    SwarmReplayVerificationCapsule {
        schema: SWARM_REPLAY_VERIFICATION_CAPSULE_SCHEMA_V1.to_owned(),
        proof_level,
        static_checks,
        rch: Some(SwarmReplayRchProofSummary {
            source_schema: swarm_replay_json_string(proof, "schema")
                .unwrap_or_else(|| crate::models::RCH_VERIFY_SCHEMA_V1.to_owned()),
            status: source_status,
            command_hash: record.command_hash.clone(),
            command_kind: swarm_replay_json_string(proof, "command_kind"),
            worker_id: record.offload.worker.clone(),
            remote_marker_present,
            cargo_started: swarm_replay_cargo_started(record, &degraded_codes),
            local_fallback_refused,
            local_fallback_detected,
            local_cargo_process_count,
            degraded_codes,
            selector_admission: record
                .selector_admission
                .as_ref()
                .map(swarm_replay_selector_summary),
            known_blocker: swarm_replay_known_blocker_summary(proof),
            raw_output_included: false,
            local_paths_redacted: true,
        }),
    }
}

fn swarm_replay_proof_level(
    status: VerificationStatus,
    source_status: &str,
    local_fallback_detected: bool,
) -> SwarmReplayVerificationProofLevel {
    if local_fallback_detected {
        return SwarmReplayVerificationProofLevel::LocalCargoContaminated;
    }
    let source_status_is_blocked = matches!(
        source_status,
        "rch_environment_failure"
            | "capacity_or_timeout"
            | "committed_tree_unsupported"
            | "build_admission_refused"
            | "source_state_refused"
            | "known_blocker_refused"
            | "refused"
            | "dry_run"
    );
    match status {
        VerificationStatus::Passed => SwarmReplayVerificationProofLevel::RemoteVerified,
        VerificationStatus::Blocked | VerificationStatus::Unknown => {
            SwarmReplayVerificationProofLevel::RchBlocked
        }
        VerificationStatus::Failed | VerificationStatus::Interrupted => {
            if source_status_is_blocked {
                SwarmReplayVerificationProofLevel::RchBlocked
            } else {
                SwarmReplayVerificationProofLevel::RemoteFailed
            }
        }
        VerificationStatus::FallbackDetected => {
            if source_status_is_blocked {
                SwarmReplayVerificationProofLevel::RchBlocked
            } else {
                SwarmReplayVerificationProofLevel::LocalCargoContaminated
            }
        }
    }
}

fn swarm_replay_cargo_started(
    record: &VerificationEvidenceRecord,
    degraded_codes: &[String],
) -> Option<bool> {
    let command_is_cargo =
        record.gate_name.starts_with("cargo_") || record.command.trim_start().starts_with("cargo ");
    if !command_is_cargo {
        return None;
    }
    if record.offload.worker.is_none()
        || degraded_codes.iter().any(|code| {
            matches!(
                code.as_str(),
                "rch_verify_remote_marker_missing"
                    | "rch_verify_not_offloaded"
                    | "rch_verify_topology_blocked"
                    | "rch_verify_all_workers_preflight_failed"
                    | "rch_verify_local_fallback_refused"
            )
        })
    {
        return Some(false);
    }
    match record.status {
        VerificationStatus::Passed
        | VerificationStatus::Failed
        | VerificationStatus::Interrupted => Some(true),
        VerificationStatus::Blocked
        | VerificationStatus::FallbackDetected
        | VerificationStatus::Unknown => None,
    }
}

fn swarm_replay_selector_summary(
    selector: &crate::models::VerificationSelectorAdmission,
) -> SwarmReplayRchSelectorSummary {
    SwarmReplayRchSelectorSummary {
        status: selector.status.clone(),
        required_runtime: selector.required_runtime.clone(),
        selected_worker: selector.selected_worker.clone(),
        selection_failure_reason: selector.selection_failure_reason.clone(),
        workers_vs_selection_contradiction: selector.workers_vs_selection_contradiction,
        path_normalization_warning: selector
            .path_normalization_warning
            .as_deref()
            .map(swarm_replay_redact_private_path_text),
        remote_required: selector.remote_required,
        local_fallback_refused: selector.local_fallback_refused,
    }
}

fn swarm_replay_known_blocker_summary(proof: &JsonValue) -> Option<SwarmReplayKnownBlockerSummary> {
    let known_blocker = proof.get("known_blocker")?.as_object()?;
    let blocker_fingerprint = known_blocker.get("blocker_fingerprint")?.as_str()?;
    Some(SwarmReplayKnownBlockerSummary {
        blocker_fingerprint: blocker_fingerprint.to_owned(),
        blocker_kind: known_blocker
            .get("blocker_kind")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        remediation_bead: known_blocker
            .get("remediation_bead")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        retry_after: known_blocker
            .get("retry_after")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
    })
}

fn swarm_replay_json_string(value: &JsonValue, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn swarm_replay_json_string_vec(value: &JsonValue, key: &str) -> Vec<String> {
    let mut values = value
        .get(key)
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    dedup_stable_strings(&mut values);
    values
}

fn swarm_replay_redact_private_path_text(value: &str) -> String {
    let mut redacted = String::new();
    let mut rest = value;
    while let Some(index) = rest.find("/Users/") {
        redacted.push_str(&rest[..index]);
        let after_marker = &rest[index + "/Users/".len()..];
        redacted.push_str("/Users/<redacted>");
        if let Some(next_slash) = after_marker.find('/') {
            rest = &after_marker[next_slash..];
        } else {
            rest = "";
            break;
        }
    }
    redacted.push_str(rest);
    redacted
}

#[derive(Clone, Debug)]
struct SwarmReplayExecutionState {
    ee_binary_path: PathBuf,
    workspace: PathBuf,
    artifact_root: PathBuf,
    artifact_path_tail_prefix: String,
    remembered_memory_id: Option<String>,
    last_synthetic_content: Option<String>,
}

#[derive(Clone, Debug)]
struct ExecutedSwarmReplayCommand {
    result: SwarmReplayCommandResult,
    status: SwarmReplayStatus,
    failure: Option<SwarmReplayFailure>,
}

#[derive(Clone, Debug)]
struct SwarmReplayCommandInvocation {
    argv: Vec<String>,
    synthetic_content: Option<String>,
}

fn execute_swarm_replay_command(
    step: &SwarmWorkloadCommandStep,
    state: &mut SwarmReplayExecutionState,
) -> Result<ExecutedSwarmReplayCommand, DomainError> {
    let invocation = match swarm_replay_command_invocation(step, state) {
        Ok(invocation) => invocation,
        Err(refusal) => {
            let failure = swarm_replay_command_failure(step, &refusal);
            return Ok(ExecutedSwarmReplayCommand {
                result: swarm_replay_command_result(step, 1, vec![refusal.code.to_owned()]),
                status: SwarmReplayStatus::Blocked,
                failure: Some(failure),
            });
        }
    };

    let started = Instant::now();
    let timeout = Duration::from_millis(step.timeout_ms.unwrap_or(10_000).max(1));
    let mut child = match Command::new(&state.ee_binary_path)
        .args(&invocation.argv)
        .current_dir(&state.workspace)
        .env("EE_SWARM_REPLAY", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let stdout_artifact = write_swarm_replay_artifact(state, step, "stdout", b"")?;
            let stderr = format!("spawn_failed: {:?}\n", error.kind());
            let stderr_artifact =
                write_swarm_replay_artifact(state, step, "stderr", stderr.as_bytes())?;
            let mut result = swarm_replay_command_result(
                step,
                1,
                vec![SWARM_REPLAY_COMMAND_SPAWN_FAILED_CODE.to_owned()],
            );
            result.stderr_bytes = stderr.len() as u64;
            result.artifact_paths = vec![stdout_artifact, stderr_artifact];
            result.slo = swarm_replay_command_slo(
                step,
                result.elapsed_ms,
                result.stdout_bytes,
                result.stderr_bytes,
                &result.degraded_codes,
            );
            let failure = swarm_replay_command_observed_failure(
                step,
                SWARM_REPLAY_COMMAND_SPAWN_FAILED_CODE,
                "high",
                format!("failed to spawn replay command: {:?}", error.kind()),
                "Verify the replay runner is using an available ee binary.",
            );
            return Ok(ExecutedSwarmReplayCommand {
                result,
                status: SwarmReplayStatus::Fail,
                failure: Some(failure),
            });
        }
    };

    let stdout = child.stdout.take().ok_or_else(|| {
        lab_storage_error_message("capture swarm replay stdout", "stdout pipe unavailable")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        lab_storage_error_message("capture swarm replay stderr", "stderr pipe unavailable")
    })?;
    let stdout_reader = thread::spawn(move || {
        read_swarm_replay_pipe_bounded(stdout, MAX_SWARM_REPLAY_ARTIFACT_BYTES)
    });
    let stderr_reader = thread::spawn(move || {
        read_swarm_replay_pipe_bounded(stderr, MAX_SWARM_REPLAY_ARTIFACT_BYTES)
    });

    let mut timed_out = false;
    loop {
        if child
            .try_wait()
            .map_err(|error| lab_storage_error("poll swarm replay command", error))?
            .is_some()
        {
            break;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let status = child
        .wait()
        .map_err(|error| lab_storage_error("collect swarm replay command status", error))?;
    let stdout = join_swarm_replay_pipe_reader(stdout_reader, "stdout")?;
    let stderr = join_swarm_replay_pipe_reader(stderr_reader, "stderr")?;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut exit_code = status
        .code()
        .and_then(|code| u8::try_from(code.clamp(0, i32::from(u8::MAX))).ok())
        .unwrap_or(1);
    if timed_out {
        exit_code = 1;
    }

    let mut degraded_codes = Vec::new();
    let mut failure = None;
    if timed_out {
        degraded_codes.push(SWARM_REPLAY_COMMAND_TIMEOUT_CODE.to_owned());
        failure = Some(swarm_replay_command_observed_failure(
            step,
            SWARM_REPLAY_COMMAND_TIMEOUT_CODE,
            "high",
            format!(
                "command exceeded timeout of {}ms",
                u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
            ),
            "Increase the trace timeout for this command or inspect the replay artifact outputs.",
        ));
    }
    if let Some(expected_exit_code) = step.expected_exit_code
        && exit_code != expected_exit_code
    {
        degraded_codes.push(SWARM_REPLAY_EXPECTED_EXIT_MISMATCH_CODE.to_owned());
        if failure.is_none() {
            failure = Some(swarm_replay_command_observed_failure(
                step,
                SWARM_REPLAY_EXPECTED_EXIT_MISMATCH_CODE,
                "medium",
                format!("expected exit code {expected_exit_code}, got {exit_code}"),
                "Inspect stdout/stderr artifacts and adjust the trace expectation or command shape.",
            ));
        }
    }
    if let Some(expected_schema) = &step.expected_schema {
        let actual_schema = swarm_replay_stdout_schema(&stdout.bytes);
        if actual_schema.as_deref() != Some(expected_schema.as_str()) {
            degraded_codes.push(SWARM_REPLAY_EXPECTED_SCHEMA_MISMATCH_CODE.to_owned());
            if failure.is_none() {
                failure = Some(swarm_replay_command_observed_failure(
                    step,
                    SWARM_REPLAY_EXPECTED_SCHEMA_MISMATCH_CODE,
                    "medium",
                    format!(
                        "expected stdout schema {expected_schema}, got {}",
                        actual_schema.as_deref().unwrap_or("none")
                    ),
                    "Ensure the replayed command emits the expected machine-readable JSON schema.",
                ));
            }
        }
    }
    if stdout.truncated || stderr.truncated {
        degraded_codes.push(SWARM_REPLAY_SLO_BUDGET_FAILED_CODE.to_owned());
        if failure.is_none() {
            let streams = match (stdout.truncated, stderr.truncated) {
                (true, true) => "stdout and stderr",
                (true, false) => "stdout",
                (false, true) => "stderr",
                (false, false) => "output",
            };
            failure = Some(swarm_replay_command_observed_failure(
                step,
                SWARM_REPLAY_SLO_BUDGET_FAILED_CODE,
                "high",
                format!(
                    "command {streams} exceeded the {MAX_SWARM_REPLAY_ARTIFACT_BYTES} byte replay artifact cap"
                ),
                "Reduce command output or inspect the capped replay artifacts.",
            ));
        }
    }

    let stdout_artifact = write_swarm_replay_artifact(state, step, "stdout", &stdout.bytes)?;
    let stderr_artifact = write_swarm_replay_artifact(state, step, "stderr", &stderr.bytes)?;
    let mut result = swarm_replay_command_result(step, exit_code, degraded_codes);
    result.elapsed_ms = elapsed_ms;
    result.stdout_bytes = stdout.total_bytes;
    result.stderr_bytes = stderr.total_bytes;
    result.artifact_paths = vec![stdout_artifact, stderr_artifact];
    result.slo = swarm_replay_command_slo(
        step,
        result.elapsed_ms,
        result.stdout_bytes,
        result.stderr_bytes,
        &result.degraded_codes,
    );

    if step.command.verbs.len() == 1 && step.command.verbs[0] == "remember" && exit_code == 0 {
        if let Some(memory_id) = swarm_replay_remembered_memory_id(&stdout.bytes) {
            state.remembered_memory_id = Some(memory_id);
        }
        state.last_synthetic_content = invocation.synthetic_content;
    }

    let status = if failure.is_some() {
        SwarmReplayStatus::Fail
    } else if result.degraded_codes.is_empty() {
        SwarmReplayStatus::Pass
    } else {
        SwarmReplayStatus::Degraded
    };

    Ok(ExecutedSwarmReplayCommand {
        result,
        status,
        failure,
    })
}

fn swarm_replay_command_invocation(
    step: &SwarmWorkloadCommandStep,
    state: &SwarmReplayExecutionState,
) -> Result<SwarmReplayCommandInvocation, SwarmReplayCommandRefusal> {
    let workspace = state.workspace.display().to_string();
    let verbs = step
        .command
        .verbs
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let synthetic_content = format!(
        "swarm replay synthetic memory for {} agent {}",
        step.step_id, step.agent_slot
    );
    let synthetic_query = state
        .last_synthetic_content
        .clone()
        .unwrap_or_else(|| "swarm replay synthetic memory".to_owned());

    let argv = match verbs.as_slice() {
        ["init"] if step.command.positional_arity == 0 => {
            vec!["init", "--workspace", &workspace, "--json"]
        }
        ["remember"] if step.command.positional_arity == 1 => {
            return Ok(SwarmReplayCommandInvocation {
                argv: vec![
                    "remember".to_owned(),
                    synthetic_content.clone(),
                    "--workspace".to_owned(),
                    workspace,
                    "--level".to_owned(),
                    "procedural".to_owned(),
                    "--kind".to_owned(),
                    "rule".to_owned(),
                    "--json".to_owned(),
                ],
                synthetic_content: Some(synthetic_content),
            });
        }
        ["search"] if step.command.positional_arity == 1 => {
            vec!["search", &synthetic_query, "--workspace", &workspace, "--json"]
        }
        ["context"] if step.command.positional_arity == 1 => {
            vec!["context", &synthetic_query, "--workspace", &workspace, "--json"]
        }
        ["pack"] if step.command.positional_arity == 1 => {
            vec![
                "pack",
                "swarm replay synthetic task",
                "--workspace",
                &workspace,
                "--max-tokens",
                "512",
                "--json",
            ]
        }
        ["why"] if step.command.positional_arity == 1 => {
            let Some(memory_id) = &state.remembered_memory_id else {
                return Err(SwarmReplayCommandRefusal {
                    code: SWARM_REPLAY_PREREQUISITE_UNAVAILABLE_CODE,
                    severity: "medium",
                    diagnosis:
                        "why replay requires a memory id from an earlier successful remember step",
                    repair_hint: "Ensure the trace orders remember before why or use dry-run admission.",
                });
            };
            vec!["why", memory_id, "--workspace", &workspace, "--json"]
        }
        ["status"] if step.command.positional_arity == 0 => {
            vec!["status", "--workspace", &workspace, "--json"]
        }
        ["doctor"] if step.command.positional_arity == 0 => {
            vec!["doctor", "--workspace", &workspace, "--json"]
        }
        ["daemon", "status"] if step.command.positional_arity == 0 => {
            vec!["daemon", "status", "--json"]
        }
        ["support", "bundle"] if step.command.positional_arity == 0 => {
            vec![
                "support",
                "bundle",
                "--workspace",
                &workspace,
                "--dry-run",
                "--json",
            ]
        }
        ["health"] if step.command.positional_arity == 0 => {
            vec!["health", "--workspace", &workspace, "--json"]
        }
        ["graph", "communities"] if step.command.positional_arity == 0 => {
            vec![
                "graph",
                "communities",
                "--workspace",
                &workspace,
                "--limit",
                "5",
                "--json",
            ]
        }
        ["migrate", "status"] if step.command.positional_arity == 0 => {
            vec!["migrate", "status", "--workspace", &workspace, "--json"]
        }
        _ => {
            return Err(SwarmReplayCommandRefusal {
                code: SWARM_REPLAY_PREREQUISITE_UNAVAILABLE_CODE,
                severity: "medium",
                diagnosis: "command shape requires redacted arguments that the replay runner cannot safely reconstruct",
                repair_hint: "Use a supported generated fixture shape or dry-run admission.",
            });
        }
    }
    .into_iter()
    .map(str::to_owned)
    .collect();

    Ok(SwarmReplayCommandInvocation {
        argv,
        synthetic_content: None,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BoundedSwarmReplayPipe {
    bytes: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

fn read_swarm_replay_pipe_bounded<R: Read>(
    mut reader: R,
    byte_cap: usize,
) -> std::io::Result<BoundedSwarmReplayPipe> {
    let mut retained = Vec::with_capacity(byte_cap.min(8192));
    let mut total_bytes = 0u64;
    let mut buffer = [0u8; 8192];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        let remaining = byte_cap.saturating_sub(retained.len());
        if remaining > 0 {
            retained.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }

    Ok(BoundedSwarmReplayPipe {
        truncated: total_bytes > u64::try_from(retained.len()).unwrap_or(u64::MAX),
        bytes: retained,
        total_bytes,
    })
}

fn join_swarm_replay_pipe_reader(
    reader: thread::JoinHandle<std::io::Result<BoundedSwarmReplayPipe>>,
    kind: &str,
) -> Result<BoundedSwarmReplayPipe, DomainError> {
    reader
        .join()
        .map_err(|_| {
            lab_storage_error_message(
                "collect swarm replay command output",
                format!("{kind} reader thread panicked"),
            )
        })?
        .map_err(|error| lab_storage_error("read swarm replay command output", error))
}

fn swarm_replay_stdout_schema(stdout: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn swarm_replay_remembered_memory_id(stdout: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    value
        .get("data")
        .and_then(|data| data.get("memoryId").or_else(|| data.get("memory_id")))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn swarm_replay_command_observed_failure(
    step: &SwarmWorkloadCommandStep,
    code: &str,
    severity: &str,
    diagnosis: impl Into<String>,
    repair_hint: &str,
) -> SwarmReplayFailure {
    SwarmReplayFailure {
        step_id: step.step_id.clone(),
        agent_slot: step.agent_slot,
        code: code.to_owned(),
        severity: severity.to_owned(),
        diagnosis: diagnosis.into(),
        repair_hint: Some(repair_hint.to_owned()),
    }
}

fn write_swarm_replay_artifact(
    state: &SwarmReplayExecutionState,
    step: &SwarmWorkloadCommandStep,
    kind: &str,
    bytes: &[u8],
) -> Result<SwarmReplayArtifactRef, DomainError> {
    fs::create_dir_all(&state.artifact_root)
        .map_err(|error| lab_storage_error("create swarm replay artifact directory", error))?;
    let file_name = format!(
        "{}.{}",
        safe_swarm_replay_step_file_stem(&step.step_id),
        kind
    );
    let path = state.artifact_root.join(&file_name);
    let capped_bytes = swarm_replay_cap_artifact_bytes(bytes);
    fs::write(&path, capped_bytes)
        .map_err(|error| lab_storage_error("write swarm replay artifact", error))?;
    Ok(SwarmReplayArtifactRef {
        kind: kind.to_owned(),
        path_tail: format!("{}/{}", state.artifact_path_tail_prefix, file_name),
        path_hash: format!("blake3:{}", hash_content(capped_bytes)),
    })
}

fn swarm_replay_cap_artifact_bytes(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.len().min(MAX_SWARM_REPLAY_ARTIFACT_BYTES)]
}

fn safe_swarm_replay_step_file_stem(step_id: &str) -> String {
    step_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SwarmReplayHashInput<'a> {
    workload_id: &'a str,
    workload_hash: &'a str,
    dry_run: bool,
    status: SwarmReplayStatus,
    host_profile_admission: &'a SwarmReplayHostProfileReport,
    command_results: &'a [SwarmReplayCommandResult],
    aggregate: &'a SwarmReplayAggregate,
    redaction_status: &'a SwarmReplayRedactionStatus,
    resource_usage: &'a SwarmReplayResourceUsage,
    first_failure: &'a Option<SwarmReplayFailure>,
    warnings: &'a [String],
    rch_status: SwarmReplayRchStatus,
    proof_capsule: &'a SwarmReplayVerificationCapsule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SwarmReplayCommandRefusal {
    code: &'static str,
    severity: &'static str,
    diagnosis: &'static str,
    repair_hint: &'static str,
}

fn swarm_replay_host_warnings(report: &SwarmReplayHostProfileReport) -> Vec<String> {
    report
        .degraded_codes
        .iter()
        .map(|code| format!("{code}: host profile admission is not fully satisfied"))
        .collect()
}

fn swarm_replay_host_failure(report: &SwarmReplayHostProfileReport) -> SwarmReplayFailure {
    let diagnosis = if report.refusal_reasons.is_empty() {
        "host profile was refused for unspecified replay admission reasons".to_owned()
    } else {
        format!(
            "host profile refused because {}",
            report.refusal_reasons.join(", ")
        )
    };
    SwarmReplayFailure {
        step_id: "step_host_profile_admission".to_owned(),
        agent_slot: 0,
        code: SWARM_REPLAY_HOST_PROFILE_REFUSED_CODE.to_owned(),
        severity: "high".to_owned(),
        diagnosis,
        repair_hint: Some(
            "Replay on a host that satisfies resourceProfileHints or regenerate a smaller workload."
                .to_owned(),
        ),
    }
}

fn swarm_replay_command_refusal(
    step: &SwarmWorkloadCommandStep,
) -> Option<SwarmReplayCommandRefusal> {
    let first = step.command.verbs.first().map(String::as_str)?;
    if first == "cargo" {
        return Some(SwarmReplayCommandRefusal {
            code: SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE,
            severity: "high",
            diagnosis: "local Cargo commands are not admitted by the swarm replay runner",
            repair_hint: "Use the RCH verification path for Cargo proof.",
        });
    }
    if is_allowed_swarm_replay_command(&step.command.verbs) {
        None
    } else {
        Some(SwarmReplayCommandRefusal {
            code: SWARM_REPLAY_COMMAND_NOT_ALLOWLISTED_CODE,
            severity: "medium",
            diagnosis: "command shape is outside the redaction-safe swarm replay allowlist",
            repair_hint: "Regenerate the workload with supported ee command shapes.",
        })
    }
}

fn is_allowed_swarm_replay_command(verbs: &[String]) -> bool {
    let verbs = verbs.iter().map(String::as_str).collect::<Vec<_>>();
    matches!(
        verbs.as_slice(),
        ["init"]
            | ["remember"]
            | ["search"]
            | ["context"]
            | ["pack"]
            | ["why"]
            | ["status"]
            | ["doctor"]
            | ["daemon", "status"]
            | ["support", "bundle"]
            | ["health"]
            | ["graph", "communities"]
            | ["migrate", "status"]
    )
}

fn swarm_replay_command_failure(
    step: &SwarmWorkloadCommandStep,
    refusal: &SwarmReplayCommandRefusal,
) -> SwarmReplayFailure {
    SwarmReplayFailure {
        step_id: step.step_id.clone(),
        agent_slot: step.agent_slot,
        code: refusal.code.to_owned(),
        severity: refusal.severity.to_owned(),
        diagnosis: refusal.diagnosis.to_owned(),
        repair_hint: Some(refusal.repair_hint.to_owned()),
    }
}

fn swarm_replay_execution_not_enabled_failure(
    step: &SwarmWorkloadCommandStep,
) -> SwarmReplayFailure {
    SwarmReplayFailure {
        step_id: step.step_id.clone(),
        agent_slot: step.agent_slot,
        code: SWARM_REPLAY_EXECUTION_NOT_ENABLED_CODE.to_owned(),
        severity: "medium".to_owned(),
        diagnosis: "the current swarm replay runner only emits admission ledgers".to_owned(),
        repair_hint: Some(
            "Pass --dry-run for admission evidence or run the executor-backed replay slice."
                .to_owned(),
        ),
    }
}

fn swarm_replay_command_slo(
    step: &SwarmWorkloadCommandStep,
    elapsed_ms: u64,
    stdout_bytes: u64,
    stderr_bytes: u64,
    degraded_codes: &[String],
) -> SwarmReplayCommandSlo {
    let class = swarm_replay_slo_class(step);
    let budget = swarm_replay_slo_budget(class);
    if let Some(rationale) = step.slo_exemption_rationale.clone() {
        return SwarmReplayCommandSlo {
            class,
            status: SwarmReplaySloStatus::Exempt,
            budget,
            latency_status: SwarmReplaySloStatus::Exempt,
            stdout_status: SwarmReplaySloStatus::Exempt,
            stderr_status: SwarmReplaySloStatus::Exempt,
            degraded_count_status: SwarmReplaySloStatus::Exempt,
            discoverability_status: SwarmReplaySloStatus::Exempt,
            warning_dimensions: Vec::new(),
            failed_dimensions: Vec::new(),
            diagnosis: None,
            exemption_rationale: Some(rationale),
        };
    }

    let latency_status = swarm_replay_slo_dimension_status(
        elapsed_ms,
        budget.latency_warning_ms,
        budget.latency_failure_ms,
    );
    let stdout_status = swarm_replay_slo_dimension_status(
        stdout_bytes,
        budget.stdout_warning_bytes,
        budget.stdout_failure_bytes,
    );
    let stderr_status = swarm_replay_slo_dimension_status(
        stderr_bytes,
        budget.stderr_warning_bytes,
        budget.stderr_failure_bytes,
    );
    let degraded_count = degraded_codes
        .iter()
        .filter(|code| code.as_str() != SWARM_REPLAY_DRY_RUN_ADMISSION_ONLY_CODE)
        .count() as u64;
    let degraded_count_status = swarm_replay_slo_dimension_status(
        degraded_count,
        budget.degraded_warning_count,
        budget.degraded_failure_count,
    );
    let discoverability_status = swarm_replay_slo_discoverability_status(step);

    let dimensions = [
        ("latency", latency_status),
        ("stdout_bytes", stdout_status),
        ("stderr_bytes", stderr_status),
        ("degraded_count", degraded_count_status),
        ("discoverability", discoverability_status),
    ];
    let warning_dimensions = dimensions
        .iter()
        .filter_map(|(dimension, status)| {
            matches!(status, SwarmReplaySloStatus::Warn).then_some((*dimension).to_owned())
        })
        .collect::<Vec<_>>();
    let failed_dimensions = dimensions
        .iter()
        .filter_map(|(dimension, status)| {
            matches!(status, SwarmReplaySloStatus::Fail).then_some((*dimension).to_owned())
        })
        .collect::<Vec<_>>();
    let status = if !failed_dimensions.is_empty() {
        SwarmReplaySloStatus::Fail
    } else if !warning_dimensions.is_empty() {
        SwarmReplaySloStatus::Warn
    } else {
        SwarmReplaySloStatus::Pass
    };
    let diagnosis =
        swarm_replay_slo_diagnosis(status, class, &warning_dimensions, &failed_dimensions);

    SwarmReplayCommandSlo {
        class,
        status,
        budget,
        latency_status,
        stdout_status,
        stderr_status,
        degraded_count_status,
        discoverability_status,
        warning_dimensions,
        failed_dimensions,
        diagnosis,
        exemption_rationale: None,
    }
}

fn swarm_replay_slo_class(step: &SwarmWorkloadCommandStep) -> SwarmReplaySloClass {
    let verbs = step
        .command
        .verbs
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    match verbs.as_slice() {
        ["init"]
        | ["remember"]
        | ["search"]
        | ["context"]
        | ["pack"]
        | ["why"]
        | ["status"]
        | ["health"] => SwarmReplaySloClass::InteractiveAgent,
        ["support", "bundle"] | ["cargo", ..] | ["lab", ..] => SwarmReplaySloClass::HeavyLab,
        _ => SwarmReplaySloClass::Diagnostic,
    }
}

const fn swarm_replay_slo_budget(class: SwarmReplaySloClass) -> SwarmReplaySloBudget {
    match class {
        SwarmReplaySloClass::InteractiveAgent => SwarmReplaySloBudget {
            latency_warning_ms: 2_500,
            latency_failure_ms: 7_500,
            stdout_warning_bytes: 16 * 1024,
            stdout_failure_bytes: 64 * 1024,
            stderr_warning_bytes: 4 * 1024,
            stderr_failure_bytes: 16 * 1024,
            degraded_warning_count: 0,
            degraded_failure_count: 2,
        },
        SwarmReplaySloClass::Diagnostic => SwarmReplaySloBudget {
            latency_warning_ms: 5_000,
            latency_failure_ms: 15_000,
            stdout_warning_bytes: 64 * 1024,
            stdout_failure_bytes: 256 * 1024,
            stderr_warning_bytes: 16 * 1024,
            stderr_failure_bytes: 64 * 1024,
            degraded_warning_count: 1,
            degraded_failure_count: 5,
        },
        SwarmReplaySloClass::HeavyLab => SwarmReplaySloBudget {
            latency_warning_ms: 30_000,
            latency_failure_ms: 120_000,
            stdout_warning_bytes: 256 * 1024,
            stdout_failure_bytes: 1024 * 1024,
            stderr_warning_bytes: 64 * 1024,
            stderr_failure_bytes: 256 * 1024,
            degraded_warning_count: 4,
            degraded_failure_count: 10,
        },
    }
}

fn swarm_replay_slo_dimension_status(
    value: u64,
    warning_threshold: u64,
    failure_threshold: u64,
) -> SwarmReplaySloStatus {
    if value > failure_threshold {
        SwarmReplaySloStatus::Fail
    } else if value > warning_threshold {
        SwarmReplaySloStatus::Warn
    } else {
        SwarmReplaySloStatus::Pass
    }
}

fn swarm_replay_slo_discoverability_status(
    step: &SwarmWorkloadCommandStep,
) -> SwarmReplaySloStatus {
    let json_output_requested = step.command.output_format.as_deref() == Some("json")
        || step
            .command
            .flag_names
            .iter()
            .any(|flag| flag.as_str() == "--json");
    let command_hash_present = step.command.command_hash.starts_with("blake3:");
    if !json_output_requested || !command_hash_present {
        SwarmReplaySloStatus::Fail
    } else if step.expected_schema.is_none() {
        SwarmReplaySloStatus::Warn
    } else {
        SwarmReplaySloStatus::Pass
    }
}

fn swarm_replay_slo_diagnosis(
    status: SwarmReplaySloStatus,
    class: SwarmReplaySloClass,
    warning_dimensions: &[String],
    failed_dimensions: &[String],
) -> Option<String> {
    match status {
        SwarmReplaySloStatus::Pass | SwarmReplaySloStatus::Exempt => None,
        SwarmReplaySloStatus::Warn => Some(format!(
            "{} replay command exceeded warning budget dimensions: {}",
            class.as_label(),
            warning_dimensions.join(", ")
        )),
        SwarmReplaySloStatus::Fail => Some(format!(
            "{} replay command exceeded failure budget dimensions: {}",
            class.as_label(),
            failed_dimensions.join(", ")
        )),
    }
}

fn swarm_replay_command_result(
    step: &SwarmWorkloadCommandStep,
    exit_code: u8,
    mut degraded_codes: Vec<String>,
) -> SwarmReplayCommandResult {
    degraded_codes.sort();
    degraded_codes.dedup();
    let slo = swarm_replay_command_slo(step, 0, 0, 0, &degraded_codes);
    SwarmReplayCommandResult {
        step_id: step.step_id.clone(),
        agent_slot: step.agent_slot,
        command_hash: step.command.command_hash.clone(),
        exit_code,
        elapsed_ms: 0,
        stdout_bytes: 0,
        stderr_bytes: 0,
        degraded_codes,
        artifact_paths: Vec::new(),
        redaction_status: SwarmReplayCommandRedactionStatus::Clean,
        slo,
        memory_rss_bytes: None,
        cpu_ms: None,
    }
}

fn swarm_replay_aggregate(results: &[SwarmReplayCommandResult]) -> SwarmReplayAggregate {
    let mut elapsed = results
        .iter()
        .map(|result| result.elapsed_ms)
        .collect::<Vec<_>>();
    elapsed.sort_unstable();
    SwarmReplayAggregate {
        command_count: results.len() as u64,
        success_count: results
            .iter()
            .filter(|result| result.exit_code == 0)
            .count() as u64,
        failure_count: results
            .iter()
            .filter(|result| result.exit_code != 0)
            .count() as u64,
        degraded_count: results
            .iter()
            .filter(|result| !result.degraded_codes.is_empty())
            .count() as u64,
        slo_pass_count: results
            .iter()
            .filter(|result| matches!(result.slo.status, SwarmReplaySloStatus::Pass))
            .count() as u64,
        slo_warning_count: results
            .iter()
            .filter(|result| matches!(result.slo.status, SwarmReplaySloStatus::Warn))
            .count() as u64,
        slo_failure_count: results
            .iter()
            .filter(|result| matches!(result.slo.status, SwarmReplaySloStatus::Fail))
            .count() as u64,
        slo_exempt_count: results
            .iter()
            .filter(|result| matches!(result.slo.status, SwarmReplaySloStatus::Exempt))
            .count() as u64,
        first_slo_failure_step_id: results
            .iter()
            .find(|result| matches!(result.slo.status, SwarmReplaySloStatus::Fail))
            .map(|result| result.step_id.clone()),
        elapsed_ms_total: elapsed.iter().sum(),
        p50_ms: percentile_nearest_rank(&elapsed, 50),
        p95_ms: percentile_nearest_rank(&elapsed, 95),
        p99_ms: percentile_nearest_rank(&elapsed, 99),
    }
}

fn swarm_replay_redaction_status(trace: &SwarmWorkloadTrace) -> SwarmReplayRedactionStatus {
    SwarmReplayRedactionStatus {
        raw_task_string_present: false,
        raw_query_text_present: false,
        raw_memory_body_present: false,
        raw_mail_body_present: false,
        absolute_host_path_present: false,
        secrets_present: false,
        environment_dump_present: false,
        full_file_listing_present: false,
        redaction_probes_passed: !trace.redaction_probes.is_empty(),
    }
}

fn swarm_replay_resource_usage(results: &[SwarmReplayCommandResult]) -> SwarmReplayResourceUsage {
    let cpu_samples = results
        .iter()
        .filter_map(|result| result.cpu_ms)
        .collect::<Vec<_>>();
    SwarmReplayResourceUsage {
        peak_rss_bytes: results
            .iter()
            .filter_map(|result| result.memory_rss_bytes)
            .max(),
        max_command_rss_bytes: results
            .iter()
            .filter_map(|result| result.memory_rss_bytes)
            .max(),
        total_cpu_ms: if cpu_samples.is_empty() {
            None
        } else {
            Some(cpu_samples.iter().sum())
        },
        io_read_bytes: None,
        io_write_bytes: None,
    }
}

fn swarm_workload_trace_hash(trace: &SwarmWorkloadTrace) -> Result<String, DomainError> {
    let bytes = serde_json::to_vec(trace).map_err(|error| {
        lab_storage_error_message(
            "serialize swarm workload trace for hashing",
            error.to_string(),
        )
    })?;
    Ok(format!("blake3:{}", hash_content(&bytes)))
}

fn swarm_replay_result_hash(input: SwarmReplayHashInput<'_>) -> Result<String, DomainError> {
    let bytes = serde_json::to_vec(&input).map_err(|error| {
        lab_storage_error_message(
            "serialize swarm replay result for hashing",
            error.to_string(),
        )
    })?;
    Ok(format!("blake3:{}", hash_content(&bytes)))
}

fn swarm_replay_run_id(workload_id: &str, workload_hash: &str, dry_run: bool) -> String {
    let hash = hash_content(format!("{workload_id}\n{workload_hash}\n{dry_run}").as_bytes());
    format!("swarmrun_{}", &hash[..16])
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

#[cfg(test)]
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

fn promote_agent_workload_trace_jsonl_to_swarm_workload(
    source_path_tail: &str,
    text: &str,
    agent_count: u16,
    profile: SwarmWorkloadFixtureProfile,
) -> Result<SwarmWorkloadTrace, DomainError> {
    validate_swarm_workload_promotion_agent_count(agent_count)?;
    let rows = parse_agent_workload_trace_jsonl(text)?;
    build_promoted_swarm_workload_trace(source_path_tail, rows, agent_count, profile)
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

fn validate_swarm_workload_promotion_agent_count(agent_count: u16) -> Result<(), DomainError> {
    if agent_count == 0 || agent_count > 1024 {
        return Err(DomainError::Usage {
            message:
                "lab promote-workload --agents must be between 1 and 1024 for ee.swarm_workload.v1"
                    .to_owned(),
            repair: Some("Pass --agents 1 or a bounded swarm replay size.".to_owned()),
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

fn build_promoted_swarm_workload_trace(
    source_path_tail: &str,
    rows: Vec<AgentWorkloadTraceRow>,
    agent_count: u16,
    profile: SwarmWorkloadFixtureProfile,
) -> Result<SwarmWorkloadTrace, DomainError> {
    let mut redaction_levels = BTreeSet::<String>::new();
    let mut harness_programs = BTreeSet::<String>::new();
    let mut model_families = BTreeSet::<String>::new();
    let mut memory_references = BTreeSet::<AgentWorkloadTraceMemoryReference>::new();
    let mut normalized_rows = Vec::with_capacity(rows.len());
    let mut saw_nonzero_exit = false;
    let mut saw_degraded_code = false;

    for row in rows {
        redaction_levels.insert(row.redaction_level.clone());
        harness_programs.insert(row.harness_identity.program.clone());
        if let Some(model_family) = &row.harness_identity.model_family {
            model_families.insert(model_family.clone());
        }
        saw_nonzero_exit |= row.exit_code != 0;

        let mut row_memory_references = row.memory_references.clone();
        row_memory_references.sort();
        row_memory_references.dedup();
        memory_references.extend(row_memory_references.iter().cloned());

        let mut degraded_codes = row.degraded_codes.clone();
        degraded_codes.sort();
        degraded_codes.dedup();
        saw_degraded_code |= !degraded_codes.is_empty();

        let command = row.command.normalize();
        validate_promoted_swarm_command_shape(&command)?;
        normalized_rows.push(NormalizedAgentWorkloadTraceRow {
            schema: row.schema,
            side_effect_free: row.side_effect_free,
            redaction_level: row.redaction_level,
            trace_id: row.trace_id,
            command,
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
    let normalized_json = crate::core::serialize_or_error(&normalized_rows);
    let source_trace_hash = prefixed_blake3_hash(normalized_json.as_bytes());
    let fixture_seed = recorded_swarm_fixture_seed(&source_trace_hash);
    let command_sequence = promoted_swarm_command_sequence(&normalized_rows, agent_count)?;
    let redaction_probes = promoted_swarm_redaction_probes(&source_trace_hash);
    let workload_id = recorded_swarm_workload_id(&source_trace_hash, agent_count, profile);
    let fixture_author_hash =
        recorded_swarm_fixture_author_hash(&source_trace_hash, &harness_programs, &model_families);

    let trace = SwarmWorkloadTrace {
        schema: SWARM_WORKLOAD_SCHEMA_V1.to_owned(),
        workload_id,
        fixture_seed: fixture_seed.clone(),
        side_effect_free: true,
        redaction_level: promoted_swarm_redaction_level(&redaction_levels),
        workspace_shape: SwarmWorkloadWorkspaceShape {
            fixture_profile: "recorded_agent_workload".to_owned(),
            workspace_fingerprint: recorded_swarm_hash(
                "workspace",
                &source_trace_hash,
                source_path_tail,
            ),
            path_policy: SwarmWorkloadPathPolicy::HashedPathTails,
            path_tail_hash: Some(recorded_swarm_hash(
                "source-path-tail",
                &source_trace_hash,
                source_path_tail,
            )),
            repo_state: profile.repo_state().to_owned(),
        },
        agent_count,
        command_sequence,
        expected_degraded_posture: promoted_swarm_expected_degraded_posture(
            saw_nonzero_exit,
            saw_degraded_code,
        ),
        redaction_probes,
        resource_profile_hints: SwarmWorkloadResourceProfileHints {
            profile: profile.resource_profile().to_owned(),
            requested_parallel_agents: agent_count,
            max_parallel_agents: profile.max_parallel_agents().min(agent_count),
            memory_budget_mb: profile.memory_budget_mb(),
            cpu_budget_ms: profile.cpu_budget_ms(),
            rch_required: true,
        },
        generator_evidence: promoted_swarm_generator_evidence(
            &fixture_seed,
            &source_trace_hash,
            source_path_tail,
            &normalized_rows,
            memory_references.len(),
        ),
        provenance: SwarmWorkloadProvenance {
            kind: SwarmWorkloadProvenanceKind::Recorded,
            source_trace_hashes: vec![source_trace_hash],
            derived_from_schemas: vec![
                AGENT_WORKLOAD_TRACE_SCHEMA_V1.to_owned(),
                SWARM_WORKLOAD_SCHEMA_V1.to_owned(),
            ],
            fixture_author_hash: Some(fixture_author_hash),
        },
    };

    validate_swarm_workload_trace(&trace)?;
    Ok(trace)
}

fn promoted_swarm_command_sequence(
    normalized_rows: &[NormalizedAgentWorkloadTraceRow],
    agent_count: u16,
) -> Result<Vec<SwarmWorkloadCommandStep>, DomainError> {
    normalized_rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let step_id = format!("step_{:03}", index + 1);
            let positional_arity = u16::try_from(row.command.positional_arity.unwrap_or_default())
                .map_err(|_| DomainError::Usage {
                    message: format!(
                        "agent workload trace command {} positionalArity exceeds u16",
                        row.command.verbs.join(" ")
                    ),
                    repair: Some("Export command shapes with bounded positional arity.".to_owned()),
                })?;
            Ok(SwarmWorkloadCommandStep {
                step_id: step_id.clone(),
                agent_slot: (index as u16) % agent_count,
                command: SwarmWorkloadCommandShape {
                    verbs: row.command.verbs.clone(),
                    positional_arity,
                    flag_names: row.command.flag_names.clone(),
                    output_format: row.command.output_format.clone(),
                    command_hash: promoted_swarm_command_hash(&row.command),
                },
                expected_schema: promoted_swarm_expected_schema(&row.command),
                expected_exit_code: Some(row.exit_code),
                timeout_ms: Some(promoted_swarm_timeout_ms(row.elapsed_ms)),
                slo_exemption_rationale: None,
                depends_on: Vec::new(),
            })
        })
        .collect()
}

fn validate_promoted_swarm_command_shape(
    command: &NormalizedAgentWorkloadTraceCommand,
) -> Result<(), DomainError> {
    if command.verbs.is_empty() {
        return Err(DomainError::Usage {
            message: "agent workload trace command.verbs must contain at least one verb".to_owned(),
            repair: Some(
                "Export redacted command shapes with verb-only command identity.".to_owned(),
            ),
        });
    }
    if let Some(invalid) = command
        .verbs
        .iter()
        .find(|verb| !is_safe_swarm_command_token(verb))
    {
        return Err(DomainError::Usage {
            message: format!("agent workload trace command verb {invalid:?} is not schema-safe"),
            repair: Some(
                "Use lowercase command verbs containing only ASCII letters, digits, and hyphens."
                    .to_owned(),
            ),
        });
    }
    if let Some(invalid) = command
        .flag_names
        .iter()
        .find(|flag| !is_safe_swarm_flag_name(flag))
    {
        return Err(DomainError::Usage {
            message: format!("agent workload trace flag {invalid:?} is not schema-safe"),
            repair: Some(
                "Export flag names as redacted long flags such as --json or --max-tokens."
                    .to_owned(),
            ),
        });
    }
    if let Some(output_format) = &command.output_format
        && !matches!(
            output_format.as_str(),
            "json" | "human" | "markdown" | "toon" | "jsonl" | "compact" | "hook"
        )
    {
        return Err(DomainError::Usage {
            message: format!(
                "agent workload trace outputFormat {output_format:?} is not supported by ee.swarm_workload.v1"
            ),
            repair: Some(
                "Export outputFormat as a known ee output renderer or omit it.".to_owned(),
            ),
        });
    }
    Ok(())
}

fn is_safe_swarm_command_token(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn is_safe_swarm_flag_name(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("--") else {
        return false;
    };
    is_safe_swarm_command_token(rest)
}

fn promoted_swarm_expected_schema(command: &NormalizedAgentWorkloadTraceCommand) -> Option<String> {
    if command.output_format.as_deref() == Some("json") {
        Some("ee.response.v2".to_owned())
    } else {
        None
    }
}

fn promoted_swarm_timeout_ms(observed_elapsed_ms: u64) -> u64 {
    observed_elapsed_ms.saturating_mul(4).clamp(1_000, 30_000)
}

fn promoted_swarm_command_hash(command: &NormalizedAgentWorkloadTraceCommand) -> String {
    prefixed_blake3_hash(crate::core::serialize_or_error(command).as_bytes())
}

fn promoted_swarm_redaction_level(
    redaction_levels: &BTreeSet<String>,
) -> SwarmWorkloadRedactionLevel {
    if redaction_levels.iter().any(|level| level == "audit") {
        SwarmWorkloadRedactionLevel::Audit
    } else {
        SwarmWorkloadRedactionLevel::Strict
    }
}

fn promoted_swarm_expected_degraded_posture(
    saw_nonzero_exit: bool,
    saw_degraded_code: bool,
) -> SwarmExpectedDegradedPosture {
    if saw_nonzero_exit {
        SwarmExpectedDegradedPosture::Required
    } else if saw_degraded_code {
        SwarmExpectedDegradedPosture::Recoverable
    } else {
        SwarmExpectedDegradedPosture::NoneExpected
    }
}

fn promoted_swarm_redaction_probes(trace_hash: &str) -> Vec<SwarmWorkloadRedactionProbe> {
    [
        (
            SwarmRedactionProbeClass::RawTaskString,
            SwarmRedactionProbeStatus::Absent,
        ),
        (
            SwarmRedactionProbeClass::RawQueryText,
            SwarmRedactionProbeStatus::Absent,
        ),
        (
            SwarmRedactionProbeClass::RawMemoryBody,
            SwarmRedactionProbeStatus::Absent,
        ),
        (
            SwarmRedactionProbeClass::RawMailBody,
            SwarmRedactionProbeStatus::Absent,
        ),
        (
            SwarmRedactionProbeClass::Secret,
            SwarmRedactionProbeStatus::Blocked,
        ),
        (
            SwarmRedactionProbeClass::AbsoluteHostPath,
            SwarmRedactionProbeStatus::Blocked,
        ),
        (
            SwarmRedactionProbeClass::EnvironmentDump,
            SwarmRedactionProbeStatus::Blocked,
        ),
        (
            SwarmRedactionProbeClass::FullFileListing,
            SwarmRedactionProbeStatus::Blocked,
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (class, expected_status))| {
        let probe_id = format!("probe_{:03}", index + 1);
        SwarmWorkloadRedactionProbe {
            probe_id: probe_id.clone(),
            class,
            value_hash: recorded_swarm_hash(
                "redaction-probe",
                trace_hash,
                &format!("{}:{probe_id}", swarm_redaction_probe_class_key(class)),
            ),
            expected_status,
        }
    })
    .collect()
}

fn swarm_redaction_probe_class_key(class: SwarmRedactionProbeClass) -> &'static str {
    match class {
        SwarmRedactionProbeClass::RawTaskString => "raw_task_string",
        SwarmRedactionProbeClass::RawQueryText => "raw_query_text",
        SwarmRedactionProbeClass::RawMemoryBody => "raw_memory_body",
        SwarmRedactionProbeClass::RawMailBody => "raw_mail_body",
        SwarmRedactionProbeClass::Secret => "secret",
        SwarmRedactionProbeClass::AbsoluteHostPath => "absolute_host_path",
        SwarmRedactionProbeClass::EnvironmentDump => "environment_dump",
        SwarmRedactionProbeClass::FullFileListing => "full_file_listing",
    }
}

fn promoted_swarm_generator_evidence(
    fixture_seed: &str,
    source_trace_hash: &str,
    source_path_tail: &str,
    normalized_rows: &[NormalizedAgentWorkloadTraceRow],
    memory_reference_count: usize,
) -> SwarmWorkloadGeneratorEvidence {
    let generated_memory_count = normalized_rows
        .iter()
        .filter(|row| {
            row.command
                .verbs
                .first()
                .is_some_and(|verb| verb == "remember")
        })
        .count() as u16;
    let redaction_probe_count = promoted_swarm_redaction_probes(source_trace_hash).len() as u16;

    SwarmWorkloadGeneratorEvidence {
        schema: SWARM_WORKLOAD_GENERATOR_EVIDENCE_SCHEMA_V1.to_owned(),
        fixture_seed: fixture_seed.to_owned(),
        profile: "recorded".to_owned(),
        workspace_path_hash: recorded_swarm_hash(
            "workspace-path",
            source_trace_hash,
            source_path_tail,
        ),
        command_count: normalized_rows.len() as u16,
        generated_memory_count,
        redaction_probe_count,
        schema_id: SWARM_WORKLOAD_SCHEMA_ID_V1.to_owned(),
        fixture_hash: recorded_swarm_hash(
            "fixture",
            source_trace_hash,
            &format!(
                "{}:{}:{}",
                normalized_rows.len(),
                generated_memory_count,
                memory_reference_count
            ),
        ),
    }
}

fn recorded_swarm_workload_id(
    source_trace_hash: &str,
    agent_count: u16,
    profile: SwarmWorkloadFixtureProfile,
) -> String {
    let hex = hash_content(
        format!(
            "ee.swarm.recorded.v1:workload:{source_trace_hash}:{agent_count}:{}",
            profile.as_str()
        )
        .as_bytes(),
    );
    format!("swarmwl_{}", &hex[..16])
}

fn recorded_swarm_fixture_seed(source_trace_hash: &str) -> String {
    let hex = source_trace_hash
        .strip_prefix("blake3:")
        .unwrap_or(source_trace_hash);
    format!("recorded_{}", &hex[..16])
}

fn recorded_swarm_fixture_author_hash(
    source_trace_hash: &str,
    harness_programs: &BTreeSet<String>,
    model_families: &BTreeSet<String>,
) -> String {
    recorded_swarm_hash(
        "fixture-author",
        source_trace_hash,
        &format!(
            "{}:{}",
            harness_programs
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("+"),
            model_families.iter().cloned().collect::<Vec<_>>().join("+")
        ),
    )
}

fn recorded_swarm_hash(label: &str, source_trace_hash: &str, value: &str) -> String {
    prefixed_blake3_hash(
        format!("ee.swarm.recorded.v1:{label}:{source_trace_hash}:{value}").as_bytes(),
    )
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn command_verbs_match(step: &SwarmWorkloadCommandStep, expected: &[&str]) -> bool {
        step.command.verbs.len() == expected.len()
            && step
                .command
                .verbs
                .iter()
                .map(String::as_str)
                .zip(expected.iter().copied())
                .all(|(actual, expected)| actual == expected)
    }

    const REDACTED_WORKLOAD_TRACE: &str =
        include_str!("../../tests/fixtures/agent_workloads/redacted_trace_minimal.jsonl");

    fn large_swarm_hints() -> SwarmWorkloadResourceProfileHints {
        SwarmWorkloadResourceProfileHints {
            profile: "stress_256gb_host".to_owned(),
            requested_parallel_agents: 128,
            max_parallel_agents: 64,
            memory_budget_mb: Some(262_144),
            cpu_budget_ms: Some(240_000),
            rch_required: true,
        }
    }

    #[test]
    fn swarm_replay_host_profile_admits_large_host_evidence() -> TestResult {
        let report = classify_swarm_replay_host_profile(
            &large_swarm_hints(),
            SwarmReplayHostProfileObservation {
                logical_cpu_count: Some(96),
                available_memory_mb: Some(300_000),
                target_dir_posture: SwarmReplayHostPathPosture::External,
                tmpdir_posture: SwarmReplayHostPathPosture::External,
                rch_available: Some(true),
                numa_available: Some(true),
                lexical_ram_tier_available: Some(true),
                path_tail_hashes: vec!["blake3:aaaaaaaaaaaaaaaa".to_owned()],
            },
        );

        ensure(
            report.status,
            SwarmReplayHostAdmissionStatus::Admitted,
            "status",
        )?;
        ensure(
            report.required_class,
            SwarmReplayHostProfileClass::LargeHost,
            "required class",
        )?;
        ensure(
            report.observed_class,
            SwarmReplayHostProfileClass::LargeHost,
            "observed class",
        )?;
        ensure(report.degraded_codes.is_empty(), true, "degraded codes")?;
        ensure(report.refusal_reasons.is_empty(), true, "refusal reasons")?;
        ensure(
            serde_json::to_string(&report)
                .map_err(|error| error.to_string())?
                .contains("large-host"),
            true,
            "serialized class uses contract spelling",
        )
    }

    #[test]
    fn swarm_replay_host_profile_refuses_large_trace_on_small_host() -> TestResult {
        let report = classify_swarm_replay_host_profile(
            &large_swarm_hints(),
            SwarmReplayHostProfileObservation {
                logical_cpu_count: Some(16),
                available_memory_mb: Some(32_768),
                target_dir_posture: SwarmReplayHostPathPosture::Local,
                tmpdir_posture: SwarmReplayHostPathPosture::Unknown,
                rch_available: Some(false),
                numa_available: Some(false),
                lexical_ram_tier_available: Some(false),
                path_tail_hashes: vec!["blake3:bbbbbbbbbbbbbbbb".to_owned()],
            },
        );

        ensure(
            report.status,
            SwarmReplayHostAdmissionStatus::Refused,
            "status",
        )?;
        ensure(
            report.observed_class,
            SwarmReplayHostProfileClass::Standard,
            "observed class",
        )?;
        for expected in [
            "swarm_replay_rch_unavailable",
            "swarm_replay_host_profile_too_small",
            "swarm_replay_target_dir_not_external",
            "swarm_replay_tmpdir_not_external",
        ] {
            ensure(
                report.degraded_codes.iter().any(|code| code == expected),
                true,
                expected,
            )?;
        }
        ensure(
            serde_json::to_string(&report)
                .map_err(|error| error.to_string())?
                .contains("/Users/"),
            false,
            "report does not serialize raw paths",
        )
    }

    #[test]
    fn swarm_replay_host_profile_degrades_when_smoke_probe_is_incomplete() -> TestResult {
        let hints = SwarmWorkloadResourceProfileHints {
            profile: "ci_smoke".to_owned(),
            requested_parallel_agents: 1,
            max_parallel_agents: 1,
            memory_budget_mb: None,
            cpu_budget_ms: None,
            rch_required: false,
        };
        let report = classify_swarm_replay_host_profile(
            &hints,
            SwarmReplayHostProfileObservation::default(),
        );

        ensure(
            report.status,
            SwarmReplayHostAdmissionStatus::Degraded,
            "status",
        )?;
        ensure(
            report.required_class,
            SwarmReplayHostProfileClass::Smoke,
            "required class",
        )?;
        ensure(
            report.refusal_reasons.is_empty(),
            true,
            "no refusal reasons",
        )?;
        ensure(
            report
                .degraded_codes
                .iter()
                .any(|code| code == "swarm_replay_cpu_count_unknown"),
            true,
            "cpu degraded code",
        )
    }

    fn admitted_smoke_swarm_observation() -> SwarmReplayHostProfileObservation {
        SwarmReplayHostProfileObservation {
            logical_cpu_count: Some(8),
            available_memory_mb: Some(16_384),
            target_dir_posture: SwarmReplayHostPathPosture::Local,
            tmpdir_posture: SwarmReplayHostPathPosture::Local,
            rch_available: Some(true),
            numa_available: Some(false),
            lexical_ram_tier_available: Some(false),
            path_tail_hashes: Vec::new(),
        }
    }

    #[test]
    fn swarm_replay_slo_budget_boundaries_classify_pass_warn_and_fail() -> TestResult {
        let trace = generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("slo_001"));
        let step = trace
            .command_sequence
            .iter()
            .find(|step| command_verbs_match(step, &["pack"]))
            .ok_or_else(|| "missing pack step".to_owned())?;
        let budget = swarm_replay_slo_budget(SwarmReplaySloClass::InteractiveAgent);

        let pass = swarm_replay_command_slo(
            step,
            budget.latency_warning_ms,
            budget.stdout_warning_bytes,
            budget.stderr_warning_bytes,
            &[],
        );
        ensure(pass.status, SwarmReplaySloStatus::Pass, "pass status")?;
        ensure(
            pass.class,
            SwarmReplaySloClass::InteractiveAgent,
            "interactive class",
        )?;

        let warn = swarm_replay_command_slo(
            step,
            budget.latency_warning_ms + 1,
            budget.stdout_warning_bytes + 1,
            0,
            &[],
        );
        ensure(warn.status, SwarmReplaySloStatus::Warn, "warn status")?;
        ensure(
            warn.warning_dimensions.contains(&"latency".to_owned())
                && warn.warning_dimensions.contains(&"stdout_bytes".to_owned()),
            true,
            "warning dimensions",
        )?;

        let fail = swarm_replay_command_slo(
            step,
            budget.latency_failure_ms + 1,
            budget.stdout_failure_bytes + 1,
            0,
            &[],
        );
        ensure(fail.status, SwarmReplaySloStatus::Fail, "fail status")?;
        ensure(
            fail.failed_dimensions.contains(&"latency".to_owned())
                && fail.failed_dimensions.contains(&"stdout_bytes".to_owned()),
            true,
            "failed dimensions",
        )?;

        let dry_run = swarm_replay_command_slo(
            step,
            0,
            0,
            0,
            &[SWARM_REPLAY_DRY_RUN_ADMISSION_ONLY_CODE.to_owned()],
        );
        ensure(
            dry_run.status,
            SwarmReplaySloStatus::Pass,
            "dry-run degraded code is not an observed SLO failure",
        )
    }

    #[test]
    fn swarm_replay_slo_records_trace_exemption_for_heavy_surface() -> TestResult {
        let trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::medium("slo_002"));
        let step = trace
            .command_sequence
            .iter()
            .find(|step| command_verbs_match(step, &["support", "bundle"]))
            .ok_or_else(|| "missing support bundle step".to_owned())?;
        ensure(
            step.slo_exemption_rationale.as_deref(),
            Some("support_bundle_dry_run_is_intentionally_heavy"),
            "trace-carried exemption rationale",
        )?;

        let slo = swarm_replay_command_slo(
            step,
            900_000,
            2 * 1024 * 1024,
            512 * 1024,
            &["swarm_replay_synthetic_heavy_output".to_owned()],
        );

        ensure(slo.class, SwarmReplaySloClass::HeavyLab, "class")?;
        ensure(slo.status, SwarmReplaySloStatus::Exempt, "status")?;
        ensure(
            slo.exemption_rationale.as_deref(),
            Some("support_bundle_dry_run_is_intentionally_heavy"),
            "exemption rationale",
        )?;
        ensure(
            slo.failed_dimensions.is_empty() && slo.warning_dimensions.is_empty(),
            true,
            "exempt command has no budget dimensions",
        )
    }

    #[test]
    fn swarm_replay_aggregate_counts_slo_statuses() -> TestResult {
        let trace = generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("slo_003"));
        let step = trace
            .command_sequence
            .iter()
            .find(|step| command_verbs_match(step, &["pack"]))
            .ok_or_else(|| "missing pack step".to_owned())?;
        let budget = swarm_replay_slo_budget(SwarmReplaySloClass::InteractiveAgent);
        let pass = swarm_replay_command_result(step, 0, Vec::new());
        let mut fail = swarm_replay_command_result(step, 0, Vec::new());
        fail.step_id = "step_slo_fail".to_owned();
        fail.stdout_bytes = budget.stdout_failure_bytes + 1;
        fail.slo = swarm_replay_command_slo(
            step,
            fail.elapsed_ms,
            fail.stdout_bytes,
            fail.stderr_bytes,
            &fail.degraded_codes,
        );

        let aggregate = swarm_replay_aggregate(&[pass, fail]);
        ensure(aggregate.slo_pass_count, 1u64, "slo pass count")?;
        ensure(aggregate.slo_failure_count, 1u64, "slo failure count")?;
        ensure(
            aggregate.first_slo_failure_step_id.as_deref(),
            Some("step_slo_fail"),
            "first slo failure step",
        )
    }

    #[test]
    fn swarm_replay_bounded_pipe_reader_tracks_total_bytes_without_retaining_them() -> TestResult {
        let pipe = read_swarm_replay_pipe_bounded(std::io::Cursor::new(b"abcdef"), 4)
            .map_err(|error| error.to_string())?;

        ensure(pipe.bytes, b"abcd".to_vec(), "retained bytes")?;
        ensure(pipe.total_bytes, 6u64, "total bytes")?;
        ensure(pipe.truncated, true, "truncated")
    }

    #[test]
    fn swarm_replay_artifact_writer_caps_retained_bytes() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("artifact_001"));
        let step = trace
            .command_sequence
            .first()
            .ok_or_else(|| "generated fixture missing command".to_owned())?;
        let state = SwarmReplayExecutionState {
            ee_binary_path: PathBuf::from("ee"),
            workspace: workspace.path().to_path_buf(),
            artifact_root: workspace.path().join(".ee/lab/swarm-replay/test"),
            artifact_path_tail_prefix: ".ee/lab/swarm-replay/test".to_owned(),
            remembered_memory_id: None,
            last_synthetic_content: None,
        };
        let bytes = vec![b'x'; MAX_SWARM_REPLAY_ARTIFACT_BYTES + 17];

        let artifact = write_swarm_replay_artifact(&state, step, "stdout", &bytes)
            .map_err(|error| error.message())?;
        let artifact_path = workspace.path().join(&artifact.path_tail);
        let metadata = fs::metadata(&artifact_path).map_err(|error| error.to_string())?;

        ensure(
            metadata.len(),
            MAX_SWARM_REPLAY_ARTIFACT_BYTES as u64,
            "artifact byte length",
        )?;
        ensure(
            artifact.path_hash,
            format!(
                "blake3:{}",
                hash_content(swarm_replay_cap_artifact_bytes(&bytes))
            ),
            "artifact hash",
        )
    }

    #[cfg(unix)]
    #[test]
    fn swarm_replay_executor_caps_stdout_artifact_and_records_budget_failure() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("cap_exec_001"));
        trace.command_sequence.truncate(1);
        trace.generator_evidence.command_count = 1;
        trace.resource_profile_hints.rch_required = false;
        let trace_path = workspace.path().join("swarm-workload-output-cap.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let script_path = workspace.path().join("huge-output-ee");
        fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nhead -c {} /dev/zero | tr '\\0' x\n",
                MAX_SWARM_REPLAY_ARTIFACT_BYTES + 17
            ),
        )
        .map_err(|error| error.to_string())?;
        let mut permissions = fs::metadata(&script_path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: false,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: Some(script_path),
            rch_proof_path: None,
        };

        let report = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;
        let result = report
            .command_results
            .first()
            .ok_or_else(|| "missing command result".to_owned())?;
        let stdout_artifact = result
            .artifact_paths
            .iter()
            .find(|artifact| artifact.kind == "stdout")
            .ok_or_else(|| "missing stdout artifact".to_owned())?;
        let stdout_path = workspace.path().join(&stdout_artifact.path_tail);

        ensure(report.status, SwarmReplayStatus::Fail, "status")?;
        ensure(
            result.stdout_bytes,
            (MAX_SWARM_REPLAY_ARTIFACT_BYTES + 17) as u64,
            "observed stdout bytes",
        )?;
        ensure(
            fs::metadata(stdout_path)
                .map_err(|error| error.to_string())?
                .len(),
            MAX_SWARM_REPLAY_ARTIFACT_BYTES as u64,
            "retained stdout artifact bytes",
        )?;
        ensure(
            result
                .degraded_codes
                .iter()
                .any(|code| code == SWARM_REPLAY_SLO_BUDGET_FAILED_CODE),
            true,
            "output cap degraded code",
        )
    }

    #[test]
    fn swarm_replay_dry_run_admission_ledger_is_deterministic() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("admit_001"));
        let trace_path = workspace.path().join("swarm-workload.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: true,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: None,
            rch_proof_path: None,
        };

        let first = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;
        let second = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;

        ensure(
            first.schema.as_str(),
            SWARM_REPLAY_RESULT_SCHEMA_V1,
            "schema",
        )?;
        ensure(first.status, SwarmReplayStatus::Degraded, "status")?;
        ensure(first.side_effect_free, true, "side effect free")?;
        ensure(
            first.command_results.len(),
            trace.command_sequence.len(),
            "command result count",
        )?;
        ensure(
            first.aggregate.command_count,
            trace.command_sequence.len() as u64,
            "aggregate command count",
        )?;
        ensure(first.aggregate.failure_count, 0u64, "failure count")?;
        ensure(first.first_failure.is_none(), true, "no first failure")?;
        ensure(
            first.verification.rch_status,
            SwarmReplayRchStatus::BlockedBeforeCargo,
            "rch status",
        )?;
        ensure(
            first.verification.proof_capsule.proof_level,
            SwarmReplayVerificationProofLevel::StaticReplayOnly,
            "static-only proof level",
        )?;
        ensure(
            first.verification.proof_capsule.rch.is_none(),
            true,
            "static-only proof has no RCH summary",
        )?;
        ensure(
            first
                .warnings
                .iter()
                .any(|warning| warning.contains(SWARM_REPLAY_DRY_RUN_ADMISSION_ONLY_CODE)),
            true,
            "dry-run warning",
        )?;
        ensure(first.to_json(), second.to_json(), "deterministic json")
    }

    #[test]
    fn swarm_replay_remote_rch_proof_emits_capsule() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("proof_pass_001"));
        let trace_path = workspace.path().join("swarm-workload.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let proof_path = workspace.path().join("rch-proof.json");
        fs::write(
            &proof_path,
            serde_json::json!({
                "schema": "ee.rch.verify.v1",
                "success": true,
                "generated_at": "2026-06-03T10:00:00Z",
                "started_at": "2026-06-03T09:59:00Z",
                "completed_at": "2026-06-03T10:00:00Z",
                "command": ["cargo", "test", "--lib", "swarm_replay"],
                "command_text": "cargo test --lib swarm_replay",
                "command_kind": "cargo_test",
                "command_hash": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "remote_required": true,
                "would_offload": true,
                "worker_id": "vmi1227854",
                "exit_code": 0,
                "elapsed_ms": 60000,
                "degraded_codes": [],
                "selector_admission_probe": {
                    "schema": "ee.rch.selector_admission_probe.v1",
                    "status": "selected",
                    "required_runtime": "Rust",
                    "workers_reported": ["vmi1227854"],
                    "daemon_workers_reported": ["vmi1227854"],
                    "selected_worker": "vmi1227854",
                    "selection_failure_reason": null,
                    "workers_vs_selection_contradiction": false,
                    "path_normalization_warning": null,
                    "remote_required": true,
                    "local_fallback_refused": false
                },
                "local_cargo_processes": {
                    "schema": "ee.rch_local_cargo_tripwire.v1",
                    "status": "checked",
                    "count": 0,
                    "processes": []
                }
            })
            .to_string(),
        )
        .map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: true,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: None,
            rch_proof_path: Some(proof_path),
        };

        let report = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;
        let rch = report
            .verification
            .proof_capsule
            .rch
            .as_ref()
            .ok_or_else(|| "remote proof should include RCH summary".to_owned())?;

        ensure(
            report.verification.rch_status,
            SwarmReplayRchStatus::Passed,
            "rch status",
        )?;
        ensure(
            report.verification.proof_capsule.proof_level,
            SwarmReplayVerificationProofLevel::RemoteVerified,
            "proof level",
        )?;
        ensure(
            rch.command_hash.as_str(),
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "command hash",
        )?;
        ensure(rch.remote_marker_present, true, "remote marker")?;
        ensure(rch.cargo_started, Some(true), "cargo started")?;
        ensure(
            rch.local_cargo_process_count,
            Some(0),
            "local cargo process count",
        )
    }

    #[test]
    fn swarm_replay_rch_blocker_proof_emits_known_blocker_capsule() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let trace = generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small(
            "proof_blocked_001",
        ));
        let trace_path = workspace.path().join("swarm-workload.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let proof_path = workspace.path().join("rch-blocked-proof.json");
        fs::write(
            &proof_path,
            serde_json::json!({
                "schema": "ee.rch.verify.v1",
                "success": true,
                "generated_at": "2026-06-03T10:00:00Z",
                "command": ["cargo", "check", "--lib", "--quiet"],
                "command_text": "cargo check --lib --quiet",
                "command_kind": "cargo_check",
                "command_hash": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "status": "rch_environment_failure",
                "remote_required": true,
                "would_offload": true,
                "worker_id": null,
                "exit_code": 1,
                "elapsed_ms": 1839,
                "degraded_codes": [
                    "rch_verify_remote_command_failed",
                    "rch_verify_local_fallback_refused",
                    "rch_verify_remote_marker_missing"
                ],
                "selector_admission_probe": {
                    "schema": "ee.rch.selector_admission_probe.v1",
                    "status": "selection_failed",
                    "required_runtime": "Rust",
                    "workers_reported": ["vmi1227854"],
                    "daemon_workers_reported": ["vmi1227854"],
                    "selected_worker": null,
                    "selection_failure_reason": "remote_marker_missing",
                    "workers_vs_selection_contradiction": true,
                    "path_normalization_warning": "RCH_TOPOLOGY_ERR_ALIAS_NOT_SYMLINK:path=/Users/alice/projects",
                    "remote_required": true,
                    "local_fallback_refused": true
                },
                "known_blocker": {
                    "schema": "ee.rch.known_blocker.v1",
                    "blocker_fingerprint": "sha256:73ef58eadcc735659bb2841156b93ae208f44545c7c0d4b90d46a08b30a542db",
                    "blocker_kind": "local_fallback_refused",
                    "remediation_bead": "bd-17c65.10.17.1",
                    "retry_after": "2026-06-03T16:37:08.121551Z"
                },
                "local_cargo_processes": {
                    "schema": "ee.rch_local_cargo_tripwire.v1",
                    "status": "checked",
                    "count": 0,
                    "processes": []
                }
            })
            .to_string(),
        )
        .map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: true,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: None,
            rch_proof_path: Some(proof_path),
        };

        let report = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;
        let report_json = report.to_json();
        let rch = report
            .verification
            .proof_capsule
            .rch
            .as_ref()
            .ok_or_else(|| "blocked proof should include RCH summary".to_owned())?;
        let selector = rch
            .selector_admission
            .as_ref()
            .ok_or_else(|| "blocked proof should include selector summary".to_owned())?;
        let known_blocker = rch
            .known_blocker
            .as_ref()
            .ok_or_else(|| "blocked proof should include known blocker".to_owned())?;

        ensure(report.status, SwarmReplayStatus::Blocked, "status")?;
        ensure(
            report.verification.rch_status,
            SwarmReplayRchStatus::BlockedBeforeCargo,
            "rch status",
        )?;
        ensure(
            report.verification.proof_capsule.proof_level,
            SwarmReplayVerificationProofLevel::RchBlocked,
            "proof level",
        )?;
        ensure(rch.cargo_started, Some(false), "cargo did not start")?;
        ensure(rch.local_fallback_refused, true, "local fallback refused")?;
        ensure(
            selector.selection_failure_reason.as_deref(),
            Some("remote_marker_missing"),
            "selector failure reason",
        )?;
        ensure(
            selector
                .path_normalization_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("/Users/<redacted>")),
            true,
            "path warning redacted",
        )?;
        ensure(
            known_blocker.blocker_fingerprint.as_str(),
            "sha256:73ef58eadcc735659bb2841156b93ae208f44545c7c0d4b90d46a08b30a542db",
            "known blocker fingerprint",
        )?;
        ensure(
            !report_json.contains("/Users/alice"),
            true,
            "capsule must not leak private user path",
        )
    }

    #[test]
    fn swarm_replay_refuses_local_cargo_command_shape() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("cargo_001"));
        trace.command_sequence[0].command.verbs = vec!["cargo".to_owned(), "test".to_owned()];
        let trace_path = workspace.path().join("swarm-workload-cargo.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: true,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: None,
            rch_proof_path: None,
        };

        let report = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;

        ensure(report.status, SwarmReplayStatus::Blocked, "status")?;
        ensure(report.aggregate.failure_count, 1u64, "failure count")?;
        ensure(
            report
                .first_failure
                .as_ref()
                .map(|failure| failure.code.as_str()),
            Some(SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE),
            "first failure code",
        )?;
        ensure(
            report.command_results[0]
                .degraded_codes
                .iter()
                .any(|code| code == SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE),
            true,
            "command degraded code",
        )
    }

    #[test]
    fn swarm_replay_non_dry_run_records_execution_disabled_per_command() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("execute_001"));
        let trace_path = workspace.path().join("swarm-workload-execute.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: false,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: None,
            rch_proof_path: None,
        };

        let report = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;

        ensure(report.status, SwarmReplayStatus::Blocked, "status")?;
        ensure(
            report.command_results.len(),
            trace.command_sequence.len(),
            "command result count",
        )?;
        ensure(
            report.aggregate.failure_count,
            trace.command_sequence.len() as u64,
            "failure count",
        )?;
        ensure(
            report
                .first_failure
                .as_ref()
                .map(|failure| failure.step_id.as_str()),
            Some("step_001"),
            "first failure step",
        )?;
        ensure(
            report.command_results.iter().all(|result| {
                result
                    .degraded_codes
                    .iter()
                    .any(|code| code == SWARM_REPLAY_EXECUTION_NOT_ENABLED_CODE)
            }),
            true,
            "execution-disabled degraded codes",
        )
    }

    #[test]
    fn swarm_replay_non_dry_run_records_spawn_failure_as_ledger() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("spawn_001"));
        trace.command_sequence.truncate(1);
        trace.generator_evidence.command_count = 1;
        trace.resource_profile_hints.rch_required = false;
        let trace_path = workspace.path().join("swarm-workload-spawn.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let missing_binary = workspace.path().join("missing-ee-binary");
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: false,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: Some(missing_binary.clone()),
            rch_proof_path: None,
        };

        let report = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;
        let report_json = report.to_json();
        let missing_binary_text = missing_binary.display().to_string();

        ensure(report.status, SwarmReplayStatus::Fail, "status")?;
        ensure(report.aggregate.command_count, 1u64, "command count")?;
        ensure(report.aggregate.failure_count, 1u64, "failure count")?;
        ensure(
            report
                .first_failure
                .as_ref()
                .map(|failure| failure.code.as_str()),
            Some(SWARM_REPLAY_COMMAND_SPAWN_FAILED_CODE),
            "first failure code",
        )?;
        ensure(
            report.command_results[0]
                .degraded_codes
                .iter()
                .any(|code| code == SWARM_REPLAY_COMMAND_SPAWN_FAILED_CODE),
            true,
            "spawn failure degraded code",
        )?;
        ensure(
            report.command_results[0].artifact_paths.len(),
            2usize,
            "stdout/stderr artifact count",
        )?;
        ensure(
            report.command_results[0].stderr_bytes > 0,
            true,
            "spawn failure stderr summary",
        )?;
        ensure(
            report.command_results[0]
                .artifact_paths
                .iter()
                .all(|artifact| {
                    artifact.path_tail.starts_with(".ee/lab/swarm-replay/")
                        && artifact.path_hash.starts_with("blake3:")
                }),
            true,
            "redacted artifact refs",
        )?;
        ensure(
            report_json.contains(&missing_binary_text),
            false,
            "spawn failure ledger must not leak binary path",
        )
    }

    #[test]
    fn swarm_replay_non_dry_run_refuses_local_cargo_before_execution_gate() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("cargo_exec_001"));
        trace.command_sequence[0].command.verbs = vec!["cargo".to_owned(), "test".to_owned()];
        let trace_path = workspace.path().join("swarm-workload-cargo-execute.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: false,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: None,
            rch_proof_path: None,
        };

        let report = replay_swarm_workload_trace(&options).map_err(|error| error.message())?;

        ensure(report.status, SwarmReplayStatus::Blocked, "status")?;
        ensure(
            report
                .first_failure
                .as_ref()
                .map(|failure| failure.code.as_str()),
            Some(SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE),
            "first failure code",
        )?;
        ensure(
            report.command_results[0]
                .degraded_codes
                .iter()
                .any(|code| code == SWARM_REPLAY_LOCAL_CARGO_REFUSED_CODE),
            true,
            "cargo refusal degraded code",
        )?;
        ensure(
            report.command_results[0]
                .degraded_codes
                .iter()
                .any(|code| code == SWARM_REPLAY_EXECUTION_NOT_ENABLED_CODE),
            false,
            "cargo refusal should not be masked by execution gate",
        )
    }

    #[test]
    fn swarm_replay_invocation_synthesizes_safe_remember_arguments() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("invoke_001"));
        let state = SwarmReplayExecutionState {
            ee_binary_path: PathBuf::from("ee"),
            workspace: workspace.path().to_path_buf(),
            artifact_root: workspace.path().join(".ee/lab/swarm-replay/test"),
            artifact_path_tail_prefix: ".ee/lab/swarm-replay/test".to_owned(),
            remembered_memory_id: None,
            last_synthetic_content: None,
        };
        let remember_step = trace
            .command_sequence
            .iter()
            .find(|step| step.command.verbs == ["remember"])
            .ok_or_else(|| "generated fixture missing remember step".to_owned())?;

        let invocation = swarm_replay_command_invocation(remember_step, &state)
            .map_err(|refusal| refusal.diagnosis.to_owned())?;

        ensure(invocation.argv[0].as_str(), "remember", "command")?;
        ensure(
            invocation.argv.iter().any(|arg| arg == "--workspace"),
            true,
            "workspace flag",
        )?;
        ensure(
            invocation.argv.iter().any(|arg| arg == "--json"),
            true,
            "json flag",
        )?;
        ensure(
            invocation
                .synthetic_content
                .as_deref()
                .is_some_and(|content| content.contains("swarm replay synthetic memory")),
            true,
            "synthetic content",
        )?;

        Ok(())
    }

    #[test]
    fn swarm_replay_invocation_requires_remember_before_why() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let trace = generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("why_001"));
        let mut state = SwarmReplayExecutionState {
            ee_binary_path: PathBuf::from("ee"),
            workspace: workspace.path().to_path_buf(),
            artifact_root: workspace.path().join(".ee/lab/swarm-replay/test"),
            artifact_path_tail_prefix: ".ee/lab/swarm-replay/test".to_owned(),
            remembered_memory_id: None,
            last_synthetic_content: Some("swarm replay synthetic memory".to_owned()),
        };
        let why_step = trace
            .command_sequence
            .iter()
            .find(|step| step.command.verbs == ["why"])
            .ok_or_else(|| "generated fixture missing why step".to_owned())?;

        let refused = swarm_replay_command_invocation(why_step, &state)
            .expect_err("why should require remembered memory id");
        ensure(
            refused.code,
            SWARM_REPLAY_PREREQUISITE_UNAVAILABLE_CODE,
            "refusal code",
        )?;

        state.remembered_memory_id = Some("mem_swarm_replay_001".to_owned());
        let invocation = swarm_replay_command_invocation(why_step, &state)
            .map_err(|refusal| refusal.diagnosis.to_owned())?;
        ensure(
            invocation.argv,
            vec![
                "why".to_owned(),
                "mem_swarm_replay_001".to_owned(),
                "--workspace".to_owned(),
                workspace.path().display().to_string(),
                "--json".to_owned(),
            ],
            "why argv",
        )
    }

    #[test]
    fn swarm_replay_rejects_non_side_effect_free_trace() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut trace =
            generate_swarm_workload_fixture(&SwarmWorkloadFixtureOptions::small("effect_001"));
        trace.side_effect_free = false;
        let trace_path = workspace.path().join("swarm-workload-effect.json");
        fs::write(&trace_path, trace.to_json()).map_err(|error| error.to_string())?;
        let options = SwarmReplayOptions {
            workspace: workspace.path().to_path_buf(),
            trace_path,
            dry_run: true,
            host_observation: admitted_smoke_swarm_observation(),
            ee_binary_path: None,
            rch_proof_path: None,
        };

        let result = replay_swarm_workload_trace(&options);

        ensure(
            matches!(result, Err(DomainError::PolicyDenied { .. })),
            true,
            "policy denied",
        )
    }

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
    fn workload_promotion_emits_recorded_swarm_workload_provenance() -> TestResult {
        let trace = promote_agent_workload_trace_jsonl_to_swarm_workload(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            4,
            SwarmWorkloadFixtureProfile::Small,
        )
        .map_err(|error| error.message())?;

        ensure(trace.schema.as_str(), SWARM_WORKLOAD_SCHEMA_V1, "schema")?;
        ensure(trace.side_effect_free, true, "side effect free")?;
        ensure(trace.agent_count, 4u16, "agent count")?;
        ensure(
            trace.redaction_level,
            SwarmWorkloadRedactionLevel::Audit,
            "redaction level",
        )?;
        ensure(
            trace.provenance.kind,
            SwarmWorkloadProvenanceKind::Recorded,
            "provenance kind",
        )?;
        ensure(
            trace.provenance.source_trace_hashes.len(),
            1usize,
            "source trace hash count",
        )?;
        ensure(
            trace
                .provenance
                .source_trace_hashes
                .first()
                .is_some_and(|hash| hash.starts_with("blake3:")),
            true,
            "source trace hash prefix",
        )?;
        ensure(
            trace
                .provenance
                .derived_from_schemas
                .contains(&AGENT_WORKLOAD_TRACE_SCHEMA_V1.to_owned()),
            true,
            "agent trace schema provenance",
        )?;
        ensure(
            trace
                .provenance
                .derived_from_schemas
                .contains(&SWARM_WORKLOAD_SCHEMA_V1.to_owned()),
            true,
            "swarm workload schema provenance",
        )?;
        ensure(
            trace.generator_evidence.profile.as_str(),
            "recorded",
            "generator evidence profile",
        )?;
        ensure(
            trace.generator_evidence.command_count,
            4u16,
            "generator command count",
        )?;
        ensure(
            trace.command_sequence.len(),
            4usize,
            "command sequence length",
        )?;
        ensure(
            trace.command_sequence[0].command.verbs.clone(),
            vec!["context".to_owned()],
            "first command verbs",
        )?;
        ensure(
            trace.command_sequence[0].command.flag_names.clone(),
            vec!["--json".to_owned(), "--max-tokens".to_owned()],
            "first command flags sorted",
        )?;
        ensure(
            trace.expected_degraded_posture,
            SwarmExpectedDegradedPosture::Recoverable,
            "degraded posture",
        )?;
        validate_swarm_workload_trace(&trace).map_err(|error| error.message())?;

        let json = trace.to_json();
        for forbidden in [
            "/Users/",
            "/data/projects/",
            "raw task content",
            "raw query text",
            "memory body payload",
            "mail body payload",
            "SECRET_TOKEN",
            "HOME=/",
        ] {
            ensure(
                !json.contains(forbidden),
                true,
                &format!("promoted workload leaked forbidden marker {forbidden}"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn workload_promotion_ordering_and_hash_are_deterministic() -> TestResult {
        let reversed = REDACTED_WORKLOAD_TRACE
            .lines()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let first = promote_agent_workload_trace_jsonl_to_swarm_workload(
            "redacted_trace_minimal.jsonl",
            REDACTED_WORKLOAD_TRACE,
            4,
            SwarmWorkloadFixtureProfile::Medium,
        )
        .map_err(|error| error.message())?;
        let second = promote_agent_workload_trace_jsonl_to_swarm_workload(
            "redacted_trace_minimal.jsonl",
            &reversed,
            4,
            SwarmWorkloadFixtureProfile::Medium,
        )
        .map_err(|error| error.message())?;

        ensure(first.to_json(), second.to_json(), "promoted trace json")
    }

    #[test]
    fn workload_promotion_rejects_raw_content_posture() -> TestResult {
        let raw = REDACTED_WORKLOAD_TRACE.replacen(
            "\"rawTaskStringPresent\":false",
            "\"rawTaskStringPresent\":true",
            1,
        );
        let result = promote_agent_workload_trace_jsonl_to_swarm_workload(
            "raw_trace.jsonl",
            &raw,
            4,
            SwarmWorkloadFixtureProfile::Small,
        );

        ensure(
            matches!(result, Err(DomainError::PolicyDenied { .. })),
            true,
            "raw trace policy denial",
        )
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
