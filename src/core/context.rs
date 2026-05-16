//! Capability-narrowed command context.
//!
//! Every command handler accepts a [`CommandContext`] that bundles
//! - the active [`WorkspaceLocation`] (produced by EE-023),
//! - the per-request [`RequestBudget`] (EE-010), and
//! - a [`CapabilitySet`] naming which subsystems the handler may touch
//!   and at what [`AccessLevel`].
//!
//! Narrowing is element-wise `min` against a mask, so capabilities can
//! only contract — never widen — as control flows from the CLI entry
//! point down into subsystems. The narrowing law (`narrow(a, mask) ≤ a`
//! on every axis, with `≤` ordered as `None < Read < Write`) is the
//! load-bearing invariant: a downstream handler that holds a `Read`
//! capability for `db` cannot accidentally execute a write because the
//! narrow operation never produces a higher level than the input.
//!
//! EE-011 (this bead) ships only the type and its math. The wiring
//! that constructs a `CommandContext` from CLI arguments + workspace
//! discovery + a default capability set per command lives in EE-005 /
//! EE-018. The mapping from a capability denial to a stable
//! `degraded[]` code (e.g. `policy_capability_denied`) belongs to
//! EE-006 / EE-016. Strict scope: this module must not depend on any
//! of those landing first.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
#[cfg(unix)]
use rustix::fs::{FlockOperation, flock};
#[cfg(unix)]
use rustix::io::Errno;

use crate::config::{
    ConfigFile, EnvVar, GRAPH_FEATURE_PACK_DNA_ENABLED_KEY, GRAPH_FEATURE_PPR_ENABLED_KEY,
    GRAPH_FEATURE_PROXIMITY_ENABLED_KEY, ReadPoolConfig, WorkspaceLocation, read_env_var,
};
use crate::core::budget::RequestBudget;
use crate::core::focus::{focus_state_hash, focus_state_path, read_active_focus_state};
use crate::core::memory_scope::{
    MemoryScopeContext, MeshDisplayProvenance, MeshQueryVisibility, mesh_query_visibility,
};
use crate::core::profile::{RuntimeProfileReport, runtime_profile_for_workspace};
use crate::core::search::{
    PERFORMANCE_EXPLAIN_SCHEMA_V1, ScoreSource, SearchDegradation, SearchError, SearchHit,
    SearchOptions, SearchReport, SearchStatus, elapsed_timing_json, performance_redaction_json,
    query_observation_json, run_search_with_read_connection_seeded, search_degraded_data_json,
};
use crate::db::read_pool::{PoolConfig, ReadConnectionPool, SnapshotPin};
use crate::db::{
    CreatePackItemInput, CreatePackOmissionInput, CreatePackRecordInput, DatabaseConfig,
    DbConnection, StoredAgentContextProfileForPack, StoredMemory,
};
use crate::models::degradation::{GRAPH_PPR_EMPTY_SEED_SET_CODE, GRAPH_PPR_SNAPSHOT_STALE_CODE};
use crate::models::{
    AGENT_CONTEXT_PROFILE_SCHEMA_V1, AGENT_PROFILE_BIAS_CAP, AGENT_PROFILE_COLD_START_OUTCOMES,
    AgentContextProfileCounts, MemoryId, MemoryScope, MemoryScopeStats, PackId, ProvenanceUri,
    RedactionLevel, TrustClass, UnitScore, WorkspaceId, posture_for_trust_class,
};
use crate::pack::{
    ConflictKind, ConflictRecommendedAction, ConsensusConflictReport, ContextPackProfile,
    ContextRequest, ContextRequestInput, ContextResponse, ContextResponseDegradation,
    ContextResponseSeverity, PackAssemblySlo, PackAssemblySloActuals, PackCandidate,
    PackCandidateInput, PackCoordinationSnapshot, PackItemLifecycle, PackProvenance,
    PackResourceProfile, PackScoreBreakdown, PackSection, PackTrustSignal,
    assemble_draft_with_profile_and_options_seeded, estimate_tokens_default,
    pack_item_provenance_json,
};
use crate::runtime::determinism::{Deterministic, Seed};

static PACK_HASH_LOG_RUN_INDEX: AtomicU64 = AtomicU64::new(0);
static PACK_SLOT_PROCESS_GATES: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
const PACK_SLOT_RETRY_AFTER_MS: u64 = 250;
#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
pub(crate) const PACK_L2_CACHE_KEY_SCHEMA_V1: &str = "ee.pack.l2_cache_key.v1";
pub const DEFAULT_CONTEXT_PPR_WEIGHT: f32 = 0.30;

#[derive(Debug)]
struct PackSlotGuard {
    path: PathBuf,
    _file: File,
}

impl Drop for PackSlotGuard {
    fn drop(&mut self) {
        release_pack_slot_process_gate(&self.path);
    }
}

#[derive(Debug)]
enum PackSlotAcquisition {
    Acquired(PackSlotGuard),
    LimitReached { retry_after_ms: u64 },
    Unavailable { path: PathBuf, message: String },
}

fn pack_slot_process_gates() -> &'static Mutex<BTreeSet<PathBuf>> {
    PACK_SLOT_PROCESS_GATES.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn try_acquire_pack_slot_process_gate(path: &Path) -> bool {
    let mut active_paths = pack_slot_process_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    active_paths.insert(path.to_path_buf())
}

fn release_pack_slot_process_gate(path: &Path) {
    let mut active_paths = pack_slot_process_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    active_paths.remove(path);
}

fn try_acquire_pack_slot(
    workspace_path: &Path,
    profile: PackResourceProfile,
) -> PackSlotAcquisition {
    let budget = profile.budget_class();
    let slots_dir = workspace_path.join(".ee").join("pack-slots");
    if let Err(message) = ensure_pack_slot_path_is_not_symlink(&slots_dir, "pack slot directory") {
        return PackSlotAcquisition::Unavailable {
            path: slots_dir,
            message,
        };
    }
    if let Err(error) = std::fs::create_dir_all(&slots_dir) {
        return PackSlotAcquisition::Unavailable {
            path: slots_dir,
            message: format!("Failed to create pack slot directory: {error}"),
        };
    }
    if let Err(message) = ensure_pack_slot_path_is_not_symlink(&slots_dir, "pack slot directory") {
        return PackSlotAcquisition::Unavailable {
            path: slots_dir,
            message,
        };
    }

    for slot_index in 0..budget.concurrent_pack_max {
        let slot_path = slots_dir.join(format!("{}-{slot_index:02}.lock", profile.as_str()));
        if let Err(message) = ensure_pack_slot_path_is_not_symlink(&slot_path, "pack slot lock") {
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message,
            };
        }
        if !try_acquire_pack_slot_process_gate(&slot_path) {
            continue;
        }
        if let Err(message) = ensure_pack_slot_path_is_not_symlink(&slot_path, "pack slot lock") {
            release_pack_slot_process_gate(&slot_path);
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message,
            };
        }

        let file = match OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&slot_path)
        {
            Ok(file) => file,
            Err(error) => {
                release_pack_slot_process_gate(&slot_path);
                return PackSlotAcquisition::Unavailable {
                    path: slot_path,
                    message: format!("Failed to open pack slot lock: {error}"),
                };
            }
        };

        #[cfg(unix)]
        if let Err(error) = flock(&file, FlockOperation::NonBlockingLockExclusive) {
            release_pack_slot_process_gate(&slot_path);
            if error == Errno::WOULDBLOCK || error == Errno::AGAIN {
                continue;
            }
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message: format!("Failed to acquire pack slot lock: {error}"),
            };
        }

        return PackSlotAcquisition::Acquired(PackSlotGuard {
            path: slot_path,
            _file: file,
        });
    }

    PackSlotAcquisition::LimitReached {
        retry_after_ms: PACK_SLOT_RETRY_AFTER_MS,
    }
}

fn ensure_pack_slot_path_is_not_symlink(path: &Path, path_type: &str) -> Result<(), String> {
    if let Some(symlink_path) = first_existing_pack_slot_symlink_component(path)? {
        return Err(format!(
            "Refusing to use {} '{}': path traverses symbolic link '{}'",
            path_type,
            path.display(),
            symlink_path.display()
        ));
    }
    Ok(())
}

fn first_existing_pack_slot_symlink_component(path: &Path) -> Result<Option<PathBuf>, String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
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
                return Err(format!(
                    "Failed to inspect pack slot path component '{}': {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(None)
}

/// Per-subsystem permission level. `None < Read < Write` under the
/// derived `Ord`, which is what the narrowing law relies on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum AccessLevel {
    /// The handler may not touch the subsystem at all.
    #[default]
    None = 0,
    /// The handler may observe state without mutating it.
    Read = 1,
    /// The handler may mutate the subsystem.
    Write = 2,
}

impl AccessLevel {
    /// Stable string representation suitable for log fields and future
    /// JSON renderers.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    /// `true` if at least `Read`.
    #[must_use]
    pub const fn allows_read(self) -> bool {
        matches!(self, Self::Read | Self::Write)
    }

    /// `true` if `Write`.
    #[must_use]
    pub const fn allows_write(self) -> bool {
        matches!(self, Self::Write)
    }

    /// Element-wise lattice meet (`min`) usable in `const` context.
    /// `Ord` derive would cover this for non-`const` callers, but
    /// narrowing math runs inside `const fn`s where `Ord::min` is not
    /// yet stable.
    #[must_use]
    pub const fn min_const(a: Self, b: Self) -> Self {
        if (a as u8) <= (b as u8) { a } else { b }
    }
}

/// Per-subsystem permission map. Each slot is independent; narrowing
/// a single dimension does not affect the others.
///
/// Adding a new subsystem here is a deliberate edit: every consumer
/// pattern-matches on the named slots, and the schema-drift gate
/// (EE-SCHEMA-DRIFT-001) will eventually pin the variant order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CapabilitySet {
    /// FrankenSQLite source-of-truth database access.
    pub db: AccessLevel,
    /// Frankensearch / FTS5 lexical and vector indexes.
    pub search_index: AccessLevel,
    /// FrankenNetworkX graph snapshot artefacts.
    pub graph_snapshot: AccessLevel,
    /// `cass` subprocess invocation rights.
    pub cass_subprocess: AccessLevel,
    /// Workspace filesystem access beyond the database file.
    pub filesystem: AccessLevel,
    /// Outbound network access (off by default; only adapters may
    /// hold any non-`None` value here).
    pub network: AccessLevel,
    /// Append-only audit log writes. Reads are gated by `db`.
    pub audit_log: AccessLevel,
}

impl CapabilitySet {
    /// All subsystems set to [`AccessLevel::None`]. Useful as a
    /// starting point when explicitly opting in to capabilities.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            db: AccessLevel::None,
            search_index: AccessLevel::None,
            graph_snapshot: AccessLevel::None,
            cass_subprocess: AccessLevel::None,
            filesystem: AccessLevel::None,
            network: AccessLevel::None,
            audit_log: AccessLevel::None,
        }
    }

    /// All subsystems set to [`AccessLevel::Read`]. Suitable as the
    /// starting capability set for read-only commands such as
    /// `ee status`, `ee search`, `ee why`, `ee context`.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            db: AccessLevel::Read,
            search_index: AccessLevel::Read,
            graph_snapshot: AccessLevel::Read,
            cass_subprocess: AccessLevel::Read,
            filesystem: AccessLevel::Read,
            network: AccessLevel::None,
            audit_log: AccessLevel::Read,
        }
    }

    /// Every subsystem set to [`AccessLevel::Write`] except `network`,
    /// which stays `None` because v1 is local-first and outbound
    /// network is opt-in per adapter (see README §Local First).
    #[must_use]
    pub const fn full_local() -> Self {
        Self {
            db: AccessLevel::Write,
            search_index: AccessLevel::Write,
            graph_snapshot: AccessLevel::Write,
            cass_subprocess: AccessLevel::Write,
            filesystem: AccessLevel::Write,
            network: AccessLevel::None,
            audit_log: AccessLevel::Write,
        }
    }

    /// Element-wise narrow against `mask`. Each slot becomes
    /// `min(self.slot, mask.slot)`.
    ///
    /// The narrowing law: for every slot `s`,
    /// `self.narrow(mask).s ≤ self.s` and
    /// `self.narrow(mask).s ≤ mask.s`. Repeated narrowing therefore
    /// never widens.
    #[must_use]
    pub const fn narrow(self, mask: Self) -> Self {
        Self {
            db: AccessLevel::min_const(self.db, mask.db),
            search_index: AccessLevel::min_const(self.search_index, mask.search_index),
            graph_snapshot: AccessLevel::min_const(self.graph_snapshot, mask.graph_snapshot),
            cass_subprocess: AccessLevel::min_const(self.cass_subprocess, mask.cass_subprocess),
            filesystem: AccessLevel::min_const(self.filesystem, mask.filesystem),
            network: AccessLevel::min_const(self.network, mask.network),
            audit_log: AccessLevel::min_const(self.audit_log, mask.audit_log),
        }
    }
}

/// Bundle threaded through every command handler.
///
/// Ownership is `Clone` rather than `Copy` because [`WorkspaceLocation`]
/// owns `PathBuf`s. Cloning is cheap relative to a command's actual work
/// and keeps narrowing free of borrow gymnastics.
#[derive(Clone, Debug)]
pub struct CommandContext {
    workspace: WorkspaceLocation,
    budget: RequestBudget,
    capabilities: CapabilitySet,
}

impl CommandContext {
    /// Build a new context. The CLI entry point constructs one of
    /// these from the resolved workspace, the parsed CLI flags, and
    /// the per-command capability default.
    #[must_use]
    pub const fn new(
        workspace: WorkspaceLocation,
        budget: RequestBudget,
        capabilities: CapabilitySet,
    ) -> Self {
        Self {
            workspace,
            budget,
            capabilities,
        }
    }

    /// The active workspace location.
    #[must_use]
    pub const fn workspace(&self) -> &WorkspaceLocation {
        &self.workspace
    }

    /// Convenience accessor for the workspace root directory.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        self.workspace.root.as_path()
    }

    /// The per-request budget. Read-only access for handlers that
    /// only need to consult deadlines; mutating access goes through
    /// [`Self::budget_mut`].
    #[must_use]
    pub const fn budget(&self) -> &RequestBudget {
        &self.budget
    }

    /// Mutable access to the per-request budget so handlers can
    /// record consumption (`record_tokens`, `record_io_bytes`, etc.).
    #[must_use]
    pub const fn budget_mut(&mut self) -> &mut RequestBudget {
        &mut self.budget
    }

    /// The current capability set.
    #[must_use]
    pub const fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    /// Return a clone whose capability set is the element-wise `min`
    /// of `self.capabilities` and `mask`. Workspace and budget pass
    /// through unchanged so cancellation / deadline state is
    /// preserved across narrowing.
    #[must_use]
    pub fn with_narrowed_capabilities(&self, mask: CapabilitySet) -> Self {
        Self {
            workspace: self.workspace.clone(),
            budget: self.budget,
            capabilities: self.capabilities.narrow(mask),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContextPackOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub query: String,
    pub speed: crate::search::SpeedMode,
    pub filters: crate::models::QueryFilters,
    pub profile: Option<ContextPackProfile>,
    pub max_tokens: Option<u32>,
    pub candidate_pool: Option<u32>,
    pub max_results: Option<u32>,
    pub include_tombstoned: bool,
    pub as_of: Option<DateTime<Utc>>,
    pub include_expired: bool,
    pub include_future: bool,
    pub include_stale: bool,
    pub redaction_level: crate::models::RedactionLevel,
    pub memory_scope: MemoryScope,
    pub strict_scope: bool,
    pub ppr_weight: Option<f32>,
    pub pagination: Option<ContextPagination>,
    pub coordination_snapshot_path: Option<PathBuf>,
    pub coordination_stale_after_ms: u64,
    pub output_options: ContextPackOutputOptions,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextPackOutputProfile {
    Lean,
    #[default]
    Standard,
    Verbose,
}

impl ContextPackOutputProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lean => "lean",
            Self::Standard => "standard",
            Self::Verbose => "verbose",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextPackOutputOptions {
    pub profile: ContextPackOutputProfile,
    pub resource_profile: PackResourceProfile,
    pub include_coverage_fill: bool,
    pub include_rendered_text: bool,
    pub include_skipped: bool,
    pub include_meta: bool,
    pub include_verbose_meta: bool,
    /// Bead bd-17c65.5.2 (E2): when `false` (the default), per-response
    /// `degraded[]` filters out signals whose [`crate::pack::DegradedCategory`]
    /// classifies them as build-time feature gaps or workspace-state
    /// conditions that did not affect this particular response. When
    /// `true` (via `--include-non-affecting-degradations`), every
    /// signal surfaces — the pre-E2 verbose behavior. Defaults differ
    /// per profile only in the Verbose profile (true), to match the
    /// existing "verbose surfaces everything" convention.
    pub include_non_affecting_degradations: bool,
}

impl Default for ContextPackOutputOptions {
    fn default() -> Self {
        Self::for_profile(ContextPackOutputProfile::Standard)
    }
}

impl ContextPackOutputOptions {
    #[must_use]
    pub const fn for_profile(profile: ContextPackOutputProfile) -> Self {
        match profile {
            ContextPackOutputProfile::Lean => Self {
                profile,
                resource_profile: PackResourceProfile::Standard,
                include_coverage_fill: false,
                include_rendered_text: false,
                include_skipped: false,
                include_meta: true,
                include_verbose_meta: false,
                include_non_affecting_degradations: false,
            },
            ContextPackOutputProfile::Standard => Self {
                profile,
                resource_profile: PackResourceProfile::Standard,
                include_coverage_fill: true,
                include_rendered_text: true,
                include_skipped: true,
                include_meta: true,
                include_verbose_meta: false,
                include_non_affecting_degradations: false,
            },
            ContextPackOutputProfile::Verbose => Self {
                profile,
                resource_profile: PackResourceProfile::Standard,
                include_coverage_fill: true,
                include_rendered_text: true,
                include_skipped: true,
                include_meta: true,
                include_verbose_meta: true,
                include_non_affecting_degradations: true,
            },
        }
    }

    #[must_use]
    pub fn with_overrides(self, overrides: ContextPackOutputOptionOverrides) -> Self {
        Self {
            profile: self.profile,
            resource_profile: self.resource_profile,
            include_coverage_fill: overrides
                .no_coverage_fill
                .map_or(self.include_coverage_fill, |value| !value),
            include_rendered_text: overrides
                .no_rendered_text
                .map_or(self.include_rendered_text, |value| !value),
            include_skipped: overrides
                .no_skipped
                .map_or(self.include_skipped, |value| !value),
            include_meta: overrides.no_meta.map_or(self.include_meta, |value| !value),
            include_verbose_meta: self.include_verbose_meta,
            include_non_affecting_degradations: overrides
                .include_non_affecting_degradations
                .unwrap_or(self.include_non_affecting_degradations),
        }
    }

    #[must_use]
    pub const fn with_resource_profile(mut self, resource_profile: PackResourceProfile) -> Self {
        self.resource_profile = resource_profile;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContextPackOutputOptionOverrides {
    pub no_coverage_fill: Option<bool>,
    pub no_rendered_text: Option<bool>,
    pub no_skipped: Option<bool>,
    pub no_meta: Option<bool>,
    /// Bead bd-17c65.5.2 (E2): when `Some(true)`, surface every
    /// degraded signal regardless of category (the
    /// `--include-non-affecting-degradations` CLI flag).
    pub include_non_affecting_degradations: Option<bool>,
}

/// Pagination state for context pack execution.
#[derive(Clone, Debug, Default)]
pub struct ContextPagination {
    /// Page size limit.
    pub limit: u32,
    /// Offset from decoded cursor (0 for first page).
    pub offset: u32,
    /// Query shape hash for cursor validation.
    pub query_hash: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextPackPerformanceRun {
    pub response: ContextResponse,
    pub performance: serde_json::Value,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ContextPerformanceTrace {
    db_open_count: usize,
    index_status_checks: usize,
    pack_record_writes: usize,
    filter_input_count: usize,
    filtered_count: usize,
    focus_state_read_attempts: usize,
    focus_state_hits: usize,
    focus_candidate_count: usize,
    candidate_resolution: CandidateResolutionMetrics,
    timings: Vec<PerformanceTiming>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CandidateResolutionMetrics {
    search_hits: usize,
    artifact_link_lookups: usize,
    resolved_memory_ids: usize,
    unique_memory_ids: usize,
    memory_batch_reads: usize,
    tag_batch_reads: usize,
    tag_filtered_candidates: usize,
    trust_filtered_candidates: usize,
    redaction_filtered_candidates: usize,
    scope_filtered_candidates: usize,
    temporal_filtered_candidates: usize,
    temporal_relaxed_candidates: usize,
    graph_boosted_candidates: usize,
    graph_expanded_candidates: usize,
    graph_filtered_candidates: usize,
    graph_missing_seeds: usize,
    graph_traversed_edges: usize,
    converted_candidates: usize,
    skipped_candidates: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PerformanceTiming {
    name: &'static str,
    elapsed: std::time::Duration,
}

impl ContextPerformanceTrace {
    fn record_elapsed(&mut self, name: &'static str, start: Instant) {
        self.timings.push(PerformanceTiming {
            name,
            elapsed: start.elapsed(),
        });
    }

    fn elapsed_ms(&self, name: &str) -> u64 {
        self.timings
            .iter()
            .find(|timing| timing.name == name)
            .map_or(0, |timing| {
                u64::try_from(timing.elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

#[derive(Debug)]
pub enum ContextPackError {
    Storage(String),
    Search(SearchError),
    Pack(String),
    PolicyDenied(String),
}

impl ContextPackError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Storage(_) => Some("ee init --workspace ."),
            Self::Search(error) => error.repair_hint(),
            Self::Pack(_) => Some("ee context --help"),
            Self::PolicyDenied(_) => None,
        }
    }

    #[must_use]
    pub const fn is_policy_denied(&self) -> bool {
        matches!(self, Self::PolicyDenied(_))
    }
}

impl std::fmt::Display for ContextPackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(message) | Self::Pack(message) | Self::PolicyDenied(message) => {
                formatter.write_str(message)
            }
            Self::Search(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for ContextPackError {}

pub fn run_context_pack(options: &ContextPackOptions) -> Result<ContextResponse, ContextPackError> {
    run_context_pack_with_performance(options, "context").map(|run| run.response)
}

pub fn run_context_pack_seeded(
    options: &ContextPackOptions,
    determinism: &Deterministic<Seed>,
) -> Result<ContextResponse, ContextPackError> {
    run_context_pack_with_performance_seeded(options, "context", determinism)
        .map(|run| run.response)
}

pub fn attach_pack_dna_to_context_response(database_path: &Path, response: &mut ContextResponse) {
    let workspace_path = workspace_path_from_database_path(database_path);
    match workspace_path
        .as_deref()
        .map(context_pack_dna_feature_enabled)
        .unwrap_or(Ok(false))
    {
        Ok(true) => {}
        Ok(false) => {
            response.data.pack_dna = Some(serde_json::Value::Null);
            push_pack_dna_feature_disabled_degradation(&mut response.data.degraded);
            return;
        }
        Err(message) => {
            response.data.pack_dna = Some(serde_json::Value::Null);
            push_degradation(
                &mut response.data.degraded,
                "context_config_unavailable",
                ContextResponseSeverity::Medium,
                message,
                Some("Fix or remove .ee/config.toml.".to_string()),
            );
            return;
        }
    }

    let connection = match DbConnection::open_file(database_path) {
        Ok(connection) => connection,
        Err(error) => {
            response.data.pack_dna = Some(serde_json::Value::Null);
            push_degradation(
                &mut response.data.degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!("Pack DNA was requested but the memory graph could not be opened: {error}"),
                Some("ee status --json".to_string()),
            );
            return;
        }
    };

    let projection = match crate::graph::build_memory_graph(
        &connection,
        &crate::graph::ProjectionOptions::default(),
    ) {
        Ok(projection) => projection,
        Err(error) => {
            response.data.pack_dna = Some(serde_json::Value::Null);
            push_degradation(
                &mut response.data.degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!("Pack DNA was requested but memory graph projection failed: {error}"),
                Some("ee graph centrality-refresh --workspace .".to_string()),
            );
            return;
        }
    };

    let pack_memory_ids = response
        .data
        .pack
        .items
        .iter()
        .map(|item| item.memory_id)
        .collect::<Vec<_>>();
    let query_seed_weights = response
        .data
        .pack
        .items
        .iter()
        .filter_map(|item| {
            let score = item.relevance.into_inner();
            (score.is_finite() && score > 0.0).then_some((item.memory_id, f64::from(score)))
        })
        .collect::<BTreeMap<_, _>>();
    let trust_anchor_memory_ids = response
        .data
        .pack
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.trust.class,
                TrustClass::HumanExplicit | TrustClass::AgentValidated
            )
        })
        .map(|item| item.memory_id)
        .collect::<Vec<_>>();

    let input = crate::graph::pack_dna::PackDnaInput {
        pack_memory_ids,
        query_seed_weights,
        trust_anchor_memory_ids,
        ego_radius: crate::graph::pack_dna::DEFAULT_PACK_DNA_EGO_RADIUS,
        ppr_neighbor_limit: crate::graph::pack_dna::DEFAULT_PACK_DNA_PPR_NEIGHBOR_LIMIT,
    };
    let pack_dna = match crate::graph::pack_dna::compute_pack_dna(&projection, &input) {
        Ok(pack_dna) => pack_dna,
        Err(error) => {
            response.data.pack_dna = Some(serde_json::Value::Null);
            push_degradation(
                &mut response.data.degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!("Pack DNA computation failed: {error}"),
                Some("ee graph centrality-refresh --workspace .".to_string()),
            );
            return;
        }
    };

    for degradation in &pack_dna.degraded {
        push_degradation(
            &mut response.data.degraded,
            &degradation.code,
            context_severity_from_pack_dna(&degradation.severity),
            degradation.message.clone(),
            Some(degradation.repair.clone()),
        );
    }

    response.data.pack_dna =
        Some(serde_json::to_value(&pack_dna).unwrap_or(serde_json::Value::Null));
}

fn workspace_path_from_database_path(database_path: &Path) -> Option<PathBuf> {
    let ee_dir = database_path.parent()?;
    (ee_dir.file_name()? == ".ee").then(|| ee_dir.parent().map(Path::to_path_buf))?
}

fn context_pack_dna_feature_enabled(workspace_path: &Path) -> Result<bool, String> {
    let config = context_workspace_config(workspace_path, "Pack DNA")?;
    Ok(config
        .and_then(|config| config.graph.feature.pack_dna_enabled)
        .unwrap_or(false))
}

fn push_pack_dna_feature_disabled_degradation(degraded: &mut Vec<ContextResponseDegradation>) {
    push_degradation(
        degraded,
        "graph_feature_disabled",
        ContextResponseSeverity::Medium,
        format!("Pack DNA is disabled by {GRAPH_FEATURE_PACK_DNA_ENABLED_KEY}."),
        Some(format!(
            "ee config set {GRAPH_FEATURE_PACK_DNA_ENABLED_KEY} true"
        )),
    );
}

const fn context_severity_from_pack_dna(severity: &str) -> ContextResponseSeverity {
    match severity.as_bytes() {
        b"info" => ContextResponseSeverity::Info,
        b"medium" => ContextResponseSeverity::Medium,
        b"high" => ContextResponseSeverity::High,
        _ => ContextResponseSeverity::Low,
    }
}

pub fn run_context_pack_with_performance(
    options: &ContextPackOptions,
    command: &'static str,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    let determinism = Deterministic::from_seed(0);
    run_context_pack_with_performance_inner(
        options,
        command,
        &determinism,
        PackRecordPersistence::Ambient,
    )
}

pub fn run_context_pack_with_performance_seeded(
    options: &ContextPackOptions,
    command: &'static str,
    determinism: &Deterministic<Seed>,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    run_context_pack_with_performance_inner(
        options,
        command,
        determinism,
        PackRecordPersistence::Seeded(determinism),
    )
}

#[derive(Clone, Copy)]
enum PackRecordPersistence<'a> {
    Ambient,
    Seeded(&'a Deterministic<Seed>),
}

fn run_context_pack_with_performance_inner(
    options: &ContextPackOptions,
    command: &'static str,
    determinism: &Deterministic<Seed>,
    pack_record_persistence: PackRecordPersistence<'_>,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    let total_start = Instant::now();
    let mut trace = ContextPerformanceTrace::default();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);

    let request_start = Instant::now();
    let mut request = ContextRequest::new(ContextRequestInput {
        query: options.query.clone(),
        profile: options.profile,
        max_tokens: options.max_tokens,
        candidate_pool: options.candidate_pool,
        max_results: options.max_results,
        sections: Vec::new(),
    })
    .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    let (effective_max_tokens, tokens_capped) =
        runtime_profile.cap_pack_max_tokens(request.budget.max_tokens());
    let (effective_candidate_pool, candidate_pool_capped) =
        runtime_profile.cap_pack_candidate_pool(request.candidate_pool);
    if tokens_capped || candidate_pool_capped {
        request = ContextRequest::new(ContextRequestInput {
            query: request.query.clone(),
            profile: Some(request.profile),
            max_tokens: Some(effective_max_tokens),
            candidate_pool: Some(effective_candidate_pool),
            max_results: request.max_results,
            sections: Vec::new(),
        })
        .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    }
    trace.record_elapsed("requestValidate", request_start);

    let mut effective_filters = options.filters.clone();
    if effective_filters.temporal.as_of.is_none() {
        effective_filters.temporal.as_of = options.as_of;
    }

    if effective_filters.redaction.requests_bypass() {
        return Err(ContextPackError::PolicyDenied(
            "Redaction bypass requires elevated permission. The 'bypass' policy is not yet \
             supported; use 'respect' (default) to apply redaction filtering."
                .to_string(),
        ));
    }

    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    if !database_path.exists() {
        return Err(ContextPackError::Storage(format!(
            "Database not found at {}",
            database_path.display()
        )));
    }

    let mut degraded = Vec::new();

    let (read_pool_config, pin_snapshot) =
        context_read_pool_config(&options.workspace_path, &mut degraded);
    let snapshot_open_start = Instant::now();
    let read_pool = ReadConnectionPool::new(
        DatabaseConfig::file(database_path.clone()),
        read_pool_config,
    );
    let read_snapshot = read_pool
        .acquire_snapshot(pin_snapshot)
        .map_err(|error| ContextPackError::Storage(format!("Failed to open database: {error}")))?;
    trace.db_open_count = trace.db_open_count.saturating_add(1);
    trace.record_elapsed("dbOpen", snapshot_open_start);

    let output_redaction_enabled =
        crate::config::workspace_output_redaction_enabled(&options.workspace_path);
    if !output_redaction_enabled {
        push_degradation(
            &mut degraded,
            "output_redaction_disabled",
            ContextResponseSeverity::Info,
            "Output-time redaction is disabled by workspace policy; context content may include secret-like values.",
            Some("Set policy.output_redaction.enabled = true in .ee/config.toml.".to_string()),
        );
    }
    if tokens_capped || candidate_pool_capped {
        push_degradation(
            &mut degraded,
            "context_profile_budget_capped",
            ContextResponseSeverity::Low,
            format!(
                "Context request budget was capped by the active {} operating profile.",
                runtime_profile.active_profile.as_str()
            ),
            Some("ee profile config plan --json".to_string()),
        );
    }

    let search_start = Instant::now();
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let mut search_report = match run_search_with_read_connection_seeded(
        &SearchOptions {
            workspace_path: options.workspace_path.clone(),
            database_path: Some(database_path.clone()),
            index_dir: options.index_dir.clone(),
            query: request.query.clone(),
            limit: request.candidate_pool,
            speed: options.speed,
            explain: false,
            as_of: context_validity_reference_time(options, &effective_filters),
            include_tombstoned: options.include_tombstoned,
            include_expired: context_include_expired(options, &effective_filters),
            include_future: context_include_future(options, &effective_filters),
            include_stale: context_include_stale(options, &effective_filters),
            // Context packing owns relevance and budget filtering after retrieval.
            // Keep the candidate pool broad so an exact single-memory match is not
            // dropped by the interactive search command's presentation floor.
            relevance_floor: Some(0.0),
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: options.memory_scope,
            strict_scope: options.strict_scope,
        },
        read_connection,
        determinism,
    ) {
        Ok(report) => report,
        Err(SearchError::NoIndex) => missing_index_search_report(
            &request.query,
            request.candidate_pool,
            runtime_profile.clone(),
        ),
        Err(error) => return Err(ContextPackError::Search(error)),
    };
    trace.index_status_checks = trace.index_status_checks.saturating_add(1);
    trace.record_elapsed("search", search_start);

    push_search_degradations(&mut degraded, &search_report.degraded);
    if matches!(
        search_report.status,
        SearchStatus::IndexError | SearchStatus::IndexNotFound
    ) {
        let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
        let fallback_hits = lexical_memory_fallback_hits(
            read_connection,
            &options.workspace_path,
            &request.query,
            request.candidate_pool,
            options.include_tombstoned,
            context_validity_reference_time(options, &effective_filters),
            context_include_expired(options, &effective_filters),
            context_include_future(options, &effective_filters),
            context_include_stale(options, &effective_filters),
            &mut degraded,
        );
        let fallback_count = fallback_hits.len();
        push_degradation(
            &mut degraded,
            "context_lexical_fallback",
            ContextResponseSeverity::Medium,
            format!(
                "Search index could not satisfy the context request; assembled context from {fallback_count} deterministic lexical memory match{}.",
                plural_suffix(fallback_count)
            ),
            Some("ee index rebuild --workspace .".to_string()),
        );
        search_report.results = fallback_hits;
        search_report.status = if search_report.results.is_empty() {
            SearchStatus::NoResults
        } else {
            SearchStatus::Success
        };
    }

    // Apply metadata query filters to search results. Tag filters are applied
    // after memory tags have been batch-loaded during candidate resolution.
    if !effective_filters.filters.is_empty() {
        let pre_filter_count = search_report.results.len();
        trace.filter_input_count = pre_filter_count;
        search_report
            .results
            .retain(|hit| effective_filters.matches(hit.metadata.as_ref()));
        let filtered_count = pre_filter_count - search_report.results.len();
        trace.filtered_count = filtered_count;
        if filtered_count > 0 {
            push_degradation(
                &mut degraded,
                "context_filtered_results",
                ContextResponseSeverity::Low,
                format!(
                    "{} of {} search results excluded by query filters.",
                    filtered_count, pre_filter_count
                ),
                None,
            );
        }
    }
    if search_report.status == SearchStatus::NoResults {
        push_degradation(
            &mut degraded,
            "context_no_results",
            ContextResponseSeverity::Low,
            "Search completed but returned no candidate memories.",
            Some("ee remember --workspace . --level procedural --kind rule \"...\"".to_string()),
        );
    }

    let candidate_start = Instant::now();
    let candidate_filter_input_count = search_report.results.len();
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let (mut candidates, mut candidate_metrics) = candidates_from_search_with_metrics(
        read_connection,
        &options.workspace_path,
        &search_report,
        &effective_filters,
        options.include_tombstoned,
        &mut degraded,
    );
    if candidate_metrics.tag_filtered_candidates > 0 {
        trace.filter_input_count = trace.filter_input_count.max(candidate_filter_input_count);
        trace.filtered_count = trace
            .filtered_count
            .saturating_add(candidate_metrics.tag_filtered_candidates);
        push_degradation(
            &mut degraded,
            "context_filtered_results",
            ContextResponseSeverity::Low,
            format!(
                "{} candidate memor{} excluded by query filters.",
                candidate_metrics.tag_filtered_candidates,
                if candidate_metrics.tag_filtered_candidates == 1 {
                    "y was"
                } else {
                    "ies were"
                }
            ),
            None,
        );
    }
    if candidate_metrics.temporal_filtered_candidates > 0 {
        trace.filter_input_count = trace.filter_input_count.max(candidate_filter_input_count);
        trace.filtered_count = trace
            .filtered_count
            .saturating_add(candidate_metrics.temporal_filtered_candidates);
        push_degradation(
            &mut degraded,
            "context_temporal_filtered_results",
            ContextResponseSeverity::Low,
            format!(
                "{} candidate memor{} excluded by temporal query filters.",
                candidate_metrics.temporal_filtered_candidates,
                if candidate_metrics.temporal_filtered_candidates == 1 {
                    "y was"
                } else {
                    "ies were"
                }
            ),
            None,
        );
    }
    if candidate_metrics.temporal_relaxed_candidates > 0 {
        push_degradation(
            &mut degraded,
            "context_temporal_validity_relaxed",
            ContextResponseSeverity::Low,
            format!(
                "{} temporally invalid candidate memor{} kept because temporalValidity.posture=relaxed.",
                candidate_metrics.temporal_relaxed_candidates,
                if candidate_metrics.temporal_relaxed_candidates == 1 {
                    "y was"
                } else {
                    "ies were"
                }
            ),
            Some(
                "Use temporalValidity.posture=strict to exclude expired or not-yet-valid memories."
                    .to_string(),
            ),
        );
    }
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let graph_metrics = apply_graph_hints(
        read_connection,
        &options.workspace_path,
        &effective_filters,
        options.include_tombstoned,
        &mut candidates,
        &mut degraded,
    );
    candidate_metrics.graph_boosted_candidates = graph_metrics.boosted_candidates;
    candidate_metrics.graph_expanded_candidates = graph_metrics.expanded_candidates;
    candidate_metrics.graph_filtered_candidates = graph_metrics.filtered_candidates;
    candidate_metrics.graph_missing_seeds = graph_metrics.missing_seeds;
    candidate_metrics.graph_traversed_edges = graph_metrics.traversed_edges;
    trace.record_elapsed("candidateResolution", candidate_start);

    let focus_start = Instant::now();
    trace.focus_state_read_attempts = trace.focus_state_read_attempts.saturating_add(1);
    match read_active_focus_state(&options.workspace_path) {
        Ok(Some(focus_state)) => {
            trace.focus_state_hits = trace.focus_state_hits.saturating_add(1);
            let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
            let focus_candidates = focus_candidates_from_state(
                read_connection,
                &options.workspace_path,
                &focus_state,
                options.include_tombstoned,
                &mut degraded,
            );
            trace.focus_candidate_count = focus_candidates.len();
            candidates.extend(focus_candidates);
        }
        Ok(None) => {}
        Err(error) => push_degradation(
            &mut degraded,
            "context_focus_state_unavailable",
            ContextResponseSeverity::Low,
            format!("Passive focus state could not be read: {}", error.message()),
            Some("ee focus show --json".to_string()),
        ),
    }
    trace.record_elapsed("focusState", focus_start);

    let scope_filter_input_count =
        candidate_filter_input_count.saturating_add(trace.focus_candidate_count);
    let scope_context = MemoryScopeContext::for_workspace(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
    );
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let scope_stats = filter_candidates_by_memory_scope(
        read_connection,
        &mut candidates,
        &scope_context,
        &mut degraded,
    );
    if scope_stats.candidates_excluded_by_scope > 0 {
        candidate_metrics.scope_filtered_candidates = candidate_metrics
            .scope_filtered_candidates
            .saturating_add(scope_stats.candidates_excluded_by_scope);
        trace.filter_input_count = trace.filter_input_count.max(scope_filter_input_count);
        trace.filtered_count = trace
            .filtered_count
            .saturating_add(scope_stats.candidates_excluded_by_scope);
    }

    let redaction_filter_input_count =
        candidate_filter_input_count.saturating_add(trace.focus_candidate_count);
    let redaction_filtered_candidates = filter_candidates_by_redaction_allow_categories(
        &mut candidates,
        &effective_filters.redaction,
    );
    if redaction_filtered_candidates > 0 {
        candidate_metrics.redaction_filtered_candidates = candidate_metrics
            .redaction_filtered_candidates
            .saturating_add(redaction_filtered_candidates);
    }
    if candidate_metrics.redaction_filtered_candidates > 0 {
        trace.filter_input_count = trace.filter_input_count.max(redaction_filter_input_count);
        trace.filtered_count = trace
            .filtered_count
            .saturating_add(candidate_metrics.redaction_filtered_candidates);
        push_degradation(
            &mut degraded,
            "context_redaction_filtered_results",
            ContextResponseSeverity::Low,
            format!(
                "{} candidate memor{} excluded by redaction.allowCategories.",
                candidate_metrics.redaction_filtered_candidates,
                if candidate_metrics.redaction_filtered_candidates == 1 {
                    "y was"
                } else {
                    "ies were"
                }
            ),
            Some(
                "Add the emitted redaction reason to redaction.allowCategories or omit the allow-list."
                    .to_string(),
            ),
        );
    }

    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let ppr_metrics = apply_personalized_pagerank_rerank(
        read_connection,
        &options.workspace_path,
        &search_report,
        &mut candidates,
        effective_context_ppr_weight(options.ppr_weight),
        &mut degraded,
    );
    candidate_metrics.graph_boosted_candidates = candidate_metrics
        .graph_boosted_candidates
        .saturating_add(ppr_metrics.reranked_candidates);
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let proximity_metrics = apply_proximity_to_seed_scores(
        read_connection,
        &options.workspace_path,
        &search_report,
        &mut candidates,
        &mut degraded,
    );
    candidate_metrics.graph_boosted_candidates = candidate_metrics
        .graph_boosted_candidates
        .saturating_add(proximity_metrics.annotated_candidates);
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let mut agent_profile =
        apply_agent_context_profile_bias(read_connection, &options.workspace_path, &mut candidates);
    trace.candidate_resolution = candidate_metrics;

    sort_context_candidates(&mut candidates);

    if let Some(max_results) = request.max_results {
        let max_results = max_results as usize;
        if candidates.len() > max_results {
            let trimmed = candidates.len().saturating_sub(max_results);
            candidates.truncate(max_results);
            let noun = if trimmed == 1 {
                "candidate"
            } else {
                "candidates"
            };
            push_degradation(
                &mut degraded,
                "context_query_max_results_applied",
                ContextResponseSeverity::Low,
                format!("{trimmed} context {noun} excluded by query-file budget.maxResults."),
                Some(
                    "Increase budget.maxResults or budget.candidatePool in the query file."
                        .to_string(),
                ),
            );
        }
    }

    let _pagination_info = apply_pagination(&mut candidates, &options.pagination, &mut degraded);

    let pack_slot_acquisition = try_acquire_pack_slot(
        &options.workspace_path,
        options.output_options.resource_profile,
    );
    let (pack_slot_guard, concurrent_limit_retry_after_ms) = match pack_slot_acquisition {
        PackSlotAcquisition::Acquired(guard) => (Some(guard), None),
        PackSlotAcquisition::LimitReached { retry_after_ms } => (None, Some(retry_after_ms)),
        PackSlotAcquisition::Unavailable { path, message } => {
            push_degradation(
                &mut degraded,
                "pack_slot_lock_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Pack slot governance could not acquire a lock at {}: {message}",
                    path.display()
                ),
                Some("Check .ee/pack-slots permissions, then retry.".to_string()),
            );
            (None, None)
        }
    };

    let pack_start = Instant::now();
    let pack_candidates = if concurrent_limit_retry_after_ms.is_some() {
        Vec::new()
    } else {
        candidates
    };
    let mut draft = assemble_draft_with_profile_and_options_seeded(
        request.profile,
        request.query.clone(),
        request.budget,
        pack_candidates,
        crate::pack::PackAssemblyOptions {
            redaction_level: options.redaction_level,
            include_coverage_fill: options.output_options.include_coverage_fill,
            output_redaction_enabled,
        },
        determinism,
    )
    .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    let tombstoned_item_count = draft
        .items
        .iter()
        .filter(|item| item.tombstoned_at.is_some())
        .count();
    if options.include_tombstoned
        && tombstoned_item_count > 0
        && !degraded
            .iter()
            .any(|entry| entry.code == "tombstoned_in_results")
    {
        push_degradation(
            &mut degraded,
            "tombstoned_in_results",
            ContextResponseSeverity::Low,
            format!(
                "Context pack includes {tombstoned_item_count} tombstoned memor{suffix} because --include-tombstoned was requested.",
                suffix = if tombstoned_item_count == 1 {
                    "y"
                } else {
                    "ies"
                },
            ),
            None,
        );
    }

    let coordination = load_coordination_snapshot(options, &mut degraded);

    draft.hash = Some(compute_pack_hash_with_output_options_and_coordination(
        &request,
        &draft,
        &degraded,
        options.output_options,
        coordination.as_ref(),
    ));
    if let Some(profile) = agent_profile.as_mut() {
        set_agent_profile_base_pack_hash(profile, draft.hash.as_deref());
    }
    trace.record_elapsed("packAssembly", pack_start);
    let slo = if let Some(retry_after_ms) = concurrent_limit_retry_after_ms {
        let actuals = PackAssemblySloActuals::from_pack_run(
            &draft,
            0,
            trace.candidate_resolution.graph_traversed_edges,
            trace.elapsed_ms("packAssembly"),
        );
        PackAssemblySlo::concurrent_limit_reached(
            options.output_options.resource_profile,
            actuals,
            retry_after_ms,
        )
    } else {
        pack_assembly_slo_for_run(
            options.output_options.resource_profile,
            &draft,
            &search_report,
            &trace,
        )
    };
    let _pack_slot_guard = pack_slot_guard;

    let mut response_degraded = degraded.clone();
    response_degraded.extend(slo.context_degradations());

    let persist_start = Instant::now();
    trace.pack_record_writes = trace.pack_record_writes.saturating_add(1);
    let persist_result = DbConnection::open_file(&database_path)
        .map_err(|error| error.to_string())
        .and_then(|connection| match pack_record_persistence {
            PackRecordPersistence::Ambient => persist_pack_record(
                &connection,
                &options.workspace_path,
                &request,
                &draft,
                &degraded,
            )
            .map_err(|error| error.to_string()),
            PackRecordPersistence::Seeded(pack_id_seed) => persist_pack_record_seeded(
                &connection,
                &options.workspace_path,
                &request,
                &draft,
                &degraded,
                pack_id_seed,
            )
            .map(|_| ())
            .map_err(|error| error.to_string()),
        });
    if let Err(persist_error) = persist_result {
        push_degradation(
            &mut response_degraded,
            "context_pack_persist_failed",
            ContextResponseSeverity::Medium,
            format!("Pack assembled but persistence failed: {persist_error}"),
            Some("ee status --json".to_string()),
        );
    }
    trace.record_elapsed("packPersistence", persist_start);
    trace.record_elapsed("total", total_start);

    let consensus_conflicts = crate::pack::analyze_pack_consensus_conflicts(&draft);
    push_consensus_conflict_degradations(
        &mut response_degraded,
        &consensus_conflicts,
        draft.items.len(),
    );
    let performance = context_performance_json(
        command,
        options,
        &request,
        &search_report,
        &draft,
        &response_degraded,
        &trace,
        &slo,
    );
    let mut response = ContextResponse::new(request, draft, response_degraded)
        .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    response.data.agent_profile = agent_profile;
    response.data.slo = Some(slo);
    response.data.scope_stats = Some(scope_stats);
    response.data.consensus = consensus_conflicts.consensus;
    response.data.conflicts = consensus_conflicts.conflicts;
    response.data.coordination = coordination;

    // Bead bd-17c65.7.7 (G8): best-effort audit-log instrumentation for
    // pack assembly. One `pack.assembled` row per call + one
    // `pack.included_mem` row per selected item. Privacy: only the
    // BLAKE3 prefix of the query reaches the audit log. Failures are
    // swallowed so an audit append never blocks a successful pack.
    audit_context_pack_assembly(&database_path, &options.workspace_path, &response);

    Ok(ContextPackPerformanceRun {
        response,
        performance,
    })
}

fn audit_context_pack_assembly(
    database_path: &Path,
    workspace_path: &Path,
    response: &ContextResponse,
) {
    let Ok(conn) = DbConnection::open_file(database_path) else {
        return;
    };
    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_id = crate::core::curate::stable_workspace_id(&canonical_workspace);
    let query_hash = crate::obs::audit_events::query_hash(&response.data.request.query);
    let pack_id_for_audit = response
        .data
        .pack
        .hash
        .clone()
        .unwrap_or_else(|| "pack_unhashed".to_owned());
    let assembled_details = serde_json::json!({
        "queryHash": &query_hash,
        "packId": &pack_id_for_audit,
        "algorithm_id": response.data.pack.selection_audit.algorithm_id,
        "algorithmId": response.data.pack.selection_audit.algorithm_id,
        "algorithmDescription": response.data.pack.selection_audit.algorithm_description,
        "objective": response.data.pack.selection_audit.objective.as_str(),
        "itemCount": response.data.pack.items.len(),
        "items_selected": response.data.pack.selection_audit.selected_count,
        "itemsSelected": response.data.pack.selection_audit.selected_count,
        "items_skipped": response.data.pack.selection_audit.omitted_count,
        "itemsSkipped": response.data.pack.selection_audit.omitted_count,
        "objective_value": response.data.pack.selection_audit.total_objective_value,
        "objectiveValue": response.data.pack.selection_audit.total_objective_value,
        "budget": response.data.pack.budget.max_tokens(),
        "usedTokens": response.data.pack.used_tokens,
    })
    .to_string();
    let assembled_input = crate::db::CreateAuditInput {
        workspace_id: Some(workspace_id.clone()),
        actor: None,
        action: crate::db::audit_actions::PACK_ASSEMBLED.to_owned(),
        target_type: Some("pack".to_owned()),
        target_id: Some(pack_id_for_audit.clone()),
        details: Some(assembled_details),
    };
    let _ = conn.insert_audit(&crate::db::generate_audit_id(), &assembled_input);

    for (display_index, item) in response.data.pack.items.iter().enumerate() {
        let item_details = serde_json::json!({
            "queryHash": &query_hash,
            "packId": &pack_id_for_audit,
            "rank": item.rank,
            "displayIndex": (display_index + 1) as u32,
            "section": item.section.as_str(),
        })
        .to_string();
        let item_input = crate::db::CreateAuditInput {
            workspace_id: Some(workspace_id.clone()),
            actor: None,
            action: crate::db::audit_actions::PACK_INCLUDED_MEM.to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(item.memory_id.to_string()),
            details: Some(item_details),
        };
        let _ = conn.insert_audit(&crate::db::generate_audit_id(), &item_input);

        for redaction in &item.redactions {
            let redaction_details = serde_json::json!({
                "queryHash": &query_hash,
                "packId": &pack_id_for_audit,
                "rank": item.rank,
                "displayIndex": (display_index + 1) as u32,
                "section": item.section.as_str(),
                "surface": "context",
                "memoryId": item.memory_id.to_string(),
                "detectedPattern": redaction.reason,
                "placeholder": &redaction.placeholder,
                "action": crate::db::audit_actions::REDACT_AT_OUTPUT,
            })
            .to_string();
            let redaction_input = crate::db::CreateAuditInput {
                workspace_id: Some(workspace_id.clone()),
                actor: None,
                action: crate::db::audit_actions::REDACT_AT_OUTPUT.to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(item.memory_id.to_string()),
                details: Some(redaction_details),
            };
            let _ = conn.insert_audit(&crate::db::generate_audit_id(), &redaction_input);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn context_performance_json(
    command: &'static str,
    options: &ContextPackOptions,
    request: &ContextRequest,
    search_report: &SearchReport,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    trace: &ContextPerformanceTrace,
    slo: &PackAssemblySlo,
) -> serde_json::Value {
    serde_json::json!({
        "schema": PERFORMANCE_EXPLAIN_SCHEMA_V1,
        "success": true,
        "data": {
            "command": command,
            "query": query_observation_json(&request.query),
            "queryPlan": {
                "retrievalMode": options.speed.as_str(),
                "requestedCandidatePool": request.candidate_pool,
                "maxResults": request.max_results,
                "effectiveCandidatePool": search_report.requested_limit,
                "maxTokens": draft.budget.max_tokens(),
                "profile": request.profile.as_str(),
                "filtersApplied": !options.filters.is_empty()
                    || options.as_of.is_some()
                    || options.include_expired
                    || options.include_future
                    || options.include_stale,
                "memoryScope": options.memory_scope.as_str(),
                "strictScope": options.strict_scope,
            },
            "profileRuntime": search_report.runtime_profile.data_json(),
            "dbReads": context_db_reads_json(trace),
            "search": context_search_json(search_report, options.speed),
            "candidates": candidate_resolution_json(trace),
            "pack": context_pack_json(draft, slo),
            "cache": {
                "status": "fallback",
                "reason": "pack_cache_governor_not_enabled_for_context_command",
                "selectedItemsUnaffected": true,
            },
            "graph": {
                "status": "not_used",
                "reason": "context_pack_did_not_request_graph_projection",
            },
            "timings": trace.timings.iter().map(performance_timing_json).collect::<Vec<_>>(),
            "fallbacks": degraded.iter().map(context_degradation_json).collect::<Vec<_>>(),
            "redaction": performance_redaction_json(),
        },
    })
}

fn pack_assembly_slo_for_run(
    profile: PackResourceProfile,
    draft: &crate::pack::PackDraft,
    search_report: &SearchReport,
    trace: &ContextPerformanceTrace,
) -> PackAssemblySlo {
    let scanned_count = trace
        .candidate_resolution
        .search_hits
        .max(search_report.results.len())
        .max(draft.selection_audit.candidate_count);
    let actuals = PackAssemblySloActuals::from_pack_run(
        draft,
        scanned_count,
        trace.candidate_resolution.graph_traversed_edges,
        trace.elapsed_ms("packAssembly"),
    );
    PackAssemblySlo::evaluate(profile, actuals)
}

fn context_db_reads_json(trace: &ContextPerformanceTrace) -> serde_json::Value {
    serde_json::json!({
        "dbOpenCount": trace.db_open_count,
        "indexStatusChecks": trace.index_status_checks,
        "memoryBatchReads": trace.candidate_resolution.memory_batch_reads,
        "tagBatchReads": trace.candidate_resolution.tag_batch_reads,
        "artifactLinkReads": trace.candidate_resolution.artifact_link_lookups,
        "focusStateReads": trace.focus_state_read_attempts,
        "packRecordWrites": trace.pack_record_writes,
    })
}

fn context_search_json(
    search_report: &SearchReport,
    speed: crate::search::SpeedMode,
) -> serde_json::Value {
    let metrics = search_report.retrieval_metrics();
    serde_json::json!({
        "status": search_report.status.as_str(),
        "requestedLimit": search_report.requested_limit,
        "candidateBudget": speed.candidate_limit(),
        "returnedHits": search_report.results.len(),
        "usesEmbeddings": speed.uses_embeddings(),
        "metrics": metrics.data_json(),
        "degraded": search_degraded_data_json("search", &search_report.degraded),
        "elapsed": elapsed_timing_json(search_report.elapsed_ms),
    })
}

fn candidate_resolution_json(trace: &ContextPerformanceTrace) -> serde_json::Value {
    let metrics = &trace.candidate_resolution;
    serde_json::json!({
        "searchHits": metrics.search_hits,
        "resolvedMemoryIds": metrics.resolved_memory_ids,
        "uniqueMemoryIds": metrics.unique_memory_ids,
        "convertedCandidates": metrics.converted_candidates,
        "skippedCandidates": metrics.skipped_candidates,
        "tagFilteredCandidates": metrics.tag_filtered_candidates,
        "trustFilteredCandidates": metrics.trust_filtered_candidates,
        "scopeFilteredCandidates": metrics.scope_filtered_candidates,
        "redactionFilteredCandidates": metrics.redaction_filtered_candidates,
        "temporalFilteredCandidates": metrics.temporal_filtered_candidates,
        "temporalRelaxedCandidates": metrics.temporal_relaxed_candidates,
        "graphBoostedCandidates": metrics.graph_boosted_candidates,
        "graphExpandedCandidates": metrics.graph_expanded_candidates,
        "graphFilteredCandidates": metrics.graph_filtered_candidates,
        "graphMissingSeeds": metrics.graph_missing_seeds,
        "graphTraversedEdges": metrics.graph_traversed_edges,
        "filteredBeforeResolution": trace.filtered_count,
        "filterInputCount": trace.filter_input_count,
        "focusStateHits": trace.focus_state_hits,
        "focusCandidateCount": trace.focus_candidate_count,
    })
}

fn context_pack_json(draft: &crate::pack::PackDraft, slo: &PackAssemblySlo) -> serde_json::Value {
    let quality = draft.quality_metrics();
    let producer = crate::models::ProducerMetadata::context_pack(None, None);
    serde_json::json!({
        "profile": draft.selection_audit.profile.as_str(),
        "objective": draft.selection_audit.objective.as_str(),
        "algorithmId": draft.selection_audit.algorithm_id,
        "algorithmDescription": draft.selection_audit.algorithm_description,
        "producer": producer,
        "candidateCount": draft.selection_audit.candidate_count,
        "selectedCount": draft.selection_audit.selected_count,
        "omittedCount": draft.selection_audit.omitted_count,
        "selectionSteps": draft.selection_audit.steps.len(),
        "coverageFillCount": draft.coverage_fill_count(),
        "tokenBudget": {
            "limit": draft.selection_audit.budget_limit,
            "used": draft.selection_audit.budget_used,
            "utilization": quality.budget_utilization,
        },
        "pruning": {
            "tokenBudgetExceeded": quality.omissions.token_budget_exceeded,
            "redundantCandidates": quality.omissions.redundant_candidates,
        },
        "slo": pack_assembly_slo_json(slo),
        "hashPresent": draft.hash.is_some(),
    })
}

fn pack_assembly_slo_json(slo: &PackAssemblySlo) -> serde_json::Value {
    serde_json::json!({
        "schema": slo.schema,
        "profile": slo.profile.as_str(),
        "budgetClass": {
            "candidatesScannedMax": slo.budget_class.candidates_scanned_max,
            "graphTraversalMaxEdges": slo.budget_class.graph_traversal_max_edges,
            "elapsedMsTarget": slo.budget_class.elapsed_ms_target,
            "elapsedMsWarning": slo.budget_class.elapsed_ms_warning,
            "elapsedMsFailure": slo.budget_class.elapsed_ms_failure,
            "concurrentPackMax": slo.budget_class.concurrent_pack_max,
        },
        "actuals": {
            "candidateCount": slo.actuals.candidate_count,
            "scannedCount": slo.actuals.scanned_count,
            "indexGeneration": slo.actuals.index_generation,
            "graphGeneration": slo.actuals.graph_generation,
            "graphEdgesTraversed": slo.actuals.graph_edges_traversed,
            "elapsedMs": slo.actuals.elapsed_ms,
            "memoryBytesPeak": slo.actuals.memory_bytes_peak,
        },
        "status": slo.status.as_str(),
        "degradations": slo.degradations.iter().map(|entry| {
            serde_json::json!({
                "code": entry.code,
                "severity": entry.severity.as_str(),
                "message": &entry.message,
                "repair": &entry.repair,
            })
        }).collect::<Vec<_>>(),
    })
}

fn performance_timing_json(timing: &PerformanceTiming) -> serde_json::Value {
    elapsed_timing_json(timing.elapsed.as_secs_f64() * 1000.0)
        .as_object()
        .map(|elapsed| {
            let mut object = serde_json::Map::new();
            object.insert(
                "name".to_string(),
                serde_json::Value::String(timing.name.to_string()),
            );
            for (key, value) in elapsed {
                object.insert(key.clone(), value.clone());
            }
            serde_json::Value::Object(object)
        })
        .unwrap_or_else(|| {
            serde_json::json!({
                "name": timing.name,
                "elapsedMs": 0.0,
                "elapsedMsBucket": "lt_1ms",
                "nondeterministic": true,
            })
        })
}

fn context_degradation_json(degraded: &ContextResponseDegradation) -> serde_json::Value {
    serde_json::json!({
        "code": &degraded.code,
        "severity": degraded.severity.as_str(),
        "message": &degraded.message,
        "repair": &degraded.repair,
    })
}

fn missing_index_search_report(
    query: &str,
    limit: u32,
    runtime_profile: RuntimeProfileReport,
) -> SearchReport {
    SearchReport {
        status: SearchStatus::IndexNotFound,
        query: query.to_owned(),
        requested_limit: limit,
        results: Vec::new(),
        elapsed_ms: 0.0,
        errors: vec!["Search index not found".to_owned()],
        degraded: vec![SearchDegradation {
            code: "index_missing".to_owned(),
            severity: "medium".to_owned(),
            message: "Search index metadata or files are missing; context used stored memories directly where possible."
                .to_owned(),
            repair: Some("ee index rebuild --workspace .".to_owned()),
        }],
        runtime_profile,
        relevance_floor_applied: None,
        candidates_below_floor: 0,
        source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
        source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
    }
}

fn push_search_degradations(
    degraded: &mut Vec<ContextResponseDegradation>,
    search_degraded: &[SearchDegradation],
) {
    for entry in search_degraded {
        let severity = match entry.severity.as_str() {
            "info" => ContextResponseSeverity::Info,
            "high" => ContextResponseSeverity::High,
            "medium" => ContextResponseSeverity::Medium,
            _ => ContextResponseSeverity::Low,
        };
        push_degradation(
            degraded,
            &entry.code,
            severity,
            entry.message.clone(),
            entry.repair.clone(),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn lexical_memory_fallback_hits(
    connection: &DbConnection,
    workspace_path: &Path,
    query: &str,
    limit: u32,
    include_tombstoned: bool,
    as_of: Option<DateTime<Utc>>,
    include_expired: bool,
    include_future: bool,
    include_stale: bool,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<SearchHit> {
    let query_terms = lexical_terms(query);
    if query_terms.is_empty() {
        return Vec::new();
    }
    let reference_time = as_of.unwrap_or_else(Utc::now);

    let memories = fallback_memories_for_workspace(
        connection,
        workspace_path,
        include_tombstoned,
        as_of,
        include_expired,
        include_future,
        include_stale,
        degraded,
    );
    let mut scored: Vec<(StoredMemory, f32)> = memories
        .into_values()
        .filter_map(|memory| {
            lexical_memory_score(&memory, &query_terms).map(|score| (memory, score))
        })
        .collect();
    scored.sort_by(|(left_memory, left_score), (right_memory, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_memory.id.cmp(&right_memory.id))
    });

    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    scored
        .into_iter()
        .take(limit)
        .map(|(memory, score)| SearchHit {
            doc_id: memory.id.clone(),
            score,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(score),
            rerank_score: None,
            metadata: Some(memory_fallback_metadata(&memory, reference_time)),
            explanation: None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn fallback_memories_for_workspace(
    connection: &DbConnection,
    workspace_path: &Path,
    include_tombstoned: bool,
    as_of: Option<DateTime<Utc>>,
    include_expired: bool,
    include_future: bool,
    include_stale: bool,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> BTreeMap<String, StoredMemory> {
    let mut memories = BTreeMap::new();
    let reference_time = as_of.unwrap_or_else(Utc::now);
    let mut expired_filtered = 0usize;
    let mut future_filtered = 0usize;
    let mut malformed_filtered = 0usize;
    let mut total_seen = 0usize;
    for workspace_id in context_workspace_ids(connection, workspace_path, degraded) {
        match connection.list_memories_for_retrieval(&workspace_id, None, include_tombstoned) {
            Ok(rows) => {
                for memory in rows {
                    total_seen = total_seen.saturating_add(1);
                    match fallback_memory_validity_visibility(
                        &memory,
                        reference_time,
                        include_expired,
                        include_future,
                        include_stale,
                    ) {
                        FallbackMemoryVisibility::Visible => {}
                        FallbackMemoryVisibility::Expired => {
                            expired_filtered = expired_filtered.saturating_add(1);
                            continue;
                        }
                        FallbackMemoryVisibility::Future => {
                            future_filtered = future_filtered.saturating_add(1);
                            continue;
                        }
                        FallbackMemoryVisibility::Malformed => {
                            malformed_filtered = malformed_filtered.saturating_add(1);
                            continue;
                        }
                    }
                    memories.insert(memory.id.clone(), memory);
                }
            }
            Err(error) => push_degradation(
                degraded,
                "context_lexical_fallback_workspace_read_failed",
                ContextResponseSeverity::Low,
                format!("Stored memories for workspace {workspace_id} could not be read: {error}"),
                Some("ee doctor --json".to_owned()),
            ),
        }
    }
    let total_filtered = expired_filtered
        .saturating_add(future_filtered)
        .saturating_add(malformed_filtered);
    if total_filtered > 0 && total_filtered.saturating_mul(2) >= total_seen {
        push_degradation(
            degraded,
            "validity_filtered_significant_recall_drop",
            ContextResponseSeverity::Low,
            format!(
                "Validity window filtering removed {total_filtered} fallback candidate{}; {} candidate{} remain.",
                if total_filtered == 1 { "" } else { "s" },
                memories.len(),
                if memories.len() == 1 { "" } else { "s" },
            ),
            Some("Consider --as-of, --include-expired, --include-future, or --include-stale when historic or inactive memories are expected.".to_owned()),
        );
    }
    memories
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FallbackMemoryVisibility {
    Visible,
    Expired,
    Future,
    Malformed,
}

fn fallback_memory_validity_visibility(
    memory: &StoredMemory,
    reference_time: DateTime<Utc>,
    include_expired: bool,
    include_future: bool,
    _include_stale: bool,
) -> FallbackMemoryVisibility {
    if let Some(valid_from) = memory.valid_from.as_deref() {
        let Some(valid_from) = parse_stored_memory_timestamp(valid_from) else {
            return FallbackMemoryVisibility::Malformed;
        };
        if valid_from > reference_time && !include_future {
            return FallbackMemoryVisibility::Future;
        }
    }

    if let Some(valid_to) = memory.valid_to.as_deref() {
        let Some(valid_to) = parse_stored_memory_timestamp(valid_to) else {
            return FallbackMemoryVisibility::Malformed;
        };
        if valid_to < reference_time && !include_expired {
            return FallbackMemoryVisibility::Expired;
        }
    }

    FallbackMemoryVisibility::Visible
}

fn context_validity_reference_time(
    options: &ContextPackOptions,
    filters: &crate::models::QueryFilters,
) -> Option<DateTime<Utc>> {
    options
        .as_of
        .or_else(|| {
            filters
                .temporal
                .validity
                .as_ref()
                .and_then(|v| v.reference_time)
        })
        .or(filters.temporal.as_of)
}

fn context_include_expired(
    options: &ContextPackOptions,
    filters: &crate::models::QueryFilters,
) -> bool {
    options.include_expired
        || matches!(
            filters
                .temporal
                .validity
                .as_ref()
                .map(|validity| validity.posture),
            Some(
                crate::models::QueryTemporalValidityPosture::Relaxed
                    | crate::models::QueryTemporalValidityPosture::Ignore
            )
        )
}

fn context_include_future(
    options: &ContextPackOptions,
    filters: &crate::models::QueryFilters,
) -> bool {
    options.include_future
        || matches!(
            filters
                .temporal
                .validity
                .as_ref()
                .map(|validity| validity.posture),
            Some(
                crate::models::QueryTemporalValidityPosture::Relaxed
                    | crate::models::QueryTemporalValidityPosture::Ignore
            )
        )
}

fn context_include_stale(
    options: &ContextPackOptions,
    filters: &crate::models::QueryFilters,
) -> bool {
    options.include_stale
        || matches!(
            filters
                .temporal
                .validity
                .as_ref()
                .map(|validity| validity.posture),
            Some(crate::models::QueryTemporalValidityPosture::Ignore)
        )
}

fn context_workspace_ids(
    connection: &DbConnection,
    workspace_path: &Path,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<String> {
    let mut ids = BTreeSet::new();

    for path in context_workspace_path_keys(workspace_path) {
        ids.insert(stable_context_workspace_id(&path));
        let path_string = path.to_string_lossy().into_owned();
        match connection.get_workspace_by_path(&path_string) {
            Ok(Some(workspace)) => {
                ids.insert(workspace.id);
            }
            Ok(None) => {}
            Err(error) => push_degradation(
                degraded,
                "context_lexical_fallback_workspace_lookup_failed",
                ContextResponseSeverity::Low,
                format!("Workspace lookup for {} failed: {error}", path.display()),
                Some("ee doctor --json".to_owned()),
            ),
        }
    }

    ids.into_iter().collect()
}

fn context_workspace_path_keys(workspace_path: &Path) -> BTreeSet<PathBuf> {
    let mut path_keys = BTreeSet::new();
    let absolute = if workspace_path.is_absolute() {
        workspace_path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(workspace_path)
    };
    path_keys.insert(workspace_path.to_path_buf());
    path_keys.insert(absolute.clone());
    if let Ok(canonical) = absolute.canonicalize() {
        path_keys.insert(canonical);
    }
    path_keys
}

fn stable_context_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn lexical_terms(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn lexical_memory_score(memory: &StoredMemory, query_terms: &BTreeSet<String>) -> Option<f32> {
    let haystack =
        format!("{} {} {}", memory.level, memory.kind, memory.content).to_ascii_lowercase();
    let matched = query_terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    if matched == 0 {
        return None;
    }
    Some(matched as f32 / query_terms.len() as f32)
}

fn memory_fallback_metadata(
    memory: &StoredMemory,
    reference_time: DateTime<Utc>,
) -> serde_json::Value {
    serde_json::json!({
        "source": "memory",
        "memoryId": &memory.id,
        "workspaceId": &memory.workspace_id,
        "level": &memory.level,
        "kind": &memory.kind,
        "confidence": memory.confidence,
        "utility": memory.utility,
        "importance": memory.importance,
        "provenanceUri": &memory.provenance_uri,
        "createdAt": &memory.created_at,
        "updatedAt": &memory.updated_at,
        "valid_from": &memory.valid_from,
        "valid_to": &memory.valid_to,
        "validity_status": validity_status_for_memory(memory, reference_time),
        "validity_window_kind": validity_window_kind(memory.valid_from.as_deref(), memory.valid_to.as_deref()),
    })
}

const fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "es" }
}

/// Pagination info returned after applying pagination to candidates.
#[derive(Clone, Debug, Default)]
pub struct PaginationInfo {
    /// Whether pagination was applied.
    pub applied: bool,
    /// Offset used for this page.
    pub offset: u32,
    /// Page size limit.
    pub limit: u32,
    /// Number of items in this page.
    pub page_size: u32,
    /// Whether there are more results after this page.
    pub has_more: bool,
    /// Next cursor token (if has_more is true).
    pub next_cursor: Option<String>,
}

fn apply_pagination(
    candidates: &mut Vec<PackCandidate>,
    pagination: &Option<ContextPagination>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> PaginationInfo {
    let Some(pagination) = pagination else {
        return PaginationInfo::default();
    };

    let total = candidates.len();
    let offset = pagination.offset as usize;
    let limit = pagination.limit as usize;

    if offset >= total {
        candidates.clear();
        return PaginationInfo {
            applied: true,
            offset: pagination.offset,
            limit: pagination.limit,
            page_size: 0,
            has_more: false,
            next_cursor: None,
        };
    }

    let remaining = total.saturating_sub(offset);
    let page_size = remaining.min(limit);
    let has_more = remaining > limit;

    *candidates = candidates
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();

    let next_cursor = if has_more {
        let next_offset = offset + limit;
        let cursor = crate::models::PaginationCursor {
            offset: u32::try_from(next_offset).unwrap_or(u32::MAX),
            query_hash: pagination.query_hash.clone(),
        };
        Some(cursor.encode())
    } else {
        None
    };

    if offset > 0 || has_more {
        push_degradation(
            degraded,
            "context_pagination_applied",
            ContextResponseSeverity::Low,
            format!(
                "Pagination applied: showing {} of {} candidates (offset {}).",
                page_size, total, offset
            ),
            None,
        );
    }

    PaginationInfo {
        applied: true,
        offset: pagination.offset,
        limit: pagination.limit,
        page_size: u32::try_from(page_size).unwrap_or(u32::MAX),
        has_more,
        next_cursor,
    }
}

fn sort_context_candidates(candidates: &mut [PackCandidate]) {
    candidates.sort_by(|left, right| {
        right
            .relevance
            .into_inner()
            .total_cmp(&left.relevance.into_inner())
            .then_with(|| {
                right
                    .utility
                    .into_inner()
                    .total_cmp(&left.utility.into_inner())
            })
            .then_with(|| {
                compare_optional_f32_desc(left.proximity_to_seed, right.proximity_to_seed)
            })
            .then_with(|| left.section.cmp(&right.section))
            .then_with(|| left.memory_id.to_string().cmp(&right.memory_id.to_string()))
    });
}

#[derive(Clone, Debug, PartialEq)]
struct AppliedAgentProfileBias {
    memory_id: String,
    bias: f64,
    counts: AgentContextProfileCounts,
    last_seen_at: String,
}

fn apply_agent_context_profile_bias(
    connection: &DbConnection,
    workspace_path: &Path,
    candidates: &mut [PackCandidate],
) -> Option<serde_json::Value> {
    let agent_name = crate::core::memory_scope::current_agent_name()?;
    let workspace_id =
        resolve_context_profile_workspace_id(connection, workspace_path, candidates)?;
    let profiles = connection
        .list_agent_context_profiles_for_pack(&workspace_id, &agent_name)
        .ok()?;
    if profiles.is_empty() {
        return None;
    }

    let summary =
        summarize_agent_context_profiles(&agent_name, &workspace_id, profiles, candidates);
    Some(summary.into_json())
}

fn resolve_context_profile_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
    candidates: &[PackCandidate],
) -> Option<String> {
    let workspace_path = workspace_path.display().to_string();
    if let Ok(Some(workspace)) = connection.get_workspace_by_path(&workspace_path) {
        return Some(workspace.id);
    }

    candidates.iter().find_map(|candidate| {
        connection
            .get_memory(&candidate.memory_id.to_string())
            .ok()
            .flatten()
            .map(|memory| memory.workspace_id)
    })
}

#[derive(Clone, Debug, PartialEq)]
struct AgentContextProfileSummary {
    agent_name: String,
    workspace_id: String,
    counts: AgentContextProfileCounts,
    bias_magnitude: f64,
    memory_bias_applied: u32,
    cold_start: bool,
    top_biases: Vec<AppliedAgentProfileBias>,
}

impl AgentContextProfileSummary {
    fn into_json(self) -> serde_json::Value {
        let agent_name_hash = agent_context_profile_agent_hash(&self.agent_name);
        let top_biases = self
            .top_biases
            .iter()
            .map(|bias| {
                serde_json::json!({
                    "memoryId": bias.memory_id,
                    "bias": score_json_f64(bias.bias),
                    "helpfulCount": bias.counts.helpful_count,
                    "harmfulCount": bias.counts.harmful_count,
                    "ignoredCount": bias.counts.ignored_count,
                    "lastSeenAt": bias.last_seen_at,
                })
            })
            .collect::<Vec<_>>();

        serde_json::json!({
            "schema": AGENT_CONTEXT_PROFILE_SCHEMA_V1,
            "agentName": self.agent_name,
            "agentNameHash": agent_name_hash.clone(),
            "workspaceId": self.workspace_id,
            "observedOutcomes": self.counts.observed_outcomes(),
            "helpfulCount": self.counts.helpful_count,
            "harmfulCount": self.counts.harmful_count,
            "ignoredCount": self.counts.ignored_count,
            "biasMagnitude": score_json_f64(self.bias_magnitude),
            "maxBiasMagnitude": AGENT_PROFILE_BIAS_CAP,
            "memoryBiasApplied": self.memory_bias_applied,
            "coldStart": self.cold_start,
            "coldStartThreshold": AGENT_PROFILE_COLD_START_OUTCOMES,
            "halfLifeDays": serde_json::Value::Null,
            "determinismKey": {
                "workspaceGeneration": 0,
                "profileGeneration": self.counts.observed_outcomes(),
                "agentNameHash": agent_name_hash,
                "basePackHash": serde_json::Value::Null,
            },
            "topBiases": top_biases,
            "degraded": [],
        })
    }
}

fn summarize_agent_context_profiles(
    agent_name: &str,
    workspace_id: &str,
    profiles: Vec<StoredAgentContextProfileForPack>,
    candidates: &mut [PackCandidate],
) -> AgentContextProfileSummary {
    let mut counts = AgentContextProfileCounts::default();
    let mut by_memory = HashMap::with_capacity(profiles.len());
    for profile in profiles {
        counts = AgentContextProfileCounts::new(
            counts
                .helpful_count
                .saturating_add(profile.counts.helpful_count),
            counts
                .harmful_count
                .saturating_add(profile.counts.harmful_count),
            counts
                .ignored_count
                .saturating_add(profile.counts.ignored_count),
        );
        by_memory.insert(profile.memory_id.clone(), profile);
    }

    let mut top_biases = Vec::new();
    let mut memory_bias_applied = 0_u32;
    let mut bias_magnitude = 0.0_f64;
    for candidate in candidates {
        let memory_id = candidate.memory_id.to_string();
        let Some(profile) = by_memory.get(&memory_id) else {
            continue;
        };
        let bias = profile.counts.bias();
        if bias.cold_start || bias.weight == 0.0 {
            continue;
        }

        let base_relevance = candidate.relevance.into_inner();
        let adjusted_relevance = (f64::from(base_relevance) + bias.weight).clamp(0.0, 1.0) as f32;
        if let Ok(relevance) = UnitScore::parse(adjusted_relevance) {
            candidate.relevance = relevance;
            memory_bias_applied = memory_bias_applied.saturating_add(1);
            bias_magnitude = bias_magnitude.max(bias.weight.abs());
            top_biases.push(AppliedAgentProfileBias {
                memory_id,
                bias: bias.weight,
                counts: profile.counts,
                last_seen_at: profile.last_seen_at.clone(),
            });
        }
    }

    top_biases.sort_by(|left, right| {
        right
            .bias
            .abs()
            .total_cmp(&left.bias.abs())
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    top_biases.truncate(8);

    AgentContextProfileSummary {
        agent_name: agent_name.to_owned(),
        workspace_id: workspace_id.to_owned(),
        counts,
        bias_magnitude,
        memory_bias_applied,
        cold_start: memory_bias_applied == 0,
        top_biases,
    }
}

fn set_agent_profile_base_pack_hash(profile: &mut serde_json::Value, pack_hash: Option<&str>) {
    if let Some(determinism_key) = profile
        .get_mut("determinismKey")
        .and_then(serde_json::Value::as_object_mut)
    {
        determinism_key.insert(
            "basePackHash".to_owned(),
            pack_hash.map_or(serde_json::Value::Null, |hash| {
                serde_json::Value::String(hash.to_owned())
            }),
        );
    }
}

fn agent_context_profile_agent_hash(agent_name: &str) -> String {
    let digest = blake3::hash(agent_name.as_bytes()).to_hex().to_string();
    format!("blake3:{}", &digest[..12])
}

fn score_json_f64(value: f64) -> serde_json::Value {
    if value.is_finite() {
        serde_json::json!((value * 1000.0).round() / 1000.0)
    } else {
        serde_json::Value::Null
    }
}

fn compare_optional_f32_desc(left: Option<f32>, right: Option<f32>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.total_cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn persist_pack_record(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
) -> Result<(), String> {
    persist_pack_record_with_pack_id(
        connection,
        workspace_path,
        request,
        draft,
        degraded,
        PackId::now(),
    )
    .map(|_| ())
}

#[allow(dead_code, reason = "N4.3 staged token-threaded pack ID helper")]
fn persist_pack_record_seeded(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    determinism: &Deterministic<Seed>,
) -> Result<String, String> {
    let mut pack_id_token = determinism.shared_child("ulid.pack");
    persist_pack_record_with_pack_id(
        connection,
        workspace_path,
        request,
        draft,
        degraded,
        PackId::now_seeded(&mut pack_id_token),
    )
}

fn persist_pack_record_with_pack_id(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    pack_id: PackId,
) -> Result<String, String> {
    // Bead bd-17c65.1.9 (A9). Pre-overhaul this surface emitted
    // `context_pack_persist_failed: workspace not found` on every call
    // because the lookup used the raw path. `ee init` / `ee remember`
    // canonicalize before registering, so on macOS `/tmp/...` queries
    // miss the registered `/private/tmp/...` row. Try the raw form
    // first (for tests / pre-registered raw paths), then the canonical
    // (symlink-resolved) form. Matches the pattern in G1's
    // resolve_workspace_id_with_fallback.
    let raw = workspace_path.display().to_string();
    let workspace = match connection
        .get_workspace_by_path(&raw)
        .map_err(|e| format!("workspace lookup failed: {e}"))?
    {
        Some(ws) => ws,
        None => {
            let canonical = workspace_path
                .canonicalize()
                .unwrap_or_else(|_| workspace_path.to_path_buf());
            let canonical_str = canonical.display().to_string();
            if canonical_str == raw {
                return Err("workspace not found".to_string());
            }
            match connection
                .get_workspace_by_path(&canonical_str)
                .map_err(|e| format!("workspace lookup failed: {e}"))?
            {
                Some(ws) => ws,
                None => return Err("workspace not found".to_string()),
            }
        }
    };

    let pack_hash = draft
        .hash
        .clone()
        .unwrap_or_else(|| compute_pack_hash(request, draft, degraded));

    let degraded_json = if degraded.is_empty() {
        None
    } else {
        serde_json::to_string(
            &degraded
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "code": d.code,
                        "severity": d.severity.as_str(),
                        "message": d.message,
                        "repair": d.repair,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .ok()
    };

    let input = CreatePackRecordInput {
        workspace_id: workspace.id.clone(),
        query: request.query.clone(),
        profile: request.profile.as_str().to_string(),
        max_tokens: request.budget.max_tokens(),
        used_tokens: draft.used_tokens,
        item_count: draft.items.len() as u32,
        omitted_count: draft.omitted.len() as u32,
        pack_hash,
        degraded_json,
        created_by: Some("ee context".to_string()),
    };

    let items: Vec<CreatePackItemInput> = draft
        .items
        .iter()
        .map(|item| CreatePackItemInput {
            pack_id: pack_id.to_string(),
            memory_id: item.memory_id.to_string(),
            rank: item.rank,
            section: item.section.as_str().to_string(),
            estimated_tokens: item.estimated_tokens,
            relevance: item.relevance.into_inner(),
            utility: item.utility.into_inner(),
            why: item.why.clone(),
            diversity_key: item.diversity_key.clone(),
            provenance_json: pack_item_provenance_json(&item.provenance),
            trust_class: item.trust.class.as_str().to_string(),
            trust_subclass: item.trust.subclass.clone(),
        })
        .collect();

    let omissions: Vec<CreatePackOmissionInput> = draft
        .omitted
        .iter()
        .map(|omission| CreatePackOmissionInput {
            pack_id: pack_id.to_string(),
            memory_id: omission.memory_id.to_string(),
            estimated_tokens: omission.estimated_tokens,
            reason: omission.reason.as_str().to_string(),
        })
        .collect();

    connection
        .insert_pack_record(&pack_id.to_string(), &input, &items, &omissions)
        .map_err(|e| format!("insert failed: {e}"))?;
    Ok(pack_id.to_string())
}

fn load_coordination_snapshot(
    options: &ContextPackOptions,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCoordinationSnapshot> {
    let path = options.coordination_snapshot_path.as_ref()?;
    match read_coordination_snapshot_contents(path) {
        Ok(contents) => match PackCoordinationSnapshot::from_json_str(
            &contents,
            options.coordination_stale_after_ms,
        ) {
            Ok(snapshot) => {
                crate::obs::log_event(
                    crate::obs::TestEvent::new(
                        crate::obs::test_id_or("coordination_snapshot"),
                        crate::obs::EventKind::Note,
                    )
                    .with_field(
                        "kind",
                        serde_json::Value::String("coordination_snapshot".to_owned()),
                    )
                    .with_field(
                        "source_count",
                        serde_json::Value::Number(snapshot.summary.source_count.into()),
                    )
                    .with_field(
                        "active_conflict_count",
                        serde_json::Value::Number(snapshot.summary.active_conflict_count.into()),
                    ),
                );
                push_coordination_snapshot_degradations(degraded, &snapshot);
                Some(snapshot)
            }
            Err(message) => {
                push_degradation(
                    degraded,
                    "coordination_snapshot_unavailable",
                    ContextResponseSeverity::Low,
                    message,
                    Some("Regenerate the redacted coordination snapshot JSON.".to_owned()),
                );
                None
            }
        },
        Err(error) => {
            push_degradation(
                degraded,
                "coordination_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Coordination snapshot at {} could not be read: {error}",
                    path.display()
                ),
                Some("Check --coordination-snapshot path and permissions.".to_owned()),
            );
            None
        }
    }
}

fn read_coordination_snapshot_contents(path: &Path) -> Result<String, String> {
    if let Some(symlink_path) = first_existing_context_path_symlink_component(path)? {
        return Err(format!(
            "path traverses symbolic link '{}'",
            symlink_path.display()
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err("path is not a regular file".to_string()),
        Err(error) => return Err(format!("failed to inspect path: {error}")),
    }
    fs::read_to_string(path).map_err(|error| error.to_string())
}

fn push_coordination_snapshot_degradations(
    degraded: &mut Vec<ContextResponseDegradation>,
    snapshot: &PackCoordinationSnapshot,
) {
    if snapshot.summary.stale_source_count > 0 {
        push_degradation(
            degraded,
            "coordination_source_stale",
            ContextResponseSeverity::Low,
            "Coordination snapshot contains stale sources.",
            Some(
                "Regenerate the redacted coordination snapshot before relying on coordination posture."
                    .to_owned(),
            ),
        );
    }
    if snapshot.summary.unavailable_source_count > 0 {
        push_degradation(
            degraded,
            "coordination_source_unavailable",
            ContextResponseSeverity::Medium,
            "Coordination snapshot contains unavailable sources.",
            Some("Provide fresh redacted coordination sources or rerun ee swarm brief.".to_owned()),
        );
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
pub(crate) struct PackL2CacheKeyInput {
    pub(crate) workspace_id: String,
    pub(crate) database_generation: u64,
    pub(crate) index_generation: u64,
    pub(crate) graph_generation: Option<u64>,
    pub(crate) redaction_level: RedactionLevel,
    pub(crate) request: ContextRequest,
    pub(crate) output_options: ContextPackOutputOptions,
    pub(crate) memory_scope: MemoryScope,
    pub(crate) strict_scope: bool,
    pub(crate) context_feature_flags_hash: String,
    pub(crate) personalization_generation: Option<u64>,
}

#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
pub(crate) fn compute_pack_l2_cache_key(input: &PackL2CacheKeyInput) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_labeled_bytes(
        &mut hasher,
        "schema",
        PACK_L2_CACHE_KEY_SCHEMA_V1.as_bytes(),
    );
    hash_labeled_bytes(&mut hasher, "workspace_id", input.workspace_id.as_bytes());
    hash_labeled_u64(
        &mut hasher,
        "database_generation",
        input.database_generation,
    );
    hash_labeled_u64(&mut hasher, "index_generation", input.index_generation);
    hash_labeled_optional_u64(&mut hasher, "graph_generation", input.graph_generation);
    hash_labeled_bytes(
        &mut hasher,
        "redaction_level",
        input.redaction_level.as_str().as_bytes(),
    );
    hash_labeled_bytes(&mut hasher, "query", input.request.query.as_bytes());
    hash_labeled_bytes(
        &mut hasher,
        "context_profile",
        input.request.profile.as_str().as_bytes(),
    );
    hash_labeled_u64(
        &mut hasher,
        "max_tokens",
        u64::from(input.request.budget.max_tokens()),
    );
    hash_labeled_u64(
        &mut hasher,
        "candidate_pool",
        u64::from(input.request.candidate_pool),
    );
    hash_labeled_optional_u64(
        &mut hasher,
        "max_results",
        input.request.max_results.map(u64::from),
    );
    hash_labeled_u64(
        &mut hasher,
        "section_count",
        input.request.sections.len() as u64,
    );
    for section in &input.request.sections {
        hash_labeled_bytes(&mut hasher, "section", section.as_str().as_bytes());
    }
    hash_labeled_bytes(
        &mut hasher,
        "output_profile",
        input.output_options.profile.as_str().as_bytes(),
    );
    hash_labeled_bytes(
        &mut hasher,
        "resource_profile",
        input.output_options.resource_profile.as_str().as_bytes(),
    );
    hash_labeled_bool(
        &mut hasher,
        "include_coverage_fill",
        input.output_options.include_coverage_fill,
    );
    hash_labeled_bool(
        &mut hasher,
        "include_rendered_text",
        input.output_options.include_rendered_text,
    );
    hash_labeled_bool(
        &mut hasher,
        "include_skipped",
        input.output_options.include_skipped,
    );
    hash_labeled_bool(
        &mut hasher,
        "include_meta",
        input.output_options.include_meta,
    );
    hash_labeled_bool(
        &mut hasher,
        "include_verbose_meta",
        input.output_options.include_verbose_meta,
    );
    hash_labeled_bool(
        &mut hasher,
        "include_non_affecting_degradations",
        input.output_options.include_non_affecting_degradations,
    );
    hash_labeled_bytes(
        &mut hasher,
        "memory_scope",
        input.memory_scope.as_str().as_bytes(),
    );
    hash_labeled_bool(&mut hasher, "strict_scope", input.strict_scope);
    hash_labeled_bytes(
        &mut hasher,
        "context_feature_flags_hash",
        input.context_feature_flags_hash.as_bytes(),
    );
    hash_labeled_optional_u64(
        &mut hasher,
        "personalization_generation",
        input.personalization_generation,
    );
    finalize_blake3(hasher)
}

#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
fn hash_labeled_bytes(hasher: &mut blake3::Hasher, label: &str, value: &[u8]) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
fn hash_labeled_u64(hasher: &mut blake3::Hasher, label: &str, value: u64) {
    hash_labeled_bytes(hasher, label, &value.to_le_bytes());
}

#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
fn hash_labeled_optional_u64(hasher: &mut blake3::Hasher, label: &str, value: Option<u64>) {
    match value {
        Some(value) => {
            hash_labeled_bool(hasher, &format!("{label}.present"), true);
            hash_labeled_u64(hasher, label, value);
        }
        None => {
            hash_labeled_bool(hasher, &format!("{label}.present"), false);
        }
    }
}

#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
fn hash_labeled_bool(hasher: &mut blake3::Hasher, label: &str, value: bool) {
    hash_labeled_bytes(hasher, label, &[u8::from(value)]);
}

fn compute_pack_hash(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
) -> String {
    compute_pack_hash_with_output_options(
        request,
        draft,
        degraded,
        ContextPackOutputOptions::default(),
    )
}

fn compute_pack_hash_with_output_options(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
) -> String {
    compute_pack_hash_with_output_options_and_coordination(
        request,
        draft,
        degraded,
        output_options,
        None,
    )
}

fn compute_pack_hash_with_output_options_and_coordination(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
    coordination: Option<&PackCoordinationSnapshot>,
) -> String {
    let components =
        compute_pack_hash_components(request, draft, degraded, output_options, coordination);
    log_pack_hash_components(&components);
    components.composite_hash
}

#[derive(Debug)]
struct PackHashComponents {
    pack_request_hash: String,
    draft_items_hash: String,
    degraded_summary_hash: String,
    rendered_text_hash: String,
    composite_hash: String,
}

fn compute_pack_hash_components(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
    coordination: Option<&PackCoordinationSnapshot>,
) -> PackHashComponents {
    use blake3::Hasher;

    let mut request_hasher = Hasher::new();
    request_hasher.update(request.query.as_bytes());
    request_hasher.update(request.profile.as_str().as_bytes());
    request_hasher.update(&request.budget.max_tokens().to_le_bytes());
    request_hasher.update(output_options.profile.as_str().as_bytes());
    request_hasher.update(output_options.resource_profile.as_str().as_bytes());
    request_hasher.update(&[u8::from(output_options.include_coverage_fill)]);
    request_hasher.update(&[u8::from(output_options.include_rendered_text)]);
    request_hasher.update(&[u8::from(output_options.include_skipped)]);
    request_hasher.update(&[u8::from(output_options.include_meta)]);
    request_hasher.update(&[u8::from(output_options.include_verbose_meta)]);

    let mut draft_hasher = Hasher::new();
    draft_hasher.update(&draft.used_tokens.to_le_bytes());

    let rendered_text = crate::pack::render_context_markdown_with_analysis(
        request,
        draft,
        degraded,
        &[],
        &[],
        coordination,
    );
    let mut rendered_text_hasher = Hasher::new();
    rendered_text_hasher.update(rendered_text.as_bytes());

    let mut composite_hasher = Hasher::new();
    composite_hasher.update(request.query.as_bytes());
    composite_hasher.update(request.profile.as_str().as_bytes());
    composite_hasher.update(&request.budget.max_tokens().to_le_bytes());
    composite_hasher.update(output_options.profile.as_str().as_bytes());
    composite_hasher.update(output_options.resource_profile.as_str().as_bytes());
    composite_hasher.update(&[u8::from(output_options.include_coverage_fill)]);
    composite_hasher.update(&[u8::from(output_options.include_rendered_text)]);
    composite_hasher.update(&[u8::from(output_options.include_skipped)]);
    composite_hasher.update(&[u8::from(output_options.include_meta)]);
    composite_hasher.update(&[u8::from(output_options.include_verbose_meta)]);
    composite_hasher.update(&draft.used_tokens.to_le_bytes());
    if output_options.include_rendered_text {
        composite_hasher.update(rendered_text.as_bytes());
    }
    if let Some(coordination) = coordination {
        let coordination_json = serde_json::to_string(coordination).unwrap_or_default();
        composite_hasher.update(coordination_json.as_bytes());
    }

    for item in &draft.items {
        for hasher in [&mut draft_hasher, &mut composite_hasher] {
            hasher.update(item.memory_id.to_string().as_bytes());
            hasher.update(&item.rank.to_le_bytes());
            hasher.update(item.section.as_str().as_bytes());
            hasher.update(item.content.as_bytes());
            hasher.update(&item.estimated_tokens.to_le_bytes());
            hasher.update(&item.relevance.into_inner().to_le_bytes());
            hasher.update(&item.utility.into_inner().to_le_bytes());
            if let Some(proximity_to_seed) = item.proximity_to_seed {
                hasher.update(&proximity_to_seed.to_le_bytes());
            }
            if let Some(score_breakdown) = item.score_breakdown {
                hasher.update(&score_breakdown.text_score.to_le_bytes());
                hasher.update(&score_breakdown.ppr_score.to_le_bytes());
                hasher.update(&score_breakdown.combined_score.to_le_bytes());
            }
            hasher.update(item.why.as_bytes());
            hasher.update(item.selected_in.as_str().as_bytes());
        }
        for provenance in &item.provenance {
            for hasher in [&mut draft_hasher, &mut composite_hasher] {
                hasher.update(provenance.uri.to_string().as_bytes());
                hasher.update(provenance.note.as_bytes());
            }
        }
        if let Some(diversity_key) = &item.diversity_key {
            for hasher in [&mut draft_hasher, &mut composite_hasher] {
                hasher.update(diversity_key.as_bytes());
            }
        }
        for hasher in [&mut draft_hasher, &mut composite_hasher] {
            hasher.update(item.trust.class.as_str().as_bytes());
        }
        if let Some(subclass) = &item.trust.subclass {
            for hasher in [&mut draft_hasher, &mut composite_hasher] {
                hasher.update(subclass.as_bytes());
            }
        }
        if let Some(tombstoned_at) = &item.tombstoned_at {
            for hasher in [&mut draft_hasher, &mut composite_hasher] {
                hasher.update(tombstoned_at.as_bytes());
            }
        }
        if let Some(lifecycle) = &item.lifecycle {
            for hasher in [&mut draft_hasher, &mut composite_hasher] {
                hasher.update(lifecycle.validity_status.as_bytes());
                hasher.update(lifecycle.validity_window_kind.as_bytes());
            }
            if let Some(valid_from) = &lifecycle.valid_from {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(valid_from.as_bytes());
                }
            }
            if let Some(valid_to) = &lifecycle.valid_to {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(valid_to.as_bytes());
                }
            }
        }
        for redaction in &item.redactions {
            for hasher in [&mut draft_hasher, &mut composite_hasher] {
                hasher.update(redaction.reason.as_bytes());
                hasher.update(redaction.placeholder.as_bytes());
            }
        }
    }
    for omission in &draft.omitted {
        draft_hasher.update(omission.memory_id.to_string().as_bytes());
        draft_hasher.update(&omission.estimated_tokens.to_le_bytes());
        draft_hasher.update(omission.reason.as_str().as_bytes());
        if output_options.include_skipped {
            composite_hasher.update(omission.memory_id.to_string().as_bytes());
            composite_hasher.update(&omission.estimated_tokens.to_le_bytes());
            composite_hasher.update(omission.reason.as_str().as_bytes());
        }
    }

    let mut degraded_hasher = Hasher::new();
    for degradation in degraded {
        for hasher in [&mut degraded_hasher, &mut composite_hasher] {
            hasher.update(degradation.code.as_bytes());
            hasher.update(degradation.severity.as_str().as_bytes());
            hasher.update(degradation.message.as_bytes());
        }
        if let Some(repair) = &degradation.repair {
            for hasher in [&mut degraded_hasher, &mut composite_hasher] {
                hasher.update(repair.as_bytes());
            }
        }
    }

    PackHashComponents {
        pack_request_hash: finalize_blake3(request_hasher),
        draft_items_hash: finalize_blake3(draft_hasher),
        degraded_summary_hash: finalize_blake3(degraded_hasher),
        rendered_text_hash: finalize_blake3(rendered_text_hasher),
        composite_hash: finalize_blake3(composite_hasher),
    }
}

fn finalize_blake3(hasher: blake3::Hasher) -> String {
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn log_pack_hash_components(components: &PackHashComponents) {
    let run_index = PACK_HASH_LOG_RUN_INDEX.fetch_add(1, Ordering::Relaxed) + 1;
    crate::obs::log_event(
        crate::obs::TestEvent::new(
            crate::obs::test_id_or("pack_hash_components"),
            crate::obs::EventKind::PackHashComponents,
        )
        .with_field(
            "pack_request_hash",
            serde_json::Value::String(components.pack_request_hash.clone()),
        )
        .with_field(
            "draft_items_hash",
            serde_json::Value::String(components.draft_items_hash.clone()),
        )
        .with_field(
            "degraded_summary_hash",
            serde_json::Value::String(components.degraded_summary_hash.clone()),
        )
        .with_field(
            "rendered_text_hash",
            serde_json::Value::String(components.rendered_text_hash.clone()),
        )
        .with_field(
            "composite_hash",
            serde_json::Value::String(components.composite_hash.clone()),
        )
        .with_field("run_index", serde_json::Value::from(run_index)),
    );
}

#[allow(clippy::type_complexity)]
fn candidates_from_search_with_metrics(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &crate::core::search::SearchReport,
    filters: &crate::models::QueryFilters,
    include_tombstoned: bool,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> (Vec<PackCandidate>, CandidateResolutionMetrics) {
    let mut metrics = CandidateResolutionMetrics {
        search_hits: search_report.results.len(),
        ..CandidateResolutionMetrics::default()
    };

    // Phase 1: Resolve all memory IDs from hits (including artifact links).
    // This still does per-hit artifact link lookups but avoids O(k) memory/tag lookups.
    let mut mesh_blocked_hits = 0usize;
    let mut hit_resolutions: Vec<(
        &crate::core::search::SearchHit,
        Option<(MemoryId, Option<String>)>,
        Option<MeshDisplayProvenance>,
    )> = Vec::new();
    for hit in &search_report.results {
        let mesh_provenance = match mesh_query_visibility(hit.metadata.as_ref()) {
            MeshQueryVisibility::Local => None,
            MeshQueryVisibility::Allowed(provenance) => Some(provenance),
            MeshQueryVisibility::Blocked => {
                metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                mesh_blocked_hits = mesh_blocked_hits.saturating_add(1);
                continue;
            }
        };
        let resolution = match MemoryId::from_str(&hit.doc_id) {
            Ok(id) => Some((id, None)),
            Err(_) => {
                metrics.artifact_link_lookups = metrics.artifact_link_lookups.saturating_add(1);
                artifact_linked_memory_id(connection, hit, degraded)
            }
        };
        if resolution.is_some() {
            metrics.resolved_memory_ids = metrics.resolved_memory_ids.saturating_add(1);
        }
        hit_resolutions.push((hit, resolution, mesh_provenance));
    }
    if mesh_blocked_hits > 0 {
        push_degradation(
            degraded,
            "mesh_workspace_scope_filtered",
            ContextResponseSeverity::Low,
            format!(
                "Filtered {mesh_blocked_hits} mesh-derived search hit{plural} because the indexed workspace-scope decision was not an explicit allow for this workspace.",
                plural = if mesh_blocked_hits == 1 { "" } else { "s" },
            ),
            Some(
                "Review the mesh peer-group binding and import ledger before authorizing remote workspace material."
                    .to_string(),
            ),
        );
    }

    // Collect unique memory IDs for batch loading.
    let memory_ids: Vec<String> = hit_resolutions
        .iter()
        .filter_map(|(_, res, _)| res.as_ref().map(|(mid, _)| mid.to_string()))
        .collect();
    metrics.unique_memory_ids = memory_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let memory_ids_refs: Vec<&str> = memory_ids.iter().map(|s| s.as_str()).collect();

    // Phase 2: Batch load all memories and tags.
    let (memories, tags_map) = load_candidate_batch_maps(connection, &memory_ids_refs, degraded);
    metrics.memory_batch_reads = usize::from(!memory_ids_refs.is_empty());
    metrics.tag_batch_reads = usize::from(!memory_ids_refs.is_empty());

    // Phase 3: Build candidates from preloaded data.
    let mut candidates = Vec::new();
    for (hit, resolution, mesh_provenance) in hit_resolutions {
        match resolution {
            Some((memory_id, artifact_id)) => {
                let memory_key = memory_id.to_string();
                if let Some(mesh_provenance) = mesh_provenance.as_ref()
                    && let Some(memory) = memories.get(&memory_key)
                    && memory.trust_class == TrustClass::HumanExplicit.as_str()
                {
                    metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                    push_mesh_peer_human_explicit_filtered_degradation(
                        degraded,
                        memory,
                        mesh_provenance,
                    );
                    continue;
                }
                if !filters.tags.is_empty() {
                    let tags = tags_map.get(&memory_key).cloned().unwrap_or_default();
                    if !filters.matches_tags(&tags) {
                        metrics.tag_filtered_candidates =
                            metrics.tag_filtered_candidates.saturating_add(1);
                        continue;
                    }
                }
                if !filters.temporal.is_empty() {
                    let Some(memory) = memories.get(&memory_key) else {
                        metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                        push_degradation(
                            degraded,
                            "context_candidate_skipped",
                            ContextResponseSeverity::Low,
                            format!(
                                "Search hit {} could not be converted into a pack candidate.",
                                hit.doc_id
                            ),
                            Some("ee index rebuild --workspace .".to_string()),
                        );
                        continue;
                    };
                    if memory.tombstoned_at.is_some() && !include_tombstoned {
                        metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                        push_degradation(
                            degraded,
                            "context_candidate_skipped",
                            ContextResponseSeverity::Low,
                            format!(
                                "Search hit {} could not be converted into a pack candidate.",
                                hit.doc_id
                            ),
                            Some("ee index rebuild --workspace .".to_string()),
                        );
                        continue;
                    }
                    match temporal_memory_outcome(memory, &filters.temporal) {
                        TemporalCandidateOutcome::Include => {}
                        TemporalCandidateOutcome::Exclude => {
                            metrics.temporal_filtered_candidates =
                                metrics.temporal_filtered_candidates.saturating_add(1);
                            continue;
                        }
                        TemporalCandidateOutcome::IncludeRelaxedInvalid => {
                            metrics.temporal_relaxed_candidates =
                                metrics.temporal_relaxed_candidates.saturating_add(1);
                        }
                    }
                }
                if !filters.trust.is_empty() {
                    let Some(memory) = memories.get(&memory_key) else {
                        metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                        continue;
                    };
                    let posture = posture_for_trust_class(&memory.trust_class);
                    if !filters.trust.matches(&memory.trust_class, posture) {
                        metrics.trust_filtered_candidates =
                            metrics.trust_filtered_candidates.saturating_add(1);
                        continue;
                    }
                }
                if !filters.redaction.allow_categories.is_empty() {
                    let Some(memory) = memories.get(&memory_key) else {
                        metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                        continue;
                    };
                    if !redaction_allow_categories(&memory.content, &filters.redaction) {
                        metrics.redaction_filtered_candidates =
                            metrics.redaction_filtered_candidates.saturating_add(1);
                        continue;
                    }
                }
                let preloaded = PreloadedCandidateSource {
                    memories: &memories,
                    tags_map: &tags_map,
                    workspace_path,
                    query: &search_report.query,
                    validity_reference_time: filters
                        .temporal
                        .validity
                        .as_ref()
                        .and_then(|validity| validity.reference_time)
                        .or(filters.temporal.as_of),
                    include_tombstoned,
                };
                match candidate_from_hit_preloaded(preloaded, hit, memory_id, artifact_id, degraded)
                {
                    Some(candidate) => {
                        metrics.converted_candidates =
                            metrics.converted_candidates.saturating_add(1);
                        candidates.push(candidate);
                    }
                    None => {
                        metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                        push_degradation(
                            degraded,
                            "context_candidate_skipped",
                            ContextResponseSeverity::Low,
                            format!(
                                "Search hit {} could not be converted into a pack candidate.",
                                hit.doc_id
                            ),
                            Some("ee index rebuild --workspace .".to_string()),
                        );
                    }
                }
            }
            None => {
                metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                push_degradation(
                    degraded,
                    "context_candidate_skipped",
                    ContextResponseSeverity::Low,
                    format!(
                        "Search hit {} could not be converted into a pack candidate.",
                        hit.doc_id
                    ),
                    Some("ee index rebuild --workspace .".to_string()),
                );
            }
        }
    }
    (candidates, metrics)
}

fn push_mesh_peer_human_explicit_filtered_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    memory: &StoredMemory,
    provenance: &MeshDisplayProvenance,
) {
    push_degradation(
        degraded,
        "mesh_peer_human_explicit_filtered",
        ContextResponseSeverity::Medium,
        format!(
            "Mesh-derived memory {} was excluded because peer material must not appear as local human_explicit; cachedMaterialId={}, originWorkspaceAlias={}, producerPeer={}, importDecisionRef={}, trustLane={}, redactionPosture={}.",
            memory.id,
            provenance.cached_material_id,
            provenance.origin_workspace_alias,
            provenance.producer_peer,
            provenance.import_decision_ref,
            provenance.trust_lane,
            provenance.redaction_posture
        ),
        Some(
            "Re-import the peer material with a peer policy import_trust_class such as agent_assertion or agent_validated, then rebuild the index."
                .to_string(),
        ),
    );
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct GraphHintApplicationMetrics {
    boosted_candidates: usize,
    expanded_candidates: usize,
    filtered_candidates: usize,
    missing_seeds: usize,
    traversed_edges: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PersonalizedPageRankRerankMetrics {
    reranked_candidates: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ProximityToSeedMetrics {
    annotated_candidates: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GraphHintEvidence {
    seed_memory_id: String,
    depth: u32,
    relation: Option<String>,
    traversal: crate::models::QueryGraphTraversal,
}

fn apply_personalized_pagerank_rerank(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &SearchReport,
    candidates: &mut [PackCandidate],
    ppr_weight: f32,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> PersonalizedPageRankRerankMetrics {
    let ppr_weight = ppr_weight.clamp(0.0, 1.0);
    if candidates.is_empty() || ppr_weight == 0.0 {
        return PersonalizedPageRankRerankMetrics::default();
    }
    match context_ppr_feature_enabled(workspace_path) {
        Ok(true) => {}
        Ok(false) => {
            push_ppr_feature_disabled_degradation(degraded);
            return PersonalizedPageRankRerankMetrics::default();
        }
        Err(message) => {
            push_degradation(
                degraded,
                "context_config_unavailable",
                ContextResponseSeverity::Medium,
                message,
                Some("Fix or remove .ee/config.toml.".to_string()),
            );
            return PersonalizedPageRankRerankMetrics::default();
        }
    }

    let workspace_ids = graph_context_workspace_ids(connection, workspace_path, degraded);
    let Some(snapshot) = latest_valid_memory_links_snapshot(connection, &workspace_ids, degraded)
    else {
        return PersonalizedPageRankRerankMetrics::default();
    };

    let seed_map = personalized_pagerank_seed_map(search_report, candidates);
    if seed_map.is_empty() {
        push_degradation(
            degraded,
            GRAPH_PPR_EMPTY_SEED_SET_CODE,
            ContextResponseSeverity::Low,
            "PPR rerank skipped because the graph seed set was empty.",
            Some(
                "Broaden the query or lower the relevance floor before enabling PPR reranking."
                    .to_string(),
            ),
        );
        return PersonalizedPageRankRerankMetrics::default();
    }

    let seed_weights = seed_map
        .iter()
        .map(|(memory_id, weight)| (memory_id.to_string(), *weight))
        .collect::<BTreeMap<_, _>>();
    let policy = crate::graph::ppr::PersonalizedPageRankPolicy::default();
    let ppr_params = crate::graph::ppr::personalized_pagerank_cache_params(policy, &seed_weights);
    let cache_spec = crate::graph::algorithms::AlgorithmResultCacheSpec {
        conn: connection,
        workspace_id: &snapshot.workspace_id,
        snapshot_id: &snapshot.id,
        snapshot_content_hash: &snapshot.content_hash,
        algorithm: "personalized_pagerank",
        params: &ppr_params,
        ttl_seconds: 300,
    };
    let ppr_start = Instant::now();
    let cache_run = match crate::graph::ppr::compute_personalized_pagerank_result_cached_with_graph(
        &cache_spec,
        &seed_weights,
        policy,
        || {
            crate::graph::build_memory_graph(
                connection,
                &crate::graph::ProjectionOptions::default(),
            )
            .map(|projection| projection.graph)
        },
    ) {
        Ok(result) => result,
        Err(error) => {
            push_degradation(
                degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Personalized PageRank rerank skipped because PPR computation failed: {error}"
                ),
                Some("ee graph centrality-refresh".to_string()),
            );
            return PersonalizedPageRankRerankMetrics::default();
        }
    };
    let elapsed_ms = u64::try_from(ppr_start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let result = cache_run.result;
    if !cache_run.cache_hit {
        match crate::graph::ppr::emit_personalized_pagerank_witness(
            &crate::graph::ppr::PersonalizedPageRankWitnessSpec {
                conn: connection,
                workspace_id: &snapshot.workspace_id,
                snapshot_id: &snapshot.id,
                snapshot_version: u64::from(snapshot.snapshot_version),
                params: &ppr_params,
                elapsed_ms,
            },
            &result,
        ) {
            Ok(()) => {}
            Err(error) => {
                tracing::debug!(
                    algorithm = "personalized_pagerank",
                    snapshot_id = %snapshot.id,
                    error = %error,
                    "context PPR witness emission failed"
                );
            }
        }
    }
    let scores = result
        .scores
        .iter()
        .filter_map(|score| {
            MemoryId::from_str(&score.node)
                .ok()
                .map(|memory_id| (memory_id, score.score))
        })
        .collect::<HashMap<_, _>>();

    let mut reranked_candidates = 0_usize;
    for candidate in candidates {
        let base = candidate.relevance.into_inner();
        let ppr_score = scores
            .get(&candidate.memory_id)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0) as f32;
        let blended = (ppr_weight * ppr_score) + ((1.0 - ppr_weight) * base);
        let Some(score) = unit_score(blended) else {
            continue;
        };
        candidate.relevance = score;
        candidate.score_breakdown =
            Some(PackScoreBreakdown::ppr(base, ppr_score, score.into_inner()));
        candidate.why = format!(
            "{} Personalized PageRank rerank blended base={base:.4}, ppr={ppr_score:.4}, weight={:.2}, snapshot={}.",
            candidate.why, ppr_weight, snapshot.id
        );
        reranked_candidates = reranked_candidates.saturating_add(1);
    }

    PersonalizedPageRankRerankMetrics {
        reranked_candidates,
    }
}

fn context_ppr_feature_enabled(workspace_path: &Path) -> Result<bool, String> {
    let config = context_workspace_config(workspace_path, "Personalized PageRank rerank")?;
    Ok(config
        .and_then(|config| config.graph.feature.ppr_enabled)
        .unwrap_or(false))
}

fn context_workspace_config(
    workspace_path: &Path,
    surface: &str,
) -> Result<Option<ConfigFile>, String> {
    let config_path = workspace_path.join(".ee").join("config.toml");
    match context_config_path_is_regular_file_no_symlinks(&config_path) {
        Ok(false) => return Ok(None),
        Ok(true) => {}
        Err(message) => {
            return Err(format!(
                "{surface} skipped because workspace config {} could not be read: {message}",
                config_path.display()
            ));
        }
    }
    let contents = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "{surface} skipped because workspace config {} could not be read: {error}",
                config_path.display()
            ));
        }
    };
    ConfigFile::parse(&contents)
        .map_err(|error| {
            format!(
                "{surface} skipped because workspace config {} could not be parsed: {error}",
                config_path.display()
            )
        })
        .map(Some)
}

fn context_config_path_is_regular_file_no_symlinks(path: &Path) -> Result<bool, String> {
    if let Some(symlink_path) = first_existing_context_path_symlink_component(path)? {
        return Err(format!(
            "path traverses symbolic link '{}'",
            symlink_path.display()
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err("path is not a regular file".to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to inspect path: {error}")),
    }
}

fn first_existing_context_path_symlink_component(path: &Path) -> Result<Option<PathBuf>, String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "failed to inspect path component '{}': {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(None)
}

fn context_read_pool_config(
    workspace_path: &Path,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> (PoolConfig, bool) {
    match context_workspace_config(workspace_path, "Read-pool snapshot pin") {
        Ok(config) => {
            let read_pool = config
                .map(|config| config.storage.read_pool)
                .unwrap_or_default();
            context_read_pool_config_from_values(read_pool, ContextReadPoolEnv::current())
        }
        Err(message) => {
            push_degradation(
                degraded,
                "context_config_unavailable",
                ContextResponseSeverity::Medium,
                message,
                Some("Fix or remove .ee/config.toml.".to_string()),
            );
            (PoolConfig::default_single(), true)
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ContextReadPoolEnv {
    size: Option<u64>,
    idle_timeout_seconds: Option<u64>,
    max_pin_duration_seconds: Option<u64>,
    acquire_timeout_ms: Option<u64>,
    disable_pin: Option<bool>,
}

impl ContextReadPoolEnv {
    fn current() -> Self {
        Self {
            size: read_env_u64(EnvVar::ReadPoolSize),
            idle_timeout_seconds: read_env_u64(EnvVar::ReadPoolIdleTimeoutSeconds),
            max_pin_duration_seconds: read_env_u64(EnvVar::ReadPoolMaxPinSeconds),
            acquire_timeout_ms: read_env_u64(EnvVar::ReadPoolAcquireTimeoutMs),
            disable_pin: read_env_bool(EnvVar::ReadPoolDisablePin),
        }
    }
}

fn context_read_pool_config_from_values(
    read_pool: ReadPoolConfig,
    env: ContextReadPoolEnv,
) -> (PoolConfig, bool) {
    let max_size = env
        .size
        .or(read_pool.size)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1);
    let idle_timeout_seconds = env
        .idle_timeout_seconds
        .or(read_pool.idle_timeout_seconds)
        .unwrap_or(30);
    let max_pin_duration_seconds = env
        .max_pin_duration_seconds
        .or(read_pool.max_pin_duration_seconds)
        .unwrap_or(30);
    let acquire_timeout_ms = env.acquire_timeout_ms.unwrap_or(5000);
    let pin_snapshot = env
        .disable_pin
        .map(|disabled| !disabled)
        .or(read_pool.pin_snapshot)
        .unwrap_or(true);

    (
        PoolConfig::new(max_size, Duration::from_secs(idle_timeout_seconds))
            .with_max_pin_duration(Duration::from_secs(max_pin_duration_seconds))
            .with_acquire_timeout(Duration::from_millis(acquire_timeout_ms)),
        pin_snapshot,
    )
}

fn read_env_u64(var: EnvVar) -> Option<u64> {
    read_env_var(var).and_then(|raw| raw.parse::<u64>().ok())
}

fn read_env_bool(var: EnvVar) -> Option<bool> {
    read_env_var(var).and_then(|raw| match raw.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    })
}

fn checked_context_read_snapshot<'snapshot>(
    read_pool: &ReadConnectionPool,
    read_snapshot: &'snapshot SnapshotPin<'_>,
) -> Result<&'snapshot DbConnection, ContextPackError> {
    read_pool.expire_stale_pins();
    read_snapshot
        .checked_connection()
        .map_err(|error| ContextPackError::Storage(format!("Read snapshot unavailable: {error}")))
}

fn push_ppr_feature_disabled_degradation(degraded: &mut Vec<ContextResponseDegradation>) {
    push_degradation(
        degraded,
        "graph_feature_disabled",
        ContextResponseSeverity::Medium,
        format!("Personalized PageRank rerank is disabled by {GRAPH_FEATURE_PPR_ENABLED_KEY}."),
        Some(format!(
            "ee config set {GRAPH_FEATURE_PPR_ENABLED_KEY} true"
        )),
    );
}

fn apply_proximity_to_seed_scores(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &SearchReport,
    candidates: &mut [PackCandidate],
    degraded: &mut Vec<ContextResponseDegradation>,
) -> ProximityToSeedMetrics {
    if candidates.is_empty() {
        return ProximityToSeedMetrics::default();
    }
    match context_proximity_feature_enabled(workspace_path) {
        Ok(true) => {}
        Ok(false) => {
            push_proximity_feature_disabled_degradation(degraded);
            return ProximityToSeedMetrics::default();
        }
        Err(message) => {
            push_degradation(
                degraded,
                "context_config_unavailable",
                ContextResponseSeverity::Medium,
                message,
                Some("Fix or remove .ee/config.toml.".to_string()),
            );
            return ProximityToSeedMetrics::default();
        }
    }

    let seed_map = personalized_pagerank_seed_map(search_report, candidates);
    if seed_map.is_empty() {
        return ProximityToSeedMetrics::default();
    }

    let graph = match context_proximity_graph(connection) {
        Ok(graph) => graph,
        Err(error) => {
            push_degradation(
                degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Proximity-to-seed scores skipped because memory graph projection failed: {error}"
                ),
                Some("ee graph centrality-refresh".to_string()),
            );
            return ProximityToSeedMetrics::default();
        }
    };

    let tree = match crate::graph::gomory_hu::build_gomory_hu_tree(&graph) {
        Ok(tree) => tree,
        Err(error) => {
            push_degradation(
                degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Proximity-to-seed scores skipped because Gomory-Hu projection failed: {error}"
                ),
                Some("ee graph centrality-refresh".to_string()),
            );
            return ProximityToSeedMetrics::default();
        }
    };

    let seed_ids = seed_map.keys().copied().collect::<Vec<_>>();
    let mut annotated_candidates = 0_usize;
    for candidate in candidates {
        let mut best = None;
        let candidate_id = candidate.memory_id.to_string();
        for seed_id in &seed_ids {
            let seed_id = seed_id.to_string();
            let cut = if seed_id == candidate_id {
                Some(0.0)
            } else {
                crate::graph::gomory_hu::query_min_cut(&tree, &candidate_id, &seed_id)
            };
            if let Some(cut) = cut.filter(|cut| cut.is_finite() && *cut >= 0.0) {
                best = Some(best.map_or(cut, |current: f64| current.max(cut)));
            }
        }
        if let Some(best) = best {
            candidate.proximity_to_seed = Some(best as f32);
            annotated_candidates = annotated_candidates.saturating_add(1);
        }
    }

    ProximityToSeedMetrics {
        annotated_candidates,
    }
}

fn context_proximity_feature_enabled(workspace_path: &Path) -> Result<bool, String> {
    let config = context_workspace_config(workspace_path, "Proximity-to-seed scoring")?;
    Ok(config
        .and_then(|config| config.graph.feature.proximity_enabled)
        .unwrap_or(false))
}

fn push_proximity_feature_disabled_degradation(degraded: &mut Vec<ContextResponseDegradation>) {
    push_degradation(
        degraded,
        "graph_feature_disabled",
        ContextResponseSeverity::Medium,
        format!("Proximity-to-seed scoring is disabled by {GRAPH_FEATURE_PROXIMITY_ENABLED_KEY}."),
        Some(format!(
            "ee config set {GRAPH_FEATURE_PROXIMITY_ENABLED_KEY} true"
        )),
    );
}

fn context_proximity_graph(connection: &DbConnection) -> Result<fnx_classes::Graph, String> {
    use fnx_classes::AttrMap;
    use fnx_runtime::CgseValue;

    let links = connection
        .list_all_memory_links(None)
        .map_err(|error| error.to_string())?;
    let mut graph = fnx_classes::Graph::strict();
    for link in links.into_iter().filter(|link| {
        crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
    }) {
        graph.add_node(&link.src_memory_id);
        graph.add_node(&link.dst_memory_id);
        let mut attrs = AttrMap::new();
        attrs.insert(
            "weight".to_string(),
            CgseValue::Float(f64::from(link.weight)),
        );
        attrs.insert(
            "confidence".to_string(),
            CgseValue::Float(f64::from(link.confidence)),
        );
        attrs.insert(
            "relation".to_string(),
            CgseValue::String(link.relation.clone()),
        );
        graph
            .add_edge_with_attrs(link.src_memory_id, link.dst_memory_id, attrs)
            .map_err(|error| error.to_string())?;
    }
    Ok(graph)
}

fn effective_context_ppr_weight(value: Option<f32>) -> f32 {
    match value {
        Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
        Some(_) => DEFAULT_CONTEXT_PPR_WEIGHT,
        None => 0.0,
    }
}

fn latest_valid_memory_links_snapshot(
    connection: &DbConnection,
    workspace_ids: &BTreeSet<String>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<crate::db::StoredGraphSnapshot> {
    let mut stale_snapshot = None;
    for workspace_id in workspace_ids {
        match connection
            .get_latest_graph_snapshot(workspace_id, crate::db::GraphSnapshotType::MemoryLinks)
        {
            Ok(Some(snapshot)) if snapshot.status == crate::db::GraphSnapshotStatus::Valid => {
                return Some(snapshot);
            }
            Ok(Some(snapshot)) => {
                stale_snapshot.get_or_insert(snapshot);
            }
            Ok(None) => {}
            Err(error) => push_degradation(
                degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!("Graph snapshot posture could not be checked for {workspace_id}: {error}"),
                Some("ee graph centrality-refresh".to_string()),
            ),
        }
    }

    if let Some(snapshot) = stale_snapshot {
        push_degradation(
            degraded,
            GRAPH_PPR_SNAPSHOT_STALE_CODE,
            ContextResponseSeverity::Medium,
            format!(
                "PPR rerank skipped because graph snapshot {} is {}.",
                snapshot.id,
                snapshot.status.as_str()
            ),
            Some("ee graph snapshot refresh --workspace .".to_string()),
        );
    } else {
        push_degradation(
            degraded,
            "context_graph_snapshot_missing",
            ContextResponseSeverity::Low,
            "Personalized PageRank rerank skipped because no valid memory_links graph snapshot exists.",
            Some("ee graph centrality-refresh".to_string()),
        );
    }
    None
}

fn personalized_pagerank_seed_map(
    search_report: &SearchReport,
    candidates: &[PackCandidate],
) -> HashMap<MemoryId, f64> {
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id)
        .collect::<BTreeSet<_>>();
    let mut seed_map = HashMap::new();
    for hit in &search_report.results {
        let Ok(memory_id) = MemoryId::from_str(&hit.doc_id) else {
            continue;
        };
        if !candidate_ids.contains(&memory_id) {
            continue;
        }
        let vector_weight = positive_f32_score(hit.score);
        let lexical_weight = hit.lexical_score.and_then(positive_f32_score);
        let weight = match (vector_weight, lexical_weight) {
            (Some(vector), Some(lexical)) => vector.max(lexical),
            (Some(vector), None) => vector,
            (None, Some(lexical)) => lexical,
            (None, None) => continue,
        };
        seed_map
            .entry(memory_id)
            .and_modify(|current| {
                if weight > *current {
                    *current = weight;
                }
            })
            .or_insert(weight);
    }
    seed_map
}

fn positive_f32_score(value: f32) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(f64::from(value))
}

fn apply_graph_hints(
    connection: &DbConnection,
    workspace_path: &Path,
    filters: &crate::models::QueryFilters,
    include_tombstoned: bool,
    candidates: &mut Vec<PackCandidate>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> GraphHintApplicationMetrics {
    let graph = &filters.graph;
    if graph.is_empty() {
        return GraphHintApplicationMetrics::default();
    }

    let workspace_ids = graph_context_workspace_ids(connection, workspace_path, degraded);
    push_graph_snapshot_posture(connection, &workspace_ids, degraded);

    let mut metrics = GraphHintApplicationMetrics::default();
    let (graph_nodes, traversed_edges, missing_seeds) = graph_hint_nodes(
        connection,
        graph,
        &workspace_ids,
        include_tombstoned,
        degraded,
    );
    metrics.traversed_edges = traversed_edges;
    metrics.missing_seeds = missing_seeds;

    if graph_nodes.is_empty() {
        push_degradation(
            degraded,
            "context_graph_no_candidates",
            ContextResponseSeverity::Low,
            "Graph hints produced no candidate memories.",
            Some(
                "Check graph.seedMemories or create memory links with related memories."
                    .to_string(),
            ),
        );
        if !graph.include_orphans {
            let filtered = candidates.len();
            candidates.clear();
            metrics.filtered_candidates = filtered;
        }
        return metrics;
    }

    let graph_ids: BTreeSet<String> = graph_nodes.keys().cloned().collect();
    for candidate in candidates.iter_mut() {
        if let Some(evidence) = graph_nodes.get(&candidate.memory_id.to_string()) {
            if boost_candidate_for_graph(candidate, evidence) {
                metrics.boosted_candidates = metrics.boosted_candidates.saturating_add(1);
            }
        }
    }

    if !graph.include_orphans {
        let before = candidates.len();
        candidates.retain(|candidate| graph_ids.contains(&candidate.memory_id.to_string()));
        let filtered = before.saturating_sub(candidates.len());
        metrics.filtered_candidates = filtered;
        if filtered > 0 {
            let noun = if filtered == 1 {
                "candidate"
            } else {
                "candidates"
            };
            push_degradation(
                degraded,
                "context_graph_orphans_filtered",
                ContextResponseSeverity::Low,
                format!("{filtered} context {noun} excluded because graph.includeOrphans=false."),
                Some("Set graph.includeOrphans=true to keep lexical candidates outside the graph neighborhood.".to_string()),
            );
        }
    }

    let existing: BTreeSet<String> = candidates
        .iter()
        .map(|candidate| candidate.memory_id.to_string())
        .collect();
    let expansion_ids: Vec<String> = graph_nodes
        .keys()
        .filter(|memory_id| !existing.contains(*memory_id))
        .cloned()
        .collect();
    if expansion_ids.is_empty() {
        return metrics;
    }

    let expansion_refs: Vec<&str> = expansion_ids.iter().map(String::as_str).collect();
    let (memories, tags_map) = load_candidate_batch_maps(connection, &expansion_refs, degraded);
    for memory_id in expansion_ids {
        let Some(memory) = memories.get(&memory_id) else {
            metrics.missing_seeds = metrics.missing_seeds.saturating_add(1);
            if graph_nodes
                .get(&memory_id)
                .is_some_and(|evidence| evidence.depth == 0)
            {
                push_degradation(
                    degraded,
                    "context_graph_seed_missing",
                    ContextResponseSeverity::Low,
                    format!("Graph seed memory {memory_id} was not found in the memory store."),
                    Some(
                        "Use graph.seedMemories values returned by ee remember/search/why."
                            .to_string(),
                    ),
                );
            }
            continue;
        };
        if memory.tombstoned_at.is_some() && !include_tombstoned {
            continue;
        }
        if !workspace_ids.contains(&memory.workspace_id) {
            metrics.filtered_candidates = metrics.filtered_candidates.saturating_add(1);
            push_degradation(
                degraded,
                "context_graph_workspace_filtered",
                ContextResponseSeverity::Low,
                format!(
                    "Graph candidate {memory_id} belongs to workspace {}, outside the active workspace scope.",
                    memory.workspace_id
                ),
                Some("Use graph.seedMemories from the active workspace.".to_string()),
            );
            continue;
        }
        let tags = tags_map.get(&memory_id).cloned().unwrap_or_default();
        if !graph_memory_matches_filters(memory, &tags, filters) {
            continue;
        }
        let Some(typed_memory_id) = MemoryId::from_str(&memory_id).ok() else {
            continue;
        };
        let Some(evidence) = graph_nodes.get(&memory_id) else {
            continue;
        };
        if let Some(candidate) = graph_candidate_from_memory(
            memory,
            typed_memory_id,
            &tags,
            evidence,
            workspace_path,
            degraded,
        ) {
            metrics.expanded_candidates = metrics.expanded_candidates.saturating_add(1);
            candidates.push(candidate);
        }
    }

    if metrics.expanded_candidates > 0 {
        push_degradation(
            degraded,
            "context_graph_expanded_candidates",
            ContextResponseSeverity::Low,
            format!(
                "{} graph-neighborhood candidate{} added to the context candidate pool.",
                metrics.expanded_candidates,
                plural_suffix(metrics.expanded_candidates)
            ),
            None,
        );
    }

    metrics
}

fn push_graph_snapshot_posture(
    connection: &DbConnection,
    workspace_ids: &BTreeSet<String>,
    degraded: &mut Vec<ContextResponseDegradation>,
) {
    let mut stale_snapshot = None;
    for workspace_id in workspace_ids {
        match connection
            .get_latest_graph_snapshot(workspace_id, crate::db::GraphSnapshotType::MemoryLinks)
        {
            Ok(Some(snapshot)) if snapshot.status == crate::db::GraphSnapshotStatus::Valid => {
                return;
            }
            Ok(Some(snapshot)) => {
                stale_snapshot.get_or_insert(snapshot);
            }
            Ok(None) => {}
            Err(error) => push_degradation(
                degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!("Graph snapshot posture could not be checked for {workspace_id}: {error}"),
                Some("ee graph centrality-refresh".to_string()),
            ),
        }
    }

    if let Some(snapshot) = stale_snapshot {
        push_degradation(
            degraded,
            "context_graph_snapshot_not_current",
            ContextResponseSeverity::Low,
            format!(
                "Graph snapshot {} is {}; query-file traversal used source-of-truth memory_links instead of snapshot centrality.",
                snapshot.id,
                snapshot.status.as_str()
            ),
            Some("ee graph centrality-refresh".to_string()),
        );
    } else {
        push_degradation(
            degraded,
            "context_graph_snapshot_missing",
            ContextResponseSeverity::Low,
            "No persisted graph snapshot exists; query-file traversal used source-of-truth memory_links without centrality boosts.",
            Some("ee graph centrality-refresh".to_string()),
        );
    }
}

fn graph_context_workspace_ids(
    connection: &DbConnection,
    workspace_path: &Path,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> BTreeSet<String> {
    let mut workspace_ids = BTreeSet::new();
    for path in context_workspace_path_keys(workspace_path) {
        workspace_ids.insert(stable_context_workspace_id(&path));
        let path_string = path.to_string_lossy().into_owned();
        match connection.get_workspace_by_path(&path_string) {
            Ok(Some(workspace)) => {
                workspace_ids.insert(workspace.id);
            }
            Ok(None) => {}
            Err(error) => push_degradation(
                degraded,
                "context_graph_workspace_lookup_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Graph snapshot posture could not resolve workspace path {}: {error}",
                    path.display()
                ),
                Some("ee status --json".to_string()),
            ),
        }
    }

    workspace_ids
}

fn graph_hint_nodes(
    connection: &DbConnection,
    graph: &crate::models::QueryGraphHints,
    workspace_ids: &BTreeSet<String>,
    include_tombstoned: bool,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> (BTreeMap<String, GraphHintEvidence>, usize, usize) {
    let mut nodes = BTreeMap::new();
    let mut frontier = BTreeSet::new();
    let mut missing_seeds = 0_usize;
    let mut valid_seeds = Vec::new();
    for seed in &graph.seed_memories {
        if MemoryId::from_str(seed).is_err() {
            missing_seeds = missing_seeds.saturating_add(1);
            push_degradation(
                degraded,
                "context_graph_seed_invalid",
                ContextResponseSeverity::Low,
                format!("Graph seed memory ID '{seed}' is not a valid memory ID."),
                Some("Use full mem_<26-character> memory IDs in graph.seedMemories.".to_string()),
            );
            continue;
        }
        valid_seeds.push(seed.as_str());
    }

    let (seed_memories, _) = load_candidate_batch_maps(connection, &valid_seeds, degraded);
    for seed in &graph.seed_memories {
        if !valid_seeds.contains(&seed.as_str()) {
            continue;
        }
        let Some(seed_memory) = seed_memories.get(seed) else {
            missing_seeds = missing_seeds.saturating_add(1);
            push_degradation(
                degraded,
                "context_graph_seed_missing",
                ContextResponseSeverity::Low,
                format!("Graph seed memory {seed} was not found in the memory store."),
                Some(
                    "Use graph.seedMemories values returned by ee remember/search/why.".to_string(),
                ),
            );
            continue;
        };
        if !workspace_ids.contains(&seed_memory.workspace_id) {
            push_degradation(
                degraded,
                "context_graph_seed_out_of_scope",
                ContextResponseSeverity::Low,
                format!(
                    "Graph seed memory {seed} belongs to workspace {}, outside the active workspace scope.",
                    seed_memory.workspace_id
                ),
                Some("Use graph.seedMemories from the active workspace.".to_string()),
            );
            continue;
        }
        if seed_memory.tombstoned_at.is_some() && !include_tombstoned {
            continue;
        }
        nodes.insert(
            seed.clone(),
            GraphHintEvidence {
                seed_memory_id: seed.clone(),
                depth: 0,
                relation: None,
                traversal: graph.traversal,
            },
        );
        frontier.insert(seed.clone());
    }

    let link_types: BTreeSet<String> = graph.link_types.iter().cloned().collect();
    let direction = graph_neighborhood_direction(graph.traversal);
    let mut traversed_edges = 0_usize;

    for depth in 0..graph.max_hops {
        if frontier.is_empty() {
            break;
        }
        let mut pending_neighbors = BTreeMap::new();
        for memory_id in &frontier {
            let mut options = crate::graph::GraphNeighborhoodOptions::new(memory_id.clone());
            options.direction = direction;
            let report = match crate::graph::graph_neighborhood(connection, &options) {
                Ok(report) => report,
                Err(error) => {
                    push_degradation(
                        degraded,
                        "context_graph_neighborhood_unavailable",
                        ContextResponseSeverity::Low,
                        format!("Graph neighborhood for {memory_id} could not be read: {error}"),
                        Some("ee graph neighborhood <memory-id> --json".to_string()),
                    );
                    continue;
                }
            };
            for edge in report.edges {
                if !link_types.is_empty() && !link_types.contains(&edge.relation) {
                    continue;
                }
                traversed_edges = traversed_edges.saturating_add(1);
                if nodes.contains_key(&edge.neighbor_memory_id) {
                    continue;
                }
                let seed_memory_id = nodes
                    .get(memory_id)
                    .map(|evidence| evidence.seed_memory_id.clone())
                    .unwrap_or_else(|| memory_id.clone());
                pending_neighbors
                    .entry(edge.neighbor_memory_id.clone())
                    .or_insert(GraphHintEvidence {
                        seed_memory_id,
                        depth: depth.saturating_add(1),
                        relation: Some(edge.relation.clone()),
                        traversal: graph.traversal,
                    });
            }
        }

        let pending_refs: Vec<&str> = pending_neighbors.keys().map(String::as_str).collect();
        let (neighbor_memories, _) = load_candidate_batch_maps(connection, &pending_refs, degraded);
        let mut next_frontier = BTreeSet::new();
        for (neighbor_id, evidence) in pending_neighbors {
            let Some(neighbor_memory) = neighbor_memories.get(&neighbor_id) else {
                continue;
            };
            if neighbor_memory.tombstoned_at.is_some() && !include_tombstoned {
                continue;
            }
            if !workspace_ids.contains(&neighbor_memory.workspace_id) {
                push_degradation(
                    degraded,
                    "context_graph_workspace_filtered",
                    ContextResponseSeverity::Low,
                    format!(
                        "Graph neighbor {neighbor_id} belongs to workspace {}, outside the active workspace scope.",
                        neighbor_memory.workspace_id
                    ),
                    Some("Use graph.seedMemories from the active workspace.".to_string()),
                );
                continue;
            }
            if nodes.insert(neighbor_id.clone(), evidence).is_none() {
                next_frontier.insert(neighbor_id);
            }
        }
        frontier = next_frontier;
    }

    (nodes, traversed_edges, missing_seeds)
}

fn graph_neighborhood_direction(
    traversal: crate::models::QueryGraphTraversal,
) -> crate::graph::GraphNeighborhoodDirection {
    match traversal {
        crate::models::QueryGraphTraversal::Outbound => {
            crate::graph::GraphNeighborhoodDirection::Outgoing
        }
        crate::models::QueryGraphTraversal::Inbound => {
            crate::graph::GraphNeighborhoodDirection::Incoming
        }
        crate::models::QueryGraphTraversal::Bidirectional => {
            crate::graph::GraphNeighborhoodDirection::Both
        }
    }
}

fn boost_candidate_for_graph(candidate: &mut PackCandidate, evidence: &GraphHintEvidence) -> bool {
    let current = candidate.relevance.into_inner();
    let boost = match evidence.depth {
        0 => 0.20,
        1 => 0.14,
        2 => 0.09,
        _ => 0.05,
    };
    let floor = match evidence.depth {
        0 => 0.98,
        1 => 0.92,
        2 => 0.86,
        _ => 0.80,
    };
    let boosted = (current + boost).max(floor).min(1.0);
    let Some(score) = unit_score(boosted) else {
        return false;
    };
    if boosted <= current {
        return false;
    }
    candidate.relevance = score;
    candidate.why = format!(
        "{} Graph query-file hint boosted this memory: seed={}, depth={}, traversal={}, relation={}.",
        candidate.why,
        evidence.seed_memory_id,
        evidence.depth,
        evidence.traversal.as_str(),
        evidence.relation.as_deref().unwrap_or("seed")
    );
    true
}

fn graph_memory_matches_filters(
    memory: &StoredMemory,
    tags: &[String],
    filters: &crate::models::QueryFilters,
) -> bool {
    if !filters.filters.is_empty() {
        let reference_time = filters.temporal.as_of.unwrap_or_else(Utc::now);
        let metadata = memory_fallback_metadata(memory, reference_time);
        if !filters.matches(Some(&metadata)) {
            return false;
        }
    }
    if !filters.tags.is_empty() && !filters.matches_tags(tags) {
        return false;
    }
    if !filters.temporal.is_empty()
        && matches!(
            temporal_memory_outcome(memory, &filters.temporal),
            TemporalCandidateOutcome::Exclude
        )
    {
        return false;
    }
    if !filters.trust.is_empty() {
        let posture = posture_for_trust_class(&memory.trust_class);
        if !filters.trust.matches(&memory.trust_class, posture) {
            return false;
        }
    }
    if !filters.redaction.allow_categories.is_empty()
        && !redaction_allow_categories(&memory.content, &filters.redaction)
    {
        return false;
    }
    true
}

fn redaction_allow_categories(content: &str, filters: &crate::models::RedactionFilters) -> bool {
    if filters.allow_categories.is_empty() {
        return true;
    }

    let allowed: BTreeSet<&str> = filters
        .allow_categories
        .iter()
        .map(String::as_str)
        .collect();
    let report = crate::policy::redact_secret_like_content(content);
    report
        .redacted_reasons
        .iter()
        .all(|reason| allowed.contains(reason))
}

fn filter_candidates_by_memory_scope(
    connection: &DbConnection,
    candidates: &mut Vec<PackCandidate>,
    scope_context: &MemoryScopeContext,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> MemoryScopeStats {
    let mut stats = scope_context.stats();
    if candidates.is_empty() {
        return stats;
    }

    if matches!(
        scope_context.scope,
        MemoryScope::Swarm | MemoryScope::Workspace
    ) {
        for _ in candidates.iter() {
            stats.record_candidate(true);
        }
        return stats;
    }

    if matches!(
        scope_context.scope,
        MemoryScope::SelfOnly | MemoryScope::Team
    ) && scope_context.current_agent.is_none()
    {
        push_degradation(
            degraded,
            "scope_agent_unavailable",
            ContextResponseSeverity::Medium,
            format!(
                "Memory scope `{}` needs the current agent identity, but EE_AGENT_NAME is unset.",
                scope_context.scope.as_str()
            ),
            Some("Set EE_AGENT_NAME for self/team scoped retrieval.".to_string()),
        );
    }

    let mut scoped = Vec::with_capacity(candidates.len());
    let mut read_error: Option<String> = None;
    for candidate in std::mem::take(candidates) {
        let memory_id = candidate.memory_id.to_string();
        match connection.get_memory(&memory_id) {
            Ok(Some(memory)) => {
                let in_scope = scope_context.memory_in_scope(&memory);
                stats.record_candidate_id(in_scope, Some(&memory_id));
                if in_scope {
                    scoped.push(candidate);
                }
            }
            Ok(None) => {
                stats.record_candidate_id(false, Some(&memory_id));
            }
            Err(error) => {
                stats.record_candidate_id(false, Some(&memory_id));
                if read_error.is_none() {
                    read_error = Some(error.to_string());
                }
            }
        }
    }

    if let Some(error) = read_error {
        push_degradation(
            degraded,
            "scope_metadata_unavailable",
            ContextResponseSeverity::Medium,
            format!("Context could not verify memory scope against the memory database: {error}"),
            Some("ee doctor --json".to_string()),
        );
    }

    if scope_context.strict_scope && stats.strict_violations > 0 {
        let excluded = stats.strict_violations;
        push_degradation(
            degraded,
            "scope_strict_excluded_evidence",
            ContextResponseSeverity::Medium,
            format!(
                "Strict memory scope `{}` found {excluded} relevant candidate{} outside the requested trust lane; returning no scoped results.",
                scope_context.scope.as_str(),
                plural_suffix(excluded),
            ),
            Some("Retry without --strict-scope or use --memory-scope swarm.".to_string()),
        );
        scoped.clear();
    } else if stats.candidates_excluded_by_scope > 0 {
        let excluded = stats.candidates_excluded_by_scope;
        push_degradation(
            degraded,
            "scope_excluded_evidence",
            ContextResponseSeverity::Low,
            format!(
                "Memory scope `{}` excluded {excluded} candidate{} outside the requested trust lane.",
                scope_context.scope.as_str(),
                plural_suffix(excluded),
            ),
            Some(
                "Use --memory-scope swarm to inspect all candidate evidence, or pass --strict-scope to fail closed."
                    .to_string(),
            ),
        );
    }

    *candidates = scoped;
    stats
}

fn filter_candidates_by_redaction_allow_categories(
    candidates: &mut Vec<PackCandidate>,
    filters: &crate::models::RedactionFilters,
) -> usize {
    if filters.allow_categories.is_empty() {
        return 0;
    }

    let before = candidates.len();
    candidates.retain(|candidate| redaction_allow_categories(&candidate.content, filters));
    before.saturating_sub(candidates.len())
}

fn graph_candidate_from_memory(
    memory: &StoredMemory,
    memory_id: MemoryId,
    tags: &[String],
    evidence: &GraphHintEvidence,
    workspace_path: &Path,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCandidate> {
    let mut provenance = Vec::new();
    if let Some(memory_provenance) =
        provenance_for_memory(memory, memory_id, workspace_path, degraded)
    {
        provenance.push(memory_provenance);
    }
    if let Ok(seed_id) = MemoryId::from_str(&evidence.seed_memory_id)
        && let Ok(graph_provenance) = PackProvenance::new(
            ProvenanceUri::EeMemory(seed_id),
            format!(
                "Graph query-file hint reached {} from seed {} at depth {} via {} traversal.",
                memory.id,
                evidence.seed_memory_id,
                evidence.depth,
                evidence.traversal.as_str()
            ),
        )
    {
        provenance.push(graph_provenance);
    }
    let relevance = graph_expansion_relevance(evidence.depth)?;
    let utility = unit_score(memory.utility)?;
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id,
        section: section_for_memory(memory),
        content: memory.content.clone(),
        estimated_tokens: estimate_tokens_default(&memory.content),
        relevance,
        utility,
        provenance,
        why: format!(
            "Selected by ee.query.v1 graph hint: seed={}, depth={}, traversal={}, relation={}.",
            evidence.seed_memory_id,
            evidence.depth,
            evidence.traversal.as_str(),
            evidence.relation.as_deref().unwrap_or("seed")
        ),
    })
    .ok()?;
    let candidate = candidate
        .with_diversity_key(diversity_key_for_memory(memory, tags))
        .with_trust_signal(trust_signal_for_memory(memory, memory_id, degraded))
        .with_lifecycle(pack_lifecycle_for_memory(memory, None));
    let candidate = match memory.tombstoned_at.as_ref() {
        Some(tombstoned_at) => candidate.with_tombstoned_at(tombstoned_at.clone()),
        None => candidate,
    };
    Some(candidate)
}

fn graph_expansion_relevance(depth: u32) -> Option<UnitScore> {
    let relevance = match depth {
        0 => 0.96,
        1 => 0.90,
        2 => 0.84,
        _ => 0.78,
    };
    unit_score(relevance)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TemporalCandidateOutcome {
    Include,
    Exclude,
    IncludeRelaxedInvalid,
}

fn temporal_memory_outcome(
    memory: &StoredMemory,
    filters: &crate::models::QueryTemporalFilters,
) -> TemporalCandidateOutcome {
    if filters.is_empty() {
        return TemporalCandidateOutcome::Include;
    }

    let Some(created_at) = parse_stored_memory_timestamp(&memory.created_at) else {
        return TemporalCandidateOutcome::Exclude;
    };

    if let Some(after) = filters.after
        && created_at < after
    {
        return TemporalCandidateOutcome::Exclude;
    }
    if let Some(before) = filters.before
        && created_at > before
    {
        return TemporalCandidateOutcome::Exclude;
    }
    if let Some(as_of) = filters.as_of {
        let Some(updated_at) = parse_stored_memory_timestamp(&memory.updated_at) else {
            return TemporalCandidateOutcome::Exclude;
        };
        if created_at > as_of || updated_at > as_of {
            return TemporalCandidateOutcome::Exclude;
        }
    }

    let Some(validity) = &filters.validity else {
        return TemporalCandidateOutcome::Include;
    };
    match validity.posture {
        crate::models::QueryTemporalValidityPosture::Ignore => TemporalCandidateOutcome::Include,
        crate::models::QueryTemporalValidityPosture::Strict => {
            if memory_temporally_invalid_at(
                memory,
                validity
                    .reference_time
                    .or(filters.as_of)
                    .unwrap_or_else(Utc::now),
            ) {
                TemporalCandidateOutcome::Exclude
            } else {
                TemporalCandidateOutcome::Include
            }
        }
        crate::models::QueryTemporalValidityPosture::Relaxed => {
            if memory_temporally_invalid_at(
                memory,
                validity
                    .reference_time
                    .or(filters.as_of)
                    .unwrap_or_else(Utc::now),
            ) {
                TemporalCandidateOutcome::IncludeRelaxedInvalid
            } else {
                TemporalCandidateOutcome::Include
            }
        }
    }
}

fn memory_temporally_invalid_at(memory: &StoredMemory, reference_time: DateTime<Utc>) -> bool {
    if let Some(valid_from) = memory.valid_from.as_deref() {
        let Some(valid_from) = parse_stored_memory_timestamp(valid_from) else {
            return true;
        };
        if valid_from > reference_time {
            return true;
        }
    }
    if let Some(valid_to) = memory.valid_to.as_deref() {
        let Some(valid_to) = parse_stored_memory_timestamp(valid_to) else {
            return true;
        };
        if valid_to < reference_time {
            return true;
        }
    }
    false
}

fn parse_stored_memory_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
}

fn pack_lifecycle_for_memory(
    memory: &StoredMemory,
    reference_time: Option<DateTime<Utc>>,
) -> PackItemLifecycle {
    let reference_time = reference_time.unwrap_or_else(Utc::now);
    PackItemLifecycle {
        validity_status: validity_status_for_memory(memory, reference_time).to_owned(),
        validity_window_kind: validity_window_kind(
            memory.valid_from.as_deref(),
            memory.valid_to.as_deref(),
        )
        .to_owned(),
        valid_from: memory.valid_from.clone(),
        valid_to: memory.valid_to.clone(),
    }
}

fn validity_status_for_memory(
    memory: &StoredMemory,
    reference_time: DateTime<Utc>,
) -> &'static str {
    let valid_from = match memory.valid_from.as_deref() {
        Some(raw) => match parse_stored_memory_timestamp(raw) {
            Some(timestamp) => Some(timestamp),
            None => return "malformed",
        },
        None => None,
    };
    let valid_to = match memory.valid_to.as_deref() {
        Some(raw) => match parse_stored_memory_timestamp(raw) {
            Some(timestamp) => Some(timestamp),
            None => return "malformed",
        },
        None => None,
    };

    if valid_from.is_none() && valid_to.is_none() {
        "unknown"
    } else if valid_from.is_some_and(|timestamp| timestamp > reference_time) {
        "future"
    } else if valid_to.is_some_and(|timestamp| timestamp < reference_time) {
        "expired"
    } else {
        "current"
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

fn load_candidate_batch_maps(
    connection: &DbConnection,
    memory_ids: &[&str],
    degraded: &mut Vec<ContextResponseDegradation>,
) -> (
    BTreeMap<String, StoredMemory>,
    BTreeMap<String, Vec<String>>,
) {
    if memory_ids.is_empty() {
        return (BTreeMap::new(), BTreeMap::new());
    }

    let memories = match connection.get_memories_batch(memory_ids) {
        Ok(memories) => memories,
        Err(error) => {
            push_degradation(
                degraded,
                "context_candidate_memory_batch_unavailable",
                ContextResponseSeverity::Medium,
                format!("Context candidate memories could not be batch-loaded: {error}"),
                Some("ee status --json".to_string()),
            );
            BTreeMap::new()
        }
    };

    let tags_map = match connection.get_memory_tags_batch(memory_ids) {
        Ok(tags_map) => tags_map,
        Err(error) => {
            push_degradation(
                degraded,
                "context_candidate_tags_batch_unavailable",
                ContextResponseSeverity::Medium,
                format!("Context candidate memory tags could not be batch-loaded: {error}"),
                Some("ee status --json".to_string()),
            );
            BTreeMap::new()
        }
    };

    (memories, tags_map)
}

struct PreloadedCandidateSource<'a> {
    memories: &'a BTreeMap<String, StoredMemory>,
    tags_map: &'a BTreeMap<String, Vec<String>>,
    workspace_path: &'a Path,
    query: &'a str,
    validity_reference_time: Option<DateTime<Utc>>,
    include_tombstoned: bool,
}

struct FocusCandidateSource<'a> {
    connection: &'a DbConnection,
    focus_state: &'a crate::models::FocusState,
    workspace_path: &'a Path,
    focus_hash: &'a str,
    storage_path: &'a str,
    include_tombstoned: bool,
}

fn candidate_from_hit_preloaded(
    source: PreloadedCandidateSource<'_>,
    hit: &crate::core::search::SearchHit,
    memory_id: MemoryId,
    artifact_id: Option<String>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCandidate> {
    let memory = match source.memories.get(&memory_id.to_string()) {
        Some(memory) if memory.tombstoned_at.is_none() => memory,
        Some(memory) if source.include_tombstoned => memory,
        _ => return None,
    };
    let tags = source.tags_map.get(&memory.id).cloned().unwrap_or_default();
    let provenance = provenance_for_memory(memory, memory_id, source.workspace_path, degraded)?;
    let relevance = unit_score(hit.score)?;
    let utility = unit_score(memory.utility)?;
    let content = memory.content.clone();
    let why = candidate_selection_why(
        source.query,
        hit.source.as_str(),
        hit.score,
        memory.utility,
        artifact_id.as_deref(),
    );
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id,
        section: section_for_memory(memory),
        content,
        estimated_tokens: estimate_tokens_default(&memory.content),
        relevance,
        utility,
        provenance: vec![provenance],
        why,
    })
    .ok()?;

    let candidate = candidate
        .with_diversity_key(diversity_key_for_memory(memory, &tags))
        .with_trust_signal(trust_signal_for_memory(memory, memory_id, degraded))
        .with_lifecycle(pack_lifecycle_for_memory(
            memory,
            source.validity_reference_time,
        ));
    let candidate = match memory.tombstoned_at.as_ref() {
        Some(tombstoned_at) => candidate.with_tombstoned_at(tombstoned_at.clone()),
        None => candidate,
    };
    Some(candidate)
}

fn focus_candidates_from_state(
    connection: &DbConnection,
    workspace_path: &Path,
    focus_state: &crate::models::FocusState,
    include_tombstoned: bool,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<PackCandidate> {
    let mut candidates = Vec::new();
    let focus_hash = focus_state_hash(focus_state);
    let storage_path = focus_state_path(workspace_path).display().to_string();
    let source = FocusCandidateSource {
        connection,
        focus_state,
        workspace_path,
        focus_hash: &focus_hash,
        storage_path: &storage_path,
        include_tombstoned,
    };
    for item in &focus_state.items {
        match focus_candidate_from_item(&source, item, degraded) {
            Some(candidate) => candidates.push(candidate),
            None => push_degradation(
                degraded,
                "context_focus_candidate_skipped",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} could not be converted into a pack candidate.",
                    item.memory_id
                ),
                Some(format!("ee focus remove {} --json", item.memory_id)),
            ),
        }
    }
    candidates
}

fn focus_candidate_from_item(
    source: &FocusCandidateSource<'_>,
    item: &crate::models::FocusItem,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCandidate> {
    let memory = match source.connection.get_memory(&item.memory_id.to_string()) {
        Ok(Some(memory)) if memory.tombstoned_at.is_none() => memory,
        Ok(Some(memory)) if source.include_tombstoned => memory,
        Ok(Some(_)) => {
            push_degradation(
                degraded,
                "context_focus_tombstoned_memory",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} is tombstoned and was excluded from context.",
                    item.memory_id
                ),
                Some(format!("ee focus remove {} --json", item.memory_id)),
            );
            return None;
        }
        Ok(None) => {
            push_degradation(
                degraded,
                "context_focus_missing_memory",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} is missing and was excluded from context.",
                    item.memory_id
                ),
                Some(format!("ee focus remove {} --json", item.memory_id)),
            );
            return None;
        }
        Err(error) => {
            push_degradation(
                degraded,
                "context_focus_memory_lookup_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Focused memory {} could not be loaded: {error}",
                    item.memory_id
                ),
                Some("ee status --json".to_string()),
            );
            return None;
        }
    };
    let tags = source
        .connection
        .get_memory_tags(&memory.id)
        .unwrap_or_else(|_| Vec::new());
    let mut provenance = Vec::new();
    if let Some(memory_provenance) =
        provenance_for_memory(&memory, item.memory_id, source.workspace_path, degraded)
    {
        provenance.push(memory_provenance);
    }
    if let Ok(focus_provenance) = PackProvenance::new(
        ProvenanceUri::File {
            path: source.storage_path.to_owned(),
            span: None,
        },
        format!(
            "Passive focus state {} included memory {}; reason={}; provenance={}",
            source.focus_hash,
            item.memory_id,
            item.reason,
            item.provenance.join(",")
        ),
    ) {
        provenance.push(focus_provenance);
    }
    let relevance = focus_relevance(item, source.focus_state)?;
    let utility = unit_score(memory.utility.max(0.75))?;
    let why = focus_candidate_why(item, source.focus_state, source.focus_hash);
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id: item.memory_id,
        section: section_for_memory(&memory),
        content: memory.content.clone(),
        estimated_tokens: estimate_tokens_default(&memory.content),
        relevance,
        utility,
        provenance,
        why,
    })
    .ok()?;

    let candidate = candidate
        .with_diversity_key(diversity_key_for_memory(&memory, &tags))
        .with_trust_signal(trust_signal_for_memory(&memory, item.memory_id, degraded))
        .with_lifecycle(pack_lifecycle_for_memory(&memory, None));
    let candidate = match memory.tombstoned_at.as_ref() {
        Some(tombstoned_at) => candidate.with_tombstoned_at(tombstoned_at.clone()),
        None => candidate,
    };
    Some(candidate)
}

fn focus_relevance(
    item: &crate::models::FocusItem,
    focus_state: &crate::models::FocusState,
) -> Option<UnitScore> {
    let value = if focus_state.focal_memory_id == Some(item.memory_id) {
        1.0
    } else if item.pinned {
        0.97
    } else {
        0.94
    };
    unit_score(value)
}

fn focus_candidate_why(
    item: &crate::models::FocusItem,
    focus_state: &crate::models::FocusState,
    focus_hash: &str,
) -> String {
    format!(
        "Selected as passive active-memory input: focus_state_hash={focus_hash}; focal={}; pinned={}; capacity={}; reason={}; provenance={}; source=ee_focus_state; no hidden mutation or agent-plan inference occurred.",
        focus_state.focal_memory_id == Some(item.memory_id),
        item.pinned,
        focus_state.capacity,
        item.reason,
        item.provenance.join(",")
    )
}

/// Generate a per-item `why` string for a context pack candidate.
///
/// Bead bd-17c65.1.3 (A3) — replaced the previous 350-character math-
/// identity boilerplate ("Deterministic retrieval explanation for query
/// `...`: source=memory search_source=...; score_components=[relevance=
/// unit_score(search_hit.score)...]; formula=unit_score(field)=clamp(...);
/// inputs are stored memory/link fields and the explicit search hit,
/// not agent reasoning.") with a one-line actionable reason.
///
/// The old form was byte-identical across all items in a pack except
/// for the score number — 350 chars × 13 items = 4.5KB of pure
/// repetition. The new form retains the same information per item
/// (query, source, score, utility, artifact provenance) in a compact
/// shape an LLM agent can read at a glance:
///
///   matched 'query' via <source> (relevance <score>, utility <util>)
///   matched 'query' via <source> (relevance <score>, utility <util>); via artifact <id>
///
/// The math identity (`unit_score(field) = clamp(field, 0.0, 1.0)`)
/// applies to every item identically and lives in the pack-level
/// `pack.meta.algorithm.scoringFormula`, not repeated per item.
fn candidate_selection_why(
    query: &str,
    search_source: &str,
    search_score: f32,
    memory_utility: f32,
    artifact_id: Option<&str>,
) -> String {
    // Trim the query for readability; over-long queries get the
    // characteristic "..." truncation so the why line stays short.
    let display_query = if query.chars().count() > 80 {
        let mut truncated: String = query.chars().take(77).collect();
        truncated.push_str("...");
        truncated
    } else {
        query.to_owned()
    };

    let base = format!(
        "matched '{display_query}' via {search_source} (relevance {search_score:.4}, utility {memory_utility:.4})",
    );
    if let Some(artifact_id) = artifact_id {
        format!("{base}; via registered artifact {artifact_id}")
    } else {
        base
    }
}

fn artifact_linked_memory_id(
    connection: &DbConnection,
    hit: &crate::core::search::SearchHit,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<(MemoryId, Option<String>)> {
    let has_artifact_metadata = hit
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("source"))
        .and_then(serde_json::Value::as_str)
        == Some("artifact");
    if !has_artifact_metadata && !is_registered_artifact_hit(connection, &hit.doc_id, degraded) {
        return None;
    }

    let links = match connection.list_artifact_links(&hit.doc_id) {
        Ok(links) => links,
        Err(error) => {
            push_degradation(
                degraded,
                "context_artifact_links_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "Artifact links for {} could not be loaded: {error}",
                    hit.doc_id
                ),
                Some(format!("ee artifact inspect {} --json", hit.doc_id)),
            );
            return None;
        }
    };

    for link in links {
        if link.target_type != "memory" {
            continue;
        }
        match MemoryId::from_str(&link.target_id) {
            Ok(memory_id) => return Some((memory_id, Some(hit.doc_id.clone()))),
            Err(error) => push_degradation(
                degraded,
                "context_artifact_memory_link_invalid",
                ContextResponseSeverity::Low,
                format!(
                    "Artifact {} links to invalid memory id `{}`: {error}",
                    hit.doc_id, link.target_id
                ),
                Some(format!("ee artifact inspect {} --json", hit.doc_id)),
            ),
        }
    }

    push_degradation(
        degraded,
        "context_artifact_unlinked",
        ContextResponseSeverity::Low,
        format!(
            "Artifact {} matched search but has no valid memory link for context packing.",
            hit.doc_id
        ),
        Some("ee artifact register <path> --link-memory <memory-id> --json".to_string()),
    );
    None
}

fn is_registered_artifact_hit(
    connection: &DbConnection,
    artifact_id: &str,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> bool {
    if !is_registry_artifact_id(artifact_id) {
        return false;
    }

    match connection.get_artifact(artifact_id) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            push_degradation(
                degraded,
                "context_artifact_lookup_unavailable",
                ContextResponseSeverity::Low,
                format!("Artifact {artifact_id} could not be loaded: {error}"),
                Some(format!("ee artifact inspect {artifact_id} --json")),
            );
            false
        }
    }
}

fn is_registry_artifact_id(value: &str) -> bool {
    value.len() == 30
        && value.starts_with("art_")
        && value.strip_prefix("art_").is_some_and(|suffix| {
            suffix
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

fn trust_signal_for_memory(
    memory: &StoredMemory,
    memory_id: MemoryId,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> PackTrustSignal {
    let trust_class = match TrustClass::from_str(&memory.trust_class) {
        Ok(class) => class,
        Err(error) => {
            push_degradation(
                degraded,
                "context_invalid_trust_class",
                ContextResponseSeverity::Medium,
                format!(
                    "Memory {} has invalid trust class `{}`: {error}",
                    memory.id, memory.trust_class
                ),
                Some(format!("ee memory show {memory_id} --json")),
            );
            TrustClass::AgentAssertion
        }
    };
    PackTrustSignal::new(trust_class, memory.trust_subclass.clone())
}

fn provenance_for_memory(
    memory: &StoredMemory,
    memory_id: MemoryId,
    workspace_path: &Path,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackProvenance> {
    let uri = match memory.provenance_uri.as_deref() {
        Some(raw) => match ProvenanceUri::from_str(raw) {
            Ok(uri) => uri,
            Err(error) => {
                push_degradation(
                    degraded,
                    "context_invalid_provenance",
                    ContextResponseSeverity::Low,
                    format!("Memory {} has invalid provenance URI: {error}", memory.id),
                    Some(format!("ee memory show {} --json", memory.id)),
                );
                ProvenanceUri::EeMemory(memory_id)
            }
        },
        None => ProvenanceUri::EeMemory(memory_id),
    };
    let freshness =
        crate::core::memory::assess_memory_evidence_freshness(memory, Some(workspace_path));
    if freshness.status.should_report() {
        push_evidence_freshness_degradation(memory, &freshness, degraded);
    }
    let note = format!(
        "Memory {} selected for context pack; evidenceFreshness={}",
        memory.id,
        freshness.status.as_str()
    );

    PackProvenance::new(uri, note).ok()
}

fn push_evidence_freshness_degradation(
    memory: &StoredMemory,
    freshness: &crate::core::memory::EvidenceFreshness,
    degraded: &mut Vec<ContextResponseDegradation>,
) {
    let code = match freshness.status {
        crate::core::memory::EvidenceFreshnessStatus::MissingSource => {
            "context_evidence_freshness_missing_source"
        }
        crate::core::memory::EvidenceFreshnessStatus::ChangedSource => {
            "context_evidence_freshness_changed_source"
        }
        crate::core::memory::EvidenceFreshnessStatus::UnreachableSource => {
            "context_evidence_freshness_unreachable_source"
        }
        crate::core::memory::EvidenceFreshnessStatus::UnsupportedSource => {
            "context_evidence_freshness_unsupported_source"
        }
        crate::core::memory::EvidenceFreshnessStatus::Fresh
        | crate::core::memory::EvidenceFreshnessStatus::Unknown => return,
    };
    push_degradation(
        degraded,
        code,
        ContextResponseSeverity::Low,
        format!(
            "Memory {} evidence freshness is {}: {}",
            memory.id,
            freshness.status.as_str(),
            freshness.detail
        ),
        freshness.repair.clone(),
    );
}

fn section_for_memory(memory: &StoredMemory) -> PackSection {
    match (memory.level.as_str(), memory.kind.as_str()) {
        ("procedural", _) | (_, "rule" | "convention" | "playbook-step") => {
            PackSection::ProceduralRules
        }
        (_, "decision") => PackSection::Decisions,
        (_, "failure" | "anti-pattern" | "risk") => PackSection::Failures,
        ("episodic", _) => PackSection::Evidence,
        _ => PackSection::Artifacts,
    }
}

fn diversity_key_for_memory(memory: &StoredMemory, tags: &[String]) -> String {
    let tag = tags.first().map_or("untagged", String::as_str);
    format!("{}:{}:{}", memory.level, memory.kind, tag)
}

fn unit_score(value: f32) -> Option<UnitScore> {
    let bounded = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    UnitScore::parse(bounded).ok()
}

fn push_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    code: &str,
    severity: ContextResponseSeverity,
    message: impl Into<String>,
    repair: Option<String>,
) {
    if let Ok(entry) = ContextResponseDegradation::new(code, severity, message, repair) {
        degraded.push(entry);
    }
}

fn push_consensus_conflict_degradations(
    degraded: &mut Vec<ContextResponseDegradation>,
    report: &ConsensusConflictReport,
    selected_count: usize,
) {
    if selected_count == 0 && report.consensus.is_empty() && report.conflicts.is_empty() {
        push_degradation(
            degraded,
            "consensus_no_clusters",
            ContextResponseSeverity::Low,
            "Context pack did not contain enough query-relevant neighboring memories to surface consensus clusters.",
            Some(
                "Broaden the query, increase --candidate-pool, or add tagged memories for this subject."
                    .to_string(),
            ),
        );
    }

    if report
        .conflicts
        .iter()
        .any(|conflict| conflict.kind == ConflictKind::Direct)
    {
        push_degradation(
            degraded,
            "conflict_direct",
            ContextResponseSeverity::Medium,
            "Context pack contains query-relevant memories with directly conflicting claims.",
            Some("Review the conflicting memory IDs before acting on either claim.".to_string()),
        );
    }

    if report
        .conflicts
        .iter()
        .any(|conflict| conflict.recommended_action == ConflictRecommendedAction::PromoteOne)
    {
        push_degradation(
            degraded,
            "conflict_trust_mismatch",
            ContextResponseSeverity::High,
            "Context pack contains a trust mismatch conflict where a higher-trust memory should be preferred over an unvalidated assertion.",
            Some(
                "Promote the higher-trust memory only after reviewing its provenance.".to_string(),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{
        AccessLevel, CandidateResolutionMetrics, CapabilitySet, CommandContext,
        ContextPerformanceTrace, PackSlotAcquisition, PerformanceTiming, candidate_selection_why,
        context_performance_json, focus_candidate_why, focus_relevance, pack_assembly_slo_for_run,
        try_acquire_pack_slot, unit_score,
    };
    use crate::config::{ReadPoolConfig, WorkspaceLocation};
    use crate::core::budget::RequestBudget;
    use crate::core::memory::{ReviseMemoryOptions, ReviseReason, revise_memory};
    use crate::core::profile::{OperatingProfile, RuntimeProfileReport};
    use crate::core::search::{
        PERFORMANCE_EXPLAIN_SCHEMA_V1, ScoreSource, SearchHit, SearchReport, SearchStatus,
    };
    use crate::db::read_pool::{PoolConfig, ReadConnectionPool};
    use crate::db::{
        CreateMemoryInput, CreateWorkspaceInput, DatabaseConfig, DbConnection,
        StoredAgentContextProfileForPack, StoredMemory,
    };
    use crate::models::{
        AgentContextProfileCounts, FocusItem, FocusState, MemoryId, MemoryScope, MemoryScopeStats,
        ProvenanceUri, QueryTemporalFilters, QueryTemporalValidity, QueryTemporalValidityPosture,
        TrustClass, UnitScore, WorkspaceId,
    };
    use crate::pack::{
        ContextPackProfile, ContextRequest, ContextRequestInput, ContextResponseSeverity,
        PackCandidate, PackCandidateInput, PackProvenance, PackResourceProfile, PackSection,
        TokenBudget, assemble_draft_with_profile,
    };

    fn workspace_at(root: &str) -> WorkspaceLocation {
        WorkspaceLocation::new(PathBuf::from(root))
    }

    fn ctx(caps: CapabilitySet) -> CommandContext {
        CommandContext::new(
            workspace_at("/tmp/ee-test-workspace"),
            RequestBudget::unbounded(),
            caps,
        )
    }

    fn test_runtime_profile() -> RuntimeProfileReport {
        RuntimeProfileReport::for_profile(OperatingProfile::Workstation, "test_fixture")
    }

    #[test]
    fn pack_slot_guard_enforces_lean_profile_limit() -> Result<(), String> {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;

        let first = match try_acquire_pack_slot(workspace.path(), PackResourceProfile::Lean) {
            PackSlotAcquisition::Acquired(guard) => guard,
            other => {
                return Err(format!(
                    "first lean pack slot should be acquired: {other:?}"
                ));
            }
        };

        match try_acquire_pack_slot(workspace.path(), PackResourceProfile::Lean) {
            PackSlotAcquisition::LimitReached { retry_after_ms } => {
                assert_eq!(retry_after_ms, super::PACK_SLOT_RETRY_AFTER_MS);
            }
            other => {
                return Err(format!(
                    "second lean pack slot should be limited: {other:?}"
                ));
            }
        }

        drop(first);

        match try_acquire_pack_slot(workspace.path(), PackResourceProfile::Lean) {
            PackSlotAcquisition::Acquired(_guard) => Ok(()),
            other => Err(format!(
                "lean pack slot should be available after guard drop: {other:?}"
            )),
        }
    }

    #[cfg(unix)]
    #[test]
    fn pack_slot_guard_rejects_symlinked_metadata_parent() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let real_metadata = tempdir.path().join("real-ee");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&real_metadata).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&real_metadata, workspace.join(".ee"))
            .map_err(|error| error.to_string())?;

        match try_acquire_pack_slot(&workspace, PackResourceProfile::Lean) {
            PackSlotAcquisition::Unavailable { message, .. } => {
                assert!(
                    message.contains("symbolic link"),
                    "expected symlink rejection, got: {message}"
                );
                assert!(
                    !real_metadata.join("pack-slots").exists(),
                    "pack slot creation must not follow symlinked .ee parent"
                );
                Ok(())
            }
            other => Err(format!(
                "symlinked .ee parent should make pack slot unavailable: {other:?}"
            )),
        }
    }

    #[cfg(unix)]
    #[test]
    fn pack_slot_guard_rejects_symlinked_lock_file() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let slots_dir = workspace.join(".ee").join("pack-slots");
        std::fs::create_dir_all(&slots_dir).map_err(|error| error.to_string())?;
        let outside_lock = tempdir.path().join("outside.lock");
        std::fs::write(&outside_lock, b"outside").map_err(|error| error.to_string())?;
        let slot_path = slots_dir.join(format!("{}-00.lock", PackResourceProfile::Lean.as_str()));
        std::os::unix::fs::symlink(&outside_lock, &slot_path).map_err(|error| error.to_string())?;

        match try_acquire_pack_slot(&workspace, PackResourceProfile::Lean) {
            PackSlotAcquisition::Unavailable { message, .. } => {
                assert!(
                    message.contains("symbolic link"),
                    "expected symlink rejection, got: {message}"
                );
                let outside =
                    std::fs::read_to_string(&outside_lock).map_err(|error| error.to_string())?;
                assert_eq!(
                    outside, "outside",
                    "pack slot lock open must not follow or mutate symlink target"
                );
                Ok(())
            }
            other => Err(format!(
                "symlinked pack slot lock should be unavailable: {other:?}"
            )),
        }
    }

    fn context_options_with_coordination_snapshot(path: PathBuf) -> super::ContextPackOptions {
        super::ContextPackOptions {
            workspace_path: PathBuf::from("/tmp/ee-context-coordination-test"),
            database_path: None,
            index_dir: None,
            query: "coordinate safely".to_owned(),
            speed: crate::search::SpeedMode::Default,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(400),
            candidate_pool: Some(10),
            max_results: None,
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            pagination: None,
            coordination_snapshot_path: Some(path),
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            output_options: Default::default(),
        }
    }

    #[test]
    fn coordination_snapshot_rejects_non_regular_path() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let snapshot_path = tempdir.path().join("coordination-snapshot.json");
        std::fs::create_dir(&snapshot_path).map_err(|error| error.to_string())?;
        let options = context_options_with_coordination_snapshot(snapshot_path);
        let mut degraded = Vec::new();

        let snapshot = super::load_coordination_snapshot(&options, &mut degraded);

        assert!(snapshot.is_none());
        let degradation = degraded
            .iter()
            .find(|entry| entry.code == "coordination_snapshot_unavailable")
            .ok_or_else(|| "missing coordination snapshot degradation".to_string())?;
        assert!(
            degradation.message.contains("not a regular file"),
            "expected non-regular path degradation, got: {}",
            degradation.message
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn coordination_snapshot_rejects_symlinked_path() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_snapshot = tempdir.path().join("outside-coordination.json");
        std::fs::write(&outside_snapshot, "{not valid json").map_err(|error| error.to_string())?;
        let snapshot_path = tempdir.path().join("coordination-snapshot.json");
        std::os::unix::fs::symlink(&outside_snapshot, &snapshot_path)
            .map_err(|error| error.to_string())?;
        let options = context_options_with_coordination_snapshot(snapshot_path);
        let mut degraded = Vec::new();

        let snapshot = super::load_coordination_snapshot(&options, &mut degraded);

        assert!(snapshot.is_none());
        let degradation = degraded
            .iter()
            .find(|entry| entry.code == "coordination_snapshot_unavailable")
            .ok_or_else(|| "missing coordination snapshot degradation".to_string())?;
        assert!(
            degradation.message.contains("symbolic link"),
            "expected symlink path degradation, got: {}",
            degradation.message
        );
        Ok(())
    }

    struct PprContextFixture {
        connection: DbConnection,
        workspace_path: PathBuf,
        seed: MemoryId,
        neighbor: MemoryId,
        orphan: MemoryId,
    }

    fn ppr_context_fixture(
        snapshot_status: crate::db::GraphSnapshotStatus,
    ) -> Result<PprContextFixture, String> {
        use crate::db::{
            CreateGraphSnapshotInput, CreateMemoryLinkInput, GraphSnapshotType, MemoryLinkRelation,
            MemoryLinkSource,
        };

        let temp_root = PathBuf::from("/tmp");
        let tempdir = tempfile::Builder::new()
            .prefix("ee-context-ppr-")
            .tempdir_in(&temp_root)
            .or_else(|_| {
                let cwd = std::env::current_dir()?;
                tempfile::Builder::new()
                    .prefix("ee-context-ppr-")
                    .tempdir_in(cwd)
            })
            .map_err(|error| error.to_string())?;
        let workspace_path = tempdir.keep();
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(900)).to_string();
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("context ppr fixture".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let seed = MemoryId::from_uuid(uuid::Uuid::from_u128(901));
        let neighbor = MemoryId::from_uuid(uuid::Uuid::from_u128(902));
        let orphan = MemoryId::from_uuid(uuid::Uuid::from_u128(903));
        for (memory_id, content) in [
            (seed, "Seed memory for release checks."),
            (neighbor, "Neighbor memory linked by the graph."),
            (orphan, "Orphan memory with no graph edge."),
        ] {
            connection
                .insert_memory(
                    &memory_id.to_string(),
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_string(),
                        kind: "rule".to_string(),
                        content: content.to_string(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        provenance_uri: None,
                        trust_class: TrustClass::AgentAssertion.as_str().to_string(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        connection
            .insert_memory_link(
                "link_00000000000000000000000901",
                &CreateMemoryLinkInput {
                    src_memory_id: seed.to_string(),
                    dst_memory_id: neighbor.to_string(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("context-ppr-test".to_string()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                "gsnap_0000000000000000000000901",
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_id.clone(),
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot.v1".to_string(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 3,
                    edge_count: 1,
                    metrics_json: "{}".to_string(),
                    content_hash: "blake3:context-ppr".to_string(),
                    source_generation: 1,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        if snapshot_status != crate::db::GraphSnapshotStatus::Valid {
            connection
                .update_graph_snapshot_status("gsnap_0000000000000000000000901", snapshot_status)
                .map_err(|error| error.to_string())?;
        }

        Ok(PprContextFixture {
            connection,
            workspace_path,
            seed,
            neighbor,
            orphan,
        })
    }

    fn mesh_link_metadata(
        workspace_scope_decision: &str,
        material_lane: &str,
        complete: bool,
    ) -> String {
        let mut mesh = serde_json::json!({
            "workspaceScopeDecision": workspace_scope_decision,
            "workspaceId": "wsp_local_alpha",
            "cachedMaterialId": "mesh_context_link_123",
            "originWorkspaceId": "wsp_remote_beta",
            "originWorkspaceLabel": "/Users/alice/private/repo",
            "producerPeerId": "peer_builder_one",
            "producerPeerLabel": "/Users/alice/private/peer-agent",
            "materialLane": material_lane,
            "importDecisionId": "mesh_dec_456",
            "trustLane": "mesh_metadata",
            "redactionPosture": "standard"
        });
        if !complete && let Some(object) = mesh.as_object_mut() {
            object.remove("trustLane");
        }
        serde_json::json!({ "mesh": mesh }).to_string()
    }

    fn ppr_candidate(memory_id: MemoryId, relevance: f32) -> Result<PackCandidate, String> {
        let provenance =
            PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "context ppr fixture")
                .map_err(|error| error.to_string())?;
        PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: format!("candidate {memory_id}"),
            estimated_tokens: 8,
            relevance: UnitScore::parse(relevance).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            provenance: vec![provenance],
            why: "selected by fixture".to_string(),
        })
        .map_err(|error| error.to_string())
    }

    fn stored_agent_profile(
        _agent_name: &str,
        memory_id: MemoryId,
        counts: AgentContextProfileCounts,
    ) -> StoredAgentContextProfileForPack {
        StoredAgentContextProfileForPack {
            memory_id: memory_id.to_string(),
            counts,
            last_seen_at: "2026-05-16T01:12:00Z".to_string(),
            weight_cached: counts.bias().weight,
        }
    }

    #[test]
    fn agent_context_profile_bias_is_capped_and_deterministic() -> Result<(), String> {
        let boosted = MemoryId::from_uuid(uuid::Uuid::from_u128(920));
        let neutral = MemoryId::from_uuid(uuid::Uuid::from_u128(921));
        let mut candidates = vec![ppr_candidate(boosted, 0.50)?, ppr_candidate(neutral, 0.51)?];
        let summary = super::summarize_agent_context_profiles(
            "FrostyMoose",
            "wsp_01234567890123456789012345",
            vec![stored_agent_profile(
                "FrostyMoose",
                boosted,
                AgentContextProfileCounts::new(100, 0, 0),
            )],
            &mut candidates,
        );

        assert_eq!(summary.memory_bias_applied, 1);
        assert!(!summary.cold_start);
        assert!(summary.bias_magnitude <= crate::models::AGENT_PROFILE_BIAS_CAP);
        assert!(
            candidates[0].relevance.into_inner() <= 0.55,
            "profile bias must stay within +0.05"
        );
        super::sort_context_candidates(&mut candidates);
        assert_eq!(candidates[0].memory_id, boosted);

        let json = summary.into_json();
        assert_eq!(
            json["schema"],
            crate::models::AGENT_CONTEXT_PROFILE_SCHEMA_V1
        );
        assert_eq!(json["memoryBiasApplied"], 1);
        assert_eq!(json["coldStart"], false);
        assert_eq!(json["topBiases"][0]["memoryId"], boosted.to_string());
        Ok(())
    }

    #[test]
    fn agent_context_profile_cold_start_does_not_change_ranking() -> Result<(), String> {
        let cold = MemoryId::from_uuid(uuid::Uuid::from_u128(922));
        let winner = MemoryId::from_uuid(uuid::Uuid::from_u128(923));
        let mut candidates = vec![ppr_candidate(cold, 0.50)?, ppr_candidate(winner, 0.51)?];
        let before = candidates
            .iter()
            .map(|candidate| candidate.relevance.into_inner())
            .collect::<Vec<_>>();
        let summary = super::summarize_agent_context_profiles(
            "FrostyMoose",
            "wsp_01234567890123456789012345",
            vec![stored_agent_profile(
                "FrostyMoose",
                cold,
                AgentContextProfileCounts::new(9, 0, 0),
            )],
            &mut candidates,
        );
        let after = candidates
            .iter()
            .map(|candidate| candidate.relevance.into_inner())
            .collect::<Vec<_>>();

        assert_eq!(before, after);
        assert_eq!(summary.memory_bias_applied, 0);
        assert!(summary.cold_start);
        super::sort_context_candidates(&mut candidates);
        assert_eq!(candidates[0].memory_id, winner);
        Ok(())
    }

    fn ppr_search_report(hits: Vec<SearchHit>) -> SearchReport {
        SearchReport {
            status: SearchStatus::Success,
            query: "release graph".to_string(),
            requested_limit: hits.len() as u32,
            results: hits,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        }
    }

    fn ppr_hit(memory_id: MemoryId, score: f32, lexical_score: Option<f32>) -> SearchHit {
        SearchHit {
            doc_id: memory_id.to_string(),
            score,
            source: if lexical_score.is_some() {
                ScoreSource::Hybrid
            } else {
                ScoreSource::SemanticFast
            },
            fast_score: Some(score),
            quality_score: None,
            lexical_score,
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    fn enable_context_ppr_feature(workspace_path: &Path) -> Result<(), String> {
        let config_dir = workspace_path.join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            config_dir.join("config.toml"),
            "[graph.feature.ppr]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())
    }

    fn enable_context_proximity_feature(workspace_path: &Path) -> Result<(), String> {
        let config_dir = workspace_path.join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            config_dir.join("config.toml"),
            "[graph.feature.proximity]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())
    }

    fn context_response_with_pack_item(
        memory_id: MemoryId,
    ) -> Result<crate::pack::ContextResponse, String> {
        let request = ContextRequest::new(ContextRequestInput {
            query: "pack dna disabled contract".to_string(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(64),
            candidate_pool: Some(1),
            max_results: None,
            sections: Vec::new(),
        })
        .map_err(|error| error.to_string())?;
        let draft = assemble_draft_with_profile(
            request.profile,
            request.query.clone(),
            TokenBudget::new(64).map_err(|error| error.to_string())?,
            [ppr_candidate(memory_id, 0.80)?],
        )
        .map_err(|error| error.to_string())?;
        crate::pack::ContextResponse::new(request, draft, Vec::new())
            .map_err(|error| error.to_string())
    }

    #[test]
    fn context_ppr_weight_defaults_to_disabled() {
        assert_eq!(super::effective_context_ppr_weight(None), 0.0);
        assert_eq!(super::effective_context_ppr_weight(Some(0.75)), 0.75);
        assert_eq!(super::effective_context_ppr_weight(Some(2.0)), 1.0);
        assert_eq!(
            super::effective_context_ppr_weight(Some(f32::NAN)),
            super::DEFAULT_CONTEXT_PPR_WEIGHT
        );
    }

    #[test]
    fn context_ppr_rerank_fires_with_valid_snapshot() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        enable_context_ppr_feature(&fixture.workspace_path)?;
        let mut candidates = vec![
            ppr_candidate(fixture.seed, 0.80)?,
            ppr_candidate(fixture.neighbor, 0.20)?,
            ppr_candidate(fixture.orphan, 0.60)?,
        ];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            super::DEFAULT_CONTEXT_PPR_WEIGHT,
            &mut degraded,
        );

        assert_eq!(metrics.reranked_candidates, 3);
        assert!(
            degraded.is_empty(),
            "valid snapshot should not degrade: {degraded:?}"
        );
        assert!(candidates[1].relevance.into_inner() > 0.20);
        let score_breakdown = candidates[1]
            .score_breakdown
            .ok_or_else(|| "reranked candidate should carry score breakdown".to_string())?;
        assert_eq!(score_breakdown.text_score, 0.20);
        assert_eq!(
            score_breakdown.combined_score,
            candidates[1].relevance.into_inner()
        );
        assert!(
            candidates[1].why.contains("Personalized PageRank rerank"),
            "rerank should annotate candidate why: {}",
            candidates[1].why
        );
        Ok(())
    }

    #[test]
    fn context_ppr_feature_disabled_preserves_text_scores() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        let mut candidates = vec![
            ppr_candidate(fixture.seed, 0.80)?,
            ppr_candidate(fixture.neighbor, 0.20)?,
        ];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            super::DEFAULT_CONTEXT_PPR_WEIGHT,
            &mut degraded,
        );

        assert_eq!(metrics.reranked_candidates, 0);
        assert_eq!(candidates[0].relevance.into_inner(), 0.80);
        assert_eq!(candidates[1].relevance.into_inner(), 0.20);
        assert!(candidates.iter().all(|item| item.score_breakdown.is_none()));
        let disabled = degraded
            .iter()
            .find(|entry| entry.code == "graph_feature_disabled")
            .ok_or_else(|| "expected graph_feature_disabled degradation".to_string())?;
        assert_eq!(disabled.severity, ContextResponseSeverity::Medium);
        assert!(disabled.message.contains("graph.feature.ppr.enabled"));
        assert_eq!(
            disabled.repair.as_deref(),
            Some("ee config set graph.feature.ppr.enabled true")
        );
        Ok(())
    }

    #[test]
    fn context_proximity_feature_disabled_skips_annotation() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        let mut candidates = vec![
            ppr_candidate(fixture.seed, 0.80)?,
            ppr_candidate(fixture.neighbor, 0.20)?,
        ];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_proximity_to_seed_scores(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            &mut degraded,
        );

        assert_eq!(metrics.annotated_candidates, 0);
        assert!(
            candidates
                .iter()
                .all(|item| item.proximity_to_seed.is_none())
        );
        let disabled = degraded
            .iter()
            .find(|entry| entry.code == "graph_feature_disabled")
            .ok_or_else(|| "expected graph_feature_disabled degradation".to_string())?;
        assert_eq!(disabled.severity, ContextResponseSeverity::Medium);
        assert!(disabled.message.contains("graph.feature.proximity.enabled"));
        assert_eq!(
            disabled.repair.as_deref(),
            Some("ee config set graph.feature.proximity.enabled true")
        );
        Ok(())
    }

    #[test]
    fn context_proximity_feature_enabled_annotates_seed_neighbor() -> Result<(), String> {
        use crate::db::{CreateMemoryLinkInput, MemoryLinkRelation, MemoryLinkSource};

        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        enable_context_proximity_feature(&fixture.workspace_path)?;
        fixture
            .connection
            .insert_memory_link(
                "link_00000000000000000000000902",
                &CreateMemoryLinkInput {
                    src_memory_id: fixture.seed.to_string(),
                    dst_memory_id: fixture.orphan.to_string(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("context-proximity-test".to_string()),
                    metadata_json: Some(mesh_link_metadata("deny", "metadata", true)),
                },
            )
            .map_err(|error| error.to_string())?;
        let mut candidates = vec![
            ppr_candidate(fixture.seed, 0.80)?,
            ppr_candidate(fixture.neighbor, 0.20)?,
            ppr_candidate(fixture.orphan, 0.40)?,
        ];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_proximity_to_seed_scores(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            &mut degraded,
        );

        assert_eq!(metrics.annotated_candidates, 2);
        assert_eq!(candidates[0].proximity_to_seed, Some(0.0));
        let neighbor_proximity = candidates[1]
            .proximity_to_seed
            .ok_or_else(|| "neighbor should be annotated".to_string())?;
        assert!(
            neighbor_proximity >= 1.0,
            "neighbor proximity should reflect seeded support link, got {neighbor_proximity}"
        );
        assert_eq!(candidates[2].proximity_to_seed, None);
        assert!(
            degraded.is_empty(),
            "enabled proximity should not degrade: {degraded:?}"
        );
        Ok(())
    }

    #[test]
    fn context_ppr_rerank_skips_stale_snapshot() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Stale)?;
        enable_context_ppr_feature(&fixture.workspace_path)?;
        let mut candidates = vec![ppr_candidate(fixture.seed, 0.80)?];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            super::DEFAULT_CONTEXT_PPR_WEIGHT,
            &mut degraded,
        );

        assert_eq!(metrics.reranked_candidates, 0);
        assert_eq!(candidates[0].relevance.into_inner(), 0.80);
        assert!(
            degraded.iter().any(
                |entry| entry.code == crate::models::degradation::GRAPH_PPR_SNAPSHOT_STALE_CODE
            ),
            "stale snapshot skip should emit graph snapshot degradation: {degraded:?}"
        );
        Ok(())
    }

    #[test]
    fn context_ppr_rerank_skips_empty_seed_map() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        enable_context_ppr_feature(&fixture.workspace_path)?;
        let mut candidates = vec![ppr_candidate(fixture.neighbor, 0.20)?];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            super::DEFAULT_CONTEXT_PPR_WEIGHT,
            &mut degraded,
        );

        assert_eq!(metrics.reranked_candidates, 0);
        assert_eq!(candidates[0].relevance.into_inner(), 0.20);
        assert!(
            degraded.iter().any(
                |entry| entry.code == crate::models::degradation::GRAPH_PPR_EMPTY_SEED_SET_CODE
            ),
            "empty seed skip should emit PPR degradation: {degraded:?}"
        );
        Ok(())
    }

    #[test]
    fn context_ppr_weight_zero_preserves_text_scores() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        let mut candidates = vec![
            ppr_candidate(fixture.seed, 0.80)?,
            ppr_candidate(fixture.neighbor, 0.20)?,
        ];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            0.0,
            &mut degraded,
        );

        assert_eq!(metrics.reranked_candidates, 0);
        assert_eq!(candidates[0].relevance.into_inner(), 0.80);
        assert_eq!(candidates[1].relevance.into_inner(), 0.20);
        assert!(candidates.iter().all(|item| item.score_breakdown.is_none()));
        assert!(degraded.is_empty());
        Ok(())
    }

    #[test]
    fn context_pack_dna_feature_disabled_skips_graph_open() -> Result<(), String> {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join(".ee").join("ee.db");
        let mut response =
            context_response_with_pack_item(MemoryId::from_uuid(uuid::Uuid::from_u128(904)))?;

        super::attach_pack_dna_to_context_response(&database_path, &mut response);

        assert_eq!(response.data.pack_dna, Some(serde_json::Value::Null));
        let disabled = response
            .data
            .degraded
            .iter()
            .find(|entry| entry.code == "graph_feature_disabled")
            .ok_or_else(|| "expected graph_feature_disabled degradation".to_string())?;
        assert_eq!(disabled.severity, ContextResponseSeverity::Medium);
        assert!(disabled.message.contains("graph.feature.pack_dna.enabled"));
        assert_eq!(
            disabled.repair.as_deref(),
            Some("ee config set graph.feature.pack_dna.enabled true")
        );
        Ok(())
    }

    #[test]
    fn context_ppr_weight_one_uses_ppr_score_as_combined_score() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        enable_context_ppr_feature(&fixture.workspace_path)?;
        let mut candidates = vec![
            ppr_candidate(fixture.seed, 0.80)?,
            ppr_candidate(fixture.neighbor, 0.20)?,
        ];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();

        let metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            1.0,
            &mut degraded,
        );

        assert_eq!(metrics.reranked_candidates, 2);
        for candidate in &candidates {
            let score_breakdown = candidate
                .score_breakdown
                .ok_or_else(|| "reranked candidate should carry score breakdown".to_string())?;
            assert_eq!(
                score_breakdown.combined_score,
                candidate.relevance.into_inner()
            );
            assert_eq!(score_breakdown.combined_score, score_breakdown.ppr_score);
        }
        assert!(degraded.is_empty());
        Ok(())
    }

    fn query_time(raw: &str) -> chrono::DateTime<chrono::Utc> {
        match chrono::DateTime::parse_from_rfc3339(raw) {
            Ok(timestamp) => timestamp.with_timezone(&chrono::Utc),
            Err(error) => panic!("test timestamp {raw:?} must be RFC3339: {error}"),
        }
    }

    fn stored_memory_with_time(
        created_at: &str,
        updated_at: &str,
        valid_from: Option<&str>,
        valid_to: Option<&str>,
    ) -> StoredMemory {
        StoredMemory {
            id: MemoryId::from_uuid(uuid::Uuid::from_u128(700)).to_string(),
            workspace_id: WorkspaceId::from_uuid(uuid::Uuid::from_u128(701)).to_string(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Run cargo fmt --check before release.".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            provenance_uri: None,
            trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "1".to_owned(),
            provenance_verification_status: "pending".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: created_at.to_owned(),
            updated_at: updated_at.to_owned(),
            tombstoned_at: None,
            valid_from: valid_from.map(str::to_owned),
            valid_to: valid_to.map(str::to_owned),
        }
    }

    #[test]
    fn temporal_time_window_filters_created_at_with_inclusive_boundaries() {
        let memory =
            stored_memory_with_time("2026-05-01T12:00:00Z", "2026-05-01T12:00:00Z", None, None);

        let inclusive = QueryTemporalFilters {
            after: Some(query_time("2026-05-01T12:00:00Z")),
            before: Some(query_time("2026-05-01T12:00:00Z")),
            ..QueryTemporalFilters::default()
        };
        assert_eq!(
            super::temporal_memory_outcome(&memory, &inclusive),
            super::TemporalCandidateOutcome::Include
        );

        let after_window = QueryTemporalFilters {
            after: Some(query_time("2026-05-01T12:00:01Z")),
            ..QueryTemporalFilters::default()
        };
        assert_eq!(
            super::temporal_memory_outcome(&memory, &after_window),
            super::TemporalCandidateOutcome::Exclude
        );

        let before_window = QueryTemporalFilters {
            before: Some(query_time("2026-05-01T11:59:59Z")),
            ..QueryTemporalFilters::default()
        };
        assert_eq!(
            super::temporal_memory_outcome(&memory, &before_window),
            super::TemporalCandidateOutcome::Exclude
        );
    }

    #[test]
    fn temporal_as_of_excludes_later_updates() {
        let later_update =
            stored_memory_with_time("2026-05-01T00:00:00Z", "2026-05-03T00:00:00Z", None, None);
        let filters = QueryTemporalFilters {
            as_of: Some(query_time("2026-05-02T00:00:00Z")),
            ..QueryTemporalFilters::default()
        };
        assert_eq!(
            super::temporal_memory_outcome(&later_update, &filters),
            super::TemporalCandidateOutcome::Exclude
        );

        let boundary_update =
            stored_memory_with_time("2026-05-01T00:00:00Z", "2026-05-02T00:00:00Z", None, None);
        assert_eq!(
            super::temporal_memory_outcome(&boundary_update, &filters),
            super::TemporalCandidateOutcome::Include
        );
    }

    #[test]
    fn temporal_validity_postures_handle_future_expired_and_current_windows() {
        let future = stored_memory_with_time(
            "2026-05-01T00:00:00Z",
            "2026-05-01T00:00:00Z",
            Some("2026-06-01T00:00:00Z"),
            None,
        );
        let expired = stored_memory_with_time(
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:00:00Z",
            None,
            Some("2026-04-30T23:59:59Z"),
        );
        let current = stored_memory_with_time(
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:00:00Z",
            Some("2026-04-01T00:00:00Z"),
            Some("2026-05-01T00:00:00Z"),
        );
        let reference_time = query_time("2026-05-01T00:00:00Z");

        let strict = QueryTemporalFilters {
            validity: Some(QueryTemporalValidity {
                posture: QueryTemporalValidityPosture::Strict,
                reference_time: Some(reference_time),
            }),
            ..QueryTemporalFilters::default()
        };
        assert_eq!(
            super::temporal_memory_outcome(&future, &strict),
            super::TemporalCandidateOutcome::Exclude
        );
        assert_eq!(
            super::temporal_memory_outcome(&expired, &strict),
            super::TemporalCandidateOutcome::Exclude
        );
        assert_eq!(
            super::temporal_memory_outcome(&current, &strict),
            super::TemporalCandidateOutcome::Include
        );

        let relaxed = QueryTemporalFilters {
            validity: Some(QueryTemporalValidity {
                posture: QueryTemporalValidityPosture::Relaxed,
                reference_time: Some(reference_time),
            }),
            ..QueryTemporalFilters::default()
        };
        assert_eq!(
            super::temporal_memory_outcome(&future, &relaxed),
            super::TemporalCandidateOutcome::IncludeRelaxedInvalid
        );

        let ignore = QueryTemporalFilters {
            validity: Some(QueryTemporalValidity {
                posture: QueryTemporalValidityPosture::Ignore,
                reference_time: Some(reference_time),
            }),
            ..QueryTemporalFilters::default()
        };
        assert_eq!(
            super::temporal_memory_outcome(&future, &ignore),
            super::TemporalCandidateOutcome::Include
        );
    }

    #[test]
    fn candidate_batch_db_failures_are_reported_before_candidate_skips() -> Result<(), String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(70));
        let search_report = SearchReport {
            status: SearchStatus::Success,
            query: "prepare release".to_string(),
            requested_limit: 1,
            results: vec![SearchHit {
                doc_id: memory_id.to_string(),
                score: 0.91,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.91),
                rerank_score: None,
                metadata: None,
                explanation: None,
            }],
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let mut degraded = Vec::new();

        let (candidates, metrics) = super::candidates_from_search_with_metrics(
            &connection,
            Path::new("/tmp/ee-context-test"),
            &search_report,
            &crate::models::QueryFilters::default(),
            false,
            &mut degraded,
        );

        assert!(candidates.is_empty());
        assert_eq!(metrics.search_hits, 1);
        assert_eq!(metrics.resolved_memory_ids, 1);
        assert_eq!(metrics.unique_memory_ids, 1);
        assert_eq!(metrics.memory_batch_reads, 1);
        assert_eq!(metrics.tag_batch_reads, 1);
        assert_eq!(metrics.converted_candidates, 0);
        assert_eq!(metrics.skipped_candidates, 1);

        let codes: BTreeSet<&str> = degraded.iter().map(|entry| entry.code.as_str()).collect();
        assert!(
            codes.contains("context_candidate_memory_batch_unavailable"),
            "{degraded:#?}"
        );
        assert!(
            codes.contains("context_candidate_tags_batch_unavailable"),
            "{degraded:#?}"
        );
        assert!(codes.contains("context_candidate_skipped"), "{degraded:#?}");
        assert!(degraded.iter().any(|entry| {
            entry.code == "context_candidate_memory_batch_unavailable"
                && entry.severity == ContextResponseSeverity::Medium
                && entry.repair.as_deref() == Some("ee status --json")
                && entry
                    .message
                    .contains("Context candidate memories could not be batch-loaded")
        }));
        assert!(degraded.iter().any(|entry| {
            entry.code == "context_candidate_tags_batch_unavailable"
                && entry.severity == ContextResponseSeverity::Medium
                && entry.repair.as_deref() == Some("ee status --json")
                && entry
                    .message
                    .contains("Context candidate memory tags could not be batch-loaded")
        }));

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn context_candidates_skip_blocked_mesh_hits_defensively() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(710)).to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("mesh-context-guard".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let local_id = MemoryId::from_uuid(uuid::Uuid::from_u128(711)).to_string();
        let blocked_id = MemoryId::from_uuid(uuid::Uuid::from_u128(712)).to_string();
        for (id, content) in [
            (
                local_id.as_str(),
                "Local release rule allowed in the context pack.",
            ),
            (
                blocked_id.as_str(),
                "PRIVATE REMOTE MESH BODY MUST NOT ENTER CONTEXT PACK",
            ),
        ] {
            connection
                .insert_memory(
                    id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_string(),
                        kind: "rule".to_string(),
                        content: content.to_string(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        provenance_uri: Some(format!("ee://memory/{id}")),
                        trust_class: TrustClass::HumanExplicit.as_str().to_string(),
                        trust_subclass: Some("fixture".to_string()),
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let search_report = SearchReport {
            status: SearchStatus::Success,
            query: "release mesh context guard".to_string(),
            requested_limit: 2,
            results: vec![
                SearchHit {
                    doc_id: blocked_id.clone(),
                    score: 0.99,
                    source: ScoreSource::Lexical,
                    fast_score: None,
                    quality_score: None,
                    lexical_score: Some(0.99),
                    rerank_score: None,
                    metadata: Some(serde_json::json!({
                        "mesh": {
                            "workspaceScopeDecision": "quarantine",
                            "cachedMaterialId": "mesh-quarantined-context",
                            "originWorkspaceId": "origin-private",
                            "originWorkspaceLabel": "/Users/alice/private/repo",
                            "producerPeerId": "peer-private",
                            "materialLane": "memory",
                            "trustLane": "cached",
                            "redactionPosture": "quarantined"
                        }
                    })),
                    explanation: None,
                },
                freshness_search_hit(&local_id, 0.90),
            ],
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };

        let mut degraded = Vec::new();
        let (candidates, metrics) = super::candidates_from_search_with_metrics(
            &connection,
            workspace_path,
            &search_report,
            &crate::models::QueryFilters::default(),
            false,
            &mut degraded,
        );

        assert_eq!(metrics.search_hits, 2);
        assert_eq!(metrics.skipped_candidates, 1);
        assert_eq!(metrics.resolved_memory_ids, 1);
        assert_eq!(metrics.converted_candidates, 1);
        assert!(degraded.iter().any(|entry| {
            entry.code == "mesh_workspace_scope_filtered"
                && entry.severity == ContextResponseSeverity::Low
                && entry.message.contains("Filtered 1 mesh-derived search hit")
        }));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].memory_id.to_string(), local_id);
        let candidate = &candidates[0];
        assert!(candidate.content.contains("Local release rule"));
        assert!(!candidate.content.contains("PRIVATE REMOTE MESH BODY"));
        assert!(
            candidate
                .provenance
                .iter()
                .all(|entry| !entry.note.contains("/Users/alice/private/repo"))
        );
        assert!(!candidate.why.contains("mesh-quarantined-context"));

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn context_candidates_reject_mesh_hits_claiming_human_explicit_trust() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(713)).to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("mesh-human-explicit-guard".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let local_id = MemoryId::from_uuid(uuid::Uuid::from_u128(714)).to_string();
        let peer_id = MemoryId::from_uuid(uuid::Uuid::from_u128(715)).to_string();
        for (id, content) in [
            (
                local_id.as_str(),
                "Local release rule still allowed in the context pack.",
            ),
            (
                peer_id.as_str(),
                "REMOTE PEER MATERIAL MUST NOT BE AUTHORITATIVE HUMAN CONTENT",
            ),
        ] {
            connection
                .insert_memory(
                    id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_string(),
                        kind: "rule".to_string(),
                        content: content.to_string(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        provenance_uri: Some(format!("ee://memory/{id}")),
                        trust_class: TrustClass::HumanExplicit.as_str().to_string(),
                        trust_subclass: Some("fixture".to_string()),
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let search_report = SearchReport {
            status: SearchStatus::Success,
            query: "mesh human explicit guard".to_string(),
            requested_limit: 2,
            results: vec![
                SearchHit {
                    doc_id: peer_id.clone(),
                    score: 0.99,
                    source: ScoreSource::Lexical,
                    fast_score: None,
                    quality_score: None,
                    lexical_score: Some(0.99),
                    rerank_score: None,
                    metadata: Some(serde_json::json!({
                        "mesh": {
                            "workspaceScopeDecision": "allow",
                            "workspaceId": "wsp_local_alpha",
                            "cachedMaterialId": "mesh-human-explicit-context",
                            "originWorkspaceId": "origin-private",
                            "originWorkspaceLabel": "/Users/alice/private/repo",
                            "producerPeerId": "peer-private",
                            "producerPeerLabel": "/Users/alice/private/peer-agent",
                            "materialLane": "metadata",
                            "importDecisionId": "mesh_dec_human_explicit",
                            "trustLane": "peerAgent",
                            "redactionPosture": "metadata"
                        }
                    })),
                    explanation: None,
                },
                freshness_search_hit(&local_id, 0.90),
            ],
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };

        let mut degraded = Vec::new();
        let (candidates, metrics) = super::candidates_from_search_with_metrics(
            &connection,
            workspace_path,
            &search_report,
            &crate::models::QueryFilters::default(),
            false,
            &mut degraded,
        );

        assert_eq!(metrics.search_hits, 2);
        assert_eq!(metrics.skipped_candidates, 1);
        assert_eq!(metrics.resolved_memory_ids, 2);
        assert_eq!(metrics.converted_candidates, 1);
        assert!(degraded.iter().any(|entry| {
            entry.code == "mesh_peer_human_explicit_filtered"
                && entry.severity == ContextResponseSeverity::Medium
                && entry
                    .message
                    .contains("peer material must not appear as local human_explicit")
                && entry
                    .repair
                    .as_deref()
                    .is_some_and(|repair| repair.contains("import_trust_class"))
        }));
        let degradation_text = degraded
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!degradation_text.contains("/Users/alice/private/repo"));
        assert!(!degradation_text.contains("/Users/alice/private/peer-agent"));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].memory_id.to_string(), local_id);
        assert!(!candidates[0].content.contains("REMOTE PEER MATERIAL"));
        assert_eq!(
            candidates[0].trust.class,
            TrustClass::HumanExplicit,
            "local non-mesh human memory stays authoritative"
        );

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn candidate_resolution_reports_mixed_evidence_freshness_deterministically()
    -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        std::fs::write(
            workspace_path.join("changed.md"),
            "current evidence changed",
        )
        .map_err(|error| error.to_string())?;

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(800)).to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("freshness-ordering".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let missing_id = MemoryId::from_uuid(uuid::Uuid::from_u128(801)).to_string();
        let unsupported_id = MemoryId::from_uuid(uuid::Uuid::from_u128(802)).to_string();
        let changed_id = MemoryId::from_uuid(uuid::Uuid::from_u128(803)).to_string();
        for (id, content, provenance_uri) in [
            (
                missing_id.as_str(),
                "missing evidence body",
                "file://missing.md#L1",
            ),
            (
                unsupported_id.as_str(),
                "unsupported evidence body",
                "cass-session://freshness-ordering#L1",
            ),
            (
                changed_id.as_str(),
                "original evidence body",
                "file://changed.md#L1",
            ),
        ] {
            connection
                .insert_memory(
                    id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "procedural".to_string(),
                        kind: "rule".to_string(),
                        content: content.to_string(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        provenance_uri: Some(provenance_uri.to_string()),
                        trust_class: TrustClass::HumanExplicit.as_str().to_string(),
                        trust_subclass: Some("fixture".to_string()),
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let search_report = SearchReport {
            status: SearchStatus::Success,
            query: "freshness ordering".to_string(),
            requested_limit: 3,
            results: vec![
                freshness_search_hit(&missing_id, 0.93),
                freshness_search_hit(&unsupported_id, 0.92),
                freshness_search_hit(&changed_id, 0.91),
            ],
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };

        let mut first_degraded = Vec::new();
        let (first_candidates, first_metrics) = super::candidates_from_search_with_metrics(
            &connection,
            workspace_path,
            &search_report,
            &crate::models::QueryFilters::default(),
            false,
            &mut first_degraded,
        );
        let mut second_degraded = Vec::new();
        let (second_candidates, second_metrics) = super::candidates_from_search_with_metrics(
            &connection,
            workspace_path,
            &search_report,
            &crate::models::QueryFilters::default(),
            false,
            &mut second_degraded,
        );

        assert_eq!(first_candidates.len(), 3);
        assert_eq!(second_candidates.len(), 3);
        assert_eq!(first_metrics.converted_candidates, 3);
        assert_eq!(second_metrics.converted_candidates, 3);

        let first_codes = freshness_degradation_codes(&first_degraded);
        let second_codes = freshness_degradation_codes(&second_degraded);
        assert_eq!(
            first_codes,
            vec![
                "context_evidence_freshness_missing_source",
                "context_evidence_freshness_unsupported_source",
                "context_evidence_freshness_changed_source",
            ]
        );
        assert_eq!(first_codes, second_codes);

        let provenance_notes = first_candidates
            .iter()
            .filter_map(|candidate| candidate.provenance.first())
            .map(|provenance| provenance.note.as_str())
            .collect::<Vec<_>>();
        assert!(provenance_notes[0].contains("evidenceFreshness=missing_source"));
        assert!(provenance_notes[1].contains("evidenceFreshness=unsupported_source"));
        assert!(provenance_notes[2].contains("evidenceFreshness=changed_source"));

        connection.close().map_err(|error| error.to_string())
    }

    fn freshness_search_hit(memory_id: &str, score: f32) -> SearchHit {
        SearchHit {
            doc_id: memory_id.to_string(),
            score,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(score),
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    fn freshness_degradation_codes(
        degraded: &[crate::pack::ContextResponseDegradation],
    ) -> Vec<&str> {
        degraded
            .iter()
            .filter_map(|entry| {
                entry
                    .code
                    .starts_with("context_evidence_freshness_")
                    .then_some(entry.code.as_str())
            })
            .collect()
    }

    #[test]
    fn context_performance_explain_report_is_redaction_safe_and_counts_pruning()
    -> Result<(), String> {
        let memory_a = MemoryId::from_uuid(uuid::Uuid::from_u128(10));
        let memory_b = MemoryId::from_uuid(uuid::Uuid::from_u128(11));
        let provenance = vec![
            PackProvenance::new(ProvenanceUri::EeMemory(memory_a), "fixture provenance")
                .map_err(|error| error.to_string())?,
        ];
        let candidate_a = PackCandidate::new(PackCandidateInput {
            memory_id: memory_a,
            section: PackSection::ProceduralRules,
            content: "Rotate SECRET_VALUE_ONE before release.".to_string(),
            estimated_tokens: 45,
            relevance: crate::models::UnitScore::parse(0.95).map_err(|error| error.to_string())?,
            utility: crate::models::UnitScore::parse(0.80).map_err(|error| error.to_string())?,
            provenance: provenance.clone(),
            why: "selected by fixture".to_string(),
        })
        .map_err(|error| error.to_string())?
        .with_diversity_key("release".to_string());
        let candidate_b = PackCandidate::new(PackCandidateInput {
            memory_id: memory_b,
            section: PackSection::Decisions,
            content: "Check SECRET_VALUE_TWO in CI before deploy.".to_string(),
            estimated_tokens: 45,
            relevance: crate::models::UnitScore::parse(0.90).map_err(|error| error.to_string())?,
            utility: crate::models::UnitScore::parse(0.70).map_err(|error| error.to_string())?,
            provenance,
            why: "selected by fixture".to_string(),
        })
        .map_err(|error| error.to_string())?
        .with_diversity_key("ci".to_string());
        let request = ContextRequest::new(ContextRequestInput {
            query: "explain sk_live_do_not_emit".to_string(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(60),
            candidate_pool: Some(2),
            max_results: None,
            sections: Vec::new(),
        })
        .map_err(|error| error.to_string())?;
        let draft = assemble_draft_with_profile(
            request.profile,
            request.query.clone(),
            TokenBudget::new(60).map_err(|error| error.to_string())?,
            [candidate_a, candidate_b],
        )
        .map_err(|error| error.to_string())?;
        let search_report = SearchReport {
            status: SearchStatus::Success,
            query: request.query.clone(),
            requested_limit: 2,
            results: vec![
                SearchHit {
                    doc_id: memory_a.to_string(),
                    score: 0.95,
                    source: ScoreSource::Lexical,
                    fast_score: None,
                    quality_score: None,
                    lexical_score: Some(0.95),
                    rerank_score: None,
                    metadata: None,
                    explanation: None,
                },
                SearchHit {
                    doc_id: memory_b.to_string(),
                    score: 0.90,
                    source: ScoreSource::Lexical,
                    fast_score: None,
                    quality_score: None,
                    lexical_score: Some(0.90),
                    rerank_score: None,
                    metadata: None,
                    explanation: None,
                },
            ],
            elapsed_ms: 3.4,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let options = super::ContextPackOptions {
            workspace_path: PathBuf::from("/tmp/ee-explain"),
            database_path: None,
            index_dir: None,
            query: request.query.clone(),
            speed: crate::search::SpeedMode::Instant,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(60),
            candidate_pool: Some(2),
            max_results: None,
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            output_options: Default::default(),
        };
        let trace = ContextPerformanceTrace {
            db_open_count: 1,
            index_status_checks: 1,
            pack_record_writes: 1,
            candidate_resolution: CandidateResolutionMetrics {
                search_hits: 2,
                resolved_memory_ids: 2,
                unique_memory_ids: 2,
                memory_batch_reads: 1,
                tag_batch_reads: 1,
                converted_candidates: 2,
                ..CandidateResolutionMetrics::default()
            },
            timings: vec![PerformanceTiming {
                name: "packAssembly",
                elapsed: Duration::from_millis(3),
            }],
            ..ContextPerformanceTrace::default()
        };
        let slo = pack_assembly_slo_for_run(
            options.output_options.resource_profile,
            &draft,
            &search_report,
            &trace,
        );

        let json = context_performance_json(
            "pack",
            &options,
            &request,
            &search_report,
            &draft,
            &[],
            &trace,
            &slo,
        );
        let rendered = json.to_string();

        assert_eq!(json["schema"], PERFORMANCE_EXPLAIN_SCHEMA_V1);
        assert_eq!(json["data"]["command"], "pack");
        assert_eq!(json["data"]["query"]["textIncluded"], false);
        assert_eq!(json["data"]["dbReads"]["memoryBatchReads"], 1);
        assert_eq!(json["data"]["candidates"]["convertedCandidates"], 2);
        assert_eq!(json["data"]["pack"]["pruning"]["tokenBudgetExceeded"], 2);
        assert_eq!(json["data"]["cache"]["status"], "fallback");
        assert_eq!(json["data"]["redaction"]["memoryContentIncluded"], false);
        assert!(!rendered.contains("sk_live_do_not_emit"));
        assert!(!rendered.contains("SECRET_VALUE_ONE"));
        assert!(!rendered.contains("SECRET_VALUE_TWO"));
        assert!(!rendered.contains(&memory_a.to_string()));
        Ok(())
    }

    #[test]
    fn context_pack_falls_back_to_stored_memory_when_index_open_fails() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let db_path = ee_dir.join("ee.db");
        let empty_index_dir = tempdir.path().join("empty-index");
        std::fs::create_dir_all(&empty_index_dir).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = super::stable_context_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(42)).to_string();
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo fmt --check before release.".to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: None,
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let response = super::run_context_pack(&super::ContextPackOptions {
            workspace_path: workspace,
            database_path: Some(db_path),
            index_dir: Some(empty_index_dir),
            query: "format before release".to_owned(),
            speed: crate::search::SpeedMode::Default,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(400),
            candidate_pool: Some(10),
            max_results: None,
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            output_options: Default::default(),
        })
        .map_err(|error| error.to_string())?;

        let packed_ids: Vec<String> = response
            .data
            .pack
            .items
            .iter()
            .map(|item| item.memory_id.to_string())
            .collect();
        assert!(
            packed_ids.contains(&memory_id),
            "fallback context should include matching stored memory, got {packed_ids:?}"
        );
        let degraded_codes: BTreeSet<&str> = response
            .data
            .degraded
            .iter()
            .map(|entry| entry.code.as_str())
            .collect();
        assert!(degraded_codes.contains("index_missing"));
        assert!(degraded_codes.contains("context_lexical_fallback"));
        Ok(())
    }

    #[test]
    fn context_pack_seeded_entrypoint_replays_pack_record_id() -> Result<(), String> {
        fn run_seeded_pack(seed: u64) -> Result<String, String> {
            let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
            let workspace = tempdir.path().join("workspace");
            std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
            let workspace = workspace
                .canonicalize()
                .map_err(|error| error.to_string())?;
            let ee_dir = workspace.join(".ee");
            std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
            let db_path = ee_dir.join("ee.db");
            let empty_index_dir = tempdir.path().join("empty-index");
            std::fs::create_dir_all(&empty_index_dir).map_err(|error| error.to_string())?;

            let connection =
                DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            let workspace_id = super::stable_context_workspace_id(&workspace);
            connection
                .insert_workspace(
                    &workspace_id,
                    &CreateWorkspaceInput {
                        path: workspace.to_string_lossy().into_owned(),
                        name: Some("workspace".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(4242)).to_string();
            connection
                .insert_memory(
                    &memory_id,
                    &CreateMemoryInput {
                        workspace_id,
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: "Run cargo fmt --check before release.".to_owned(),
                        workflow_id: None,
                        confidence: 0.95,
                        utility: 0.80,
                        importance: 0.70,
                        provenance_uri: None,
                        trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                        trust_subclass: Some("test".to_owned()),
                        tags: vec!["release".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;

            let determinism = crate::runtime::determinism::Deterministic::from_seed(seed);
            let response = super::run_context_pack_seeded(
                &super::ContextPackOptions {
                    workspace_path: workspace,
                    database_path: Some(db_path),
                    index_dir: Some(empty_index_dir),
                    query: "format before release".to_owned(),
                    speed: crate::search::SpeedMode::Default,
                    filters: crate::models::QueryFilters::default(),
                    profile: Some(ContextPackProfile::Balanced),
                    max_tokens: Some(400),
                    candidate_pool: Some(10),
                    max_results: None,
                    include_tombstoned: false,
                    as_of: None,
                    include_expired: false,
                    include_future: false,
                    include_stale: false,
                    redaction_level: crate::models::RedactionLevel::Minimal,
                    memory_scope: MemoryScope::Swarm,
                    strict_scope: false,
                    ppr_weight: None,
                    pagination: None,
                    coordination_snapshot_path: None,
                    coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                    output_options: Default::default(),
                },
                &determinism,
            )
            .map_err(|error| error.to_string())?;

            assert!(
                response
                    .data
                    .pack
                    .items
                    .iter()
                    .any(|item| item.memory_id.to_string() == memory_id),
                "seeded context pack should include the fallback memory"
            );
            let history = connection
                .list_pack_records_for_memory(&memory_id, 10)
                .map_err(|error| error.to_string())?;
            assert_eq!(history.len(), 1);
            Ok(history[0].0.id.clone())
        }

        let first = run_seeded_pack(8080)?;
        let replay = run_seeded_pack(8080)?;
        let other_seed = run_seeded_pack(8081)?;

        assert_eq!(first, replay);
        assert_ne!(first, other_seed);
        assert!(first.starts_with("pack_"));
        Ok(())
    }

    #[test]
    fn context_read_pool_size_preserves_pack_hash() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let db_path = ee_dir.join("ee.db");
        let empty_index_dir = tempdir.path().join("empty-index");
        std::fs::create_dir_all(&empty_index_dir).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = super::stable_context_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                &MemoryId::from_uuid(uuid::Uuid::from_u128(44)).to_string(),
                &CreateMemoryInput {
                    workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run the read pool determinism gate before release.".to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: None,
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let base_options = super::ContextPackOptions {
            workspace_path: workspace.clone(),
            database_path: Some(db_path.clone()),
            index_dir: Some(empty_index_dir),
            query: "read pool determinism release".to_owned(),
            speed: crate::search::SpeedMode::Default,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(400),
            candidate_pool: Some(10),
            max_results: None,
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            output_options: Default::default(),
        };

        let mut hashes_by_pool_size = BTreeMap::new();
        for pool_size in [1_u32, 4, 8] {
            std::fs::write(
                ee_dir.join("config.toml"),
                format!(
                    "[storage.read_pool]\nsize = {pool_size}\nidle_timeout_seconds = 30\npin_snapshot = true\n"
                ),
            )
            .map_err(|error| error.to_string())?;

            let response = super::run_context_pack(&base_options)
                .map_err(|error| format!("pool_size={pool_size} context pack failed: {error:?}"))?;
            assert!(
                response
                    .data
                    .degraded
                    .iter()
                    .all(|entry| entry.code != "context_config_unavailable"),
                "valid read-pool config for size {pool_size} should not degrade"
            );
            let hash = response
                .data
                .pack
                .hash
                .clone()
                .ok_or_else(|| format!("pool_size={pool_size} response missing pack hash"))?;
            hashes_by_pool_size.insert(pool_size, hash);
        }

        let single_hash = hashes_by_pool_size
            .get(&1)
            .ok_or_else(|| "pool_size=1 hash missing".to_string())?;
        for pool_size in [4_u32, 8] {
            assert_eq!(
                hashes_by_pool_size.get(&pool_size),
                Some(single_hash),
                "pool_size={pool_size} must preserve the pool_size=1 pack hash"
            );
        }
        Ok(())
    }

    #[test]
    fn checked_context_read_snapshot_returns_clean_error_after_pin_expiry() -> Result<(), String> {
        let read_pool = ReadConnectionPool::new(
            DatabaseConfig::memory(),
            PoolConfig::new(1, Duration::from_secs(30)).with_max_pin_duration(Duration::ZERO),
        );
        let read_snapshot = read_pool
            .pin_snapshot()
            .map_err(|error| error.to_string())?;

        let error = match super::checked_context_read_snapshot(&read_pool, &read_snapshot) {
            Ok(_) => return Err("expired snapshot pin should not return a connection".to_string()),
            Err(error) => error,
        };

        assert!(
            format!("{error:?}").contains("Read snapshot unavailable"),
            "expired pin should return a storage error with clean context, got {error:?}"
        );
        assert!(read_snapshot.is_poisoned());
        Ok(())
    }

    #[test]
    fn context_read_pool_config_honors_max_pin_duration_seconds() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            ee_dir.join("config.toml"),
            "[storage.read_pool]\nsize = 2\nidle_timeout_seconds = 11\nmax_pin_duration_seconds = 7\npin_snapshot = true\n",
        )
        .map_err(|error| error.to_string())?;

        let mut degraded = Vec::new();
        let (config, pin_snapshot) = super::context_read_pool_config(&workspace, &mut degraded);

        assert!(degraded.is_empty());
        assert!(pin_snapshot);
        assert_eq!(config.max_size(), 2);
        assert_eq!(config.idle_timeout(), Duration::from_secs(11));
        assert_eq!(config.max_pin_duration(), Duration::from_secs(7));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn context_workspace_config_rejects_symlinked_config_file() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let ee_dir = workspace.join(".ee");
        let outside_config = tempdir.path().join("outside-config.toml");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        std::fs::write(&outside_config, "[graph.feature]\nppr_enabled = true\n")
            .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_config, ee_dir.join("config.toml"))
            .map_err(|error| error.to_string())?;

        let error = super::context_workspace_config(&workspace, "test context config")
            .expect_err("symlinked context config file must be rejected");

        assert!(
            error.contains("symbolic link"),
            "expected symlink rejection, got {error}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn context_workspace_config_rejects_symlinked_metadata_parent() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let real_metadata = tempdir.path().join("real-ee");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&real_metadata).map_err(|error| error.to_string())?;
        std::fs::write(
            real_metadata.join("config.toml"),
            "[graph.feature]\nppr_enabled = true\n",
        )
        .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&real_metadata, workspace.join(".ee"))
            .map_err(|error| error.to_string())?;

        let error = super::context_workspace_config(&workspace, "test context config")
            .expect_err("symlinked context config parent must be rejected");

        assert!(
            error.contains("symbolic link"),
            "expected symlink rejection, got {error}"
        );
        Ok(())
    }

    #[test]
    fn context_read_pool_config_honors_env_overrides() -> Result<(), String> {
        let read_pool = ReadPoolConfig {
            size: Some(2),
            idle_timeout_seconds: Some(11),
            max_pin_duration_seconds: Some(7),
            pin_snapshot: Some(true),
        };
        let env = super::ContextReadPoolEnv {
            size: Some(4),
            idle_timeout_seconds: Some(13),
            max_pin_duration_seconds: Some(17),
            acquire_timeout_ms: Some(23),
            disable_pin: Some(true),
        };

        let (config, pin_snapshot) = super::context_read_pool_config_from_values(read_pool, env);

        assert!(!pin_snapshot);
        assert_eq!(config.max_size(), 4);
        assert_eq!(config.idle_timeout(), Duration::from_secs(13));
        assert_eq!(config.max_pin_duration(), Duration::from_secs(17));
        assert_eq!(config.acquire_timeout(), Duration::from_millis(23));
        Ok(())
    }

    #[test]
    fn pinned_snapshot_prevents_revise_generation_mixing_in_pack_candidates() -> Result<(), String>
    {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let db_path = ee_dir.join("ee.db");

        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = super::stable_context_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let original_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(45)).to_string();
        connection
            .insert_memory(
                &original_memory_id,
                &CreateMemoryInput {
                    workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Snapshot provenance release must stay original generation."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: Some("https://example.com/original-generation".to_owned()),
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let read_pool = ReadConnectionPool::new(
            DatabaseConfig::file(db_path.clone()),
            PoolConfig::new(1, Duration::from_secs(30)),
        );
        let read_snapshot = read_pool
            .pin_snapshot()
            .map_err(|error| error.to_string())?;

        let revise_report = revise_memory(&ReviseMemoryOptions {
            database_path: &db_path,
            original_memory_id: &original_memory_id,
            content: Some("Revised generation should not leak into this pinned context pack."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: Some("https://example.com/revised-generation"),
            reason: ReviseReason::Update,
            actor: Some("context snapshot regression"),
            dry_run: false,
        });
        assert!(
            revise_report.success,
            "revise should commit through a separate write connection: {revise_report:?}"
        );
        let revised_memory_id = revise_report
            .new_id
            .clone()
            .ok_or_else(|| "revise report missing new memory id".to_string())?;

        let mut degraded = Vec::new();
        let hits = super::lexical_memory_fallback_hits(
            &read_snapshot,
            &workspace,
            "snapshot provenance release original",
            10,
            false,
            None,
            false,
            false,
            false,
            &mut degraded,
        );
        assert!(
            hits.iter().any(|hit| hit.doc_id == original_memory_id),
            "pinned snapshot should still see the original live generation, got {hits:?}"
        );
        assert!(
            hits.iter().all(|hit| hit.doc_id != revised_memory_id),
            "pinned snapshot must not see the later revised generation"
        );

        let search_report = SearchReport {
            status: SearchStatus::Success,
            query: "snapshot provenance release original".to_owned(),
            requested_limit: 10,
            results: hits,
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: Some(0.0),
            candidates_below_floor: 0,
            source_mode_requested: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_applied: crate::core::search::SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let (candidates, _) = super::candidates_from_search_with_metrics(
            &read_snapshot,
            &workspace,
            &search_report,
            &crate::models::QueryFilters::default(),
            false,
            &mut degraded,
        );
        let draft = assemble_draft_with_profile(
            ContextPackProfile::Balanced,
            "snapshot provenance release original",
            TokenBudget::new(400).map_err(|error| error.to_string())?,
            candidates,
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(draft.items.len(), 1, "expected one pinned-snapshot item");
        let item = &draft.items[0];
        assert_eq!(item.memory_id.to_string(), original_memory_id);
        assert_eq!(
            item.content,
            "Snapshot provenance release must stay original generation."
        );
        assert!(
            !item.content.contains("Revised generation should not leak"),
            "pack item content must not mix in the revised generation"
        );
        let provenance_urls = item
            .provenance
            .iter()
            .filter_map(|entry| match &entry.uri {
                ProvenanceUri::Web { url } => Some(url.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            provenance_urls,
            vec!["https://example.com/original-generation"]
        );
        Ok(())
    }

    #[test]
    fn context_pack_tombstone_visibility_is_opt_in() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let db_path = ee_dir.join("ee.db");
        let empty_index_dir = tempdir.path().join("empty-index");
        std::fs::create_dir_all(&empty_index_dir).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = super::stable_context_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(43)).to_string();
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run cargo clippy before release candidate signoff.".to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: None,
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .tombstone_memory(&memory_id)
            .map_err(|error| error.to_string())?;
        drop(connection);

        let base_options = super::ContextPackOptions {
            workspace_path: workspace,
            database_path: Some(db_path),
            index_dir: Some(empty_index_dir),
            query: "clippy release candidate".to_owned(),
            speed: crate::search::SpeedMode::Default,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(400),
            candidate_pool: Some(10),
            max_results: None,
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            output_options: Default::default(),
        };

        let default_response = super::run_context_pack(&base_options)
            .map_err(|error| format!("default context pack failed: {error:?}"))?;
        assert!(
            default_response
                .data
                .pack
                .items
                .iter()
                .all(|item| item.memory_id.to_string() != memory_id),
            "default context pack should exclude tombstoned memories"
        );

        let mut include_options = base_options.clone();
        include_options.include_tombstoned = true;
        let included_response = super::run_context_pack(&include_options)
            .map_err(|error| format!("include tombstoned context pack failed: {error:?}"))?;
        let included_item = included_response
            .data
            .pack
            .items
            .iter()
            .find(|item| item.memory_id.to_string() == memory_id)
            .ok_or_else(|| "opt-in context pack should include tombstoned memory".to_owned())?;
        let tombstoned_at = included_item.tombstoned_at.as_deref().ok_or_else(|| {
            "included tombstoned item should carry lifecycle timestamp".to_owned()
        })?;

        let rendered = crate::output::render_context_response_json(&included_response);
        let json: serde_json::Value = serde_json::from_str(&rendered)
            .map_err(|error| format!("context JSON should parse: {error}"))?;
        assert_eq!(
            json["data"]["pack"]["items"][0]["lifecycle"]["status"],
            "tombstoned"
        );
        assert_eq!(
            json["data"]["pack"]["items"][0]["lifecycle"]["tombstonedAt"],
            tombstoned_at
        );
        Ok(())
    }

    #[test]
    fn context_pack_validity_window_honors_as_of_and_include_future() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let db_path = ee_dir.join("ee.db");
        let empty_index_dir = tempdir.path().join("empty-index");
        std::fs::create_dir_all(&empty_index_dir).map_err(|error| error.to_string())?;

        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = super::stable_context_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let current_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(43)).to_string();
        let expired_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(44)).to_string();
        let future_memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(45)).to_string();
        connection
            .insert_memory(
                &current_memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Validity window marker zeta current release rule.".to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: None,
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
                    valid_to: Some("2099-01-01T00:00:00Z".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                &expired_memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Validity window marker zeta expired release rule.".to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: None,
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
                    valid_to: Some("2021-01-01T00:00:00Z".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                &future_memory_id,
                &CreateMemoryInput {
                    workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Validity window marker zeta future release rule.".to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: None,
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: Some("2099-06-01T00:00:00Z".to_owned()),
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let base_options = super::ContextPackOptions {
            workspace_path: workspace,
            database_path: Some(db_path),
            index_dir: Some(empty_index_dir),
            query: "validity window marker zeta release rule".to_owned(),
            speed: crate::search::SpeedMode::Default,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(400),
            candidate_pool: Some(10),
            max_results: None,
            include_tombstoned: false,
            as_of: Some(query_time("2098-01-01T00:00:00Z")),
            include_expired: false,
            include_future: false,
            include_stale: false,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            output_options: Default::default(),
        };

        let default_response = super::run_context_pack(&base_options)
            .map_err(|error| format!("default validity context pack failed: {error:?}"))?;
        assert!(
            default_response
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == current_memory_id),
            "context should include bounded current memory before valid_to"
        );
        assert!(
            !default_response
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == expired_memory_id),
            "context should exclude expired memory by default"
        );
        assert!(
            !default_response
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == future_memory_id),
            "context should exclude not-yet-valid memory before valid_from"
        );

        let mut include_options = base_options.clone();
        include_options.include_future = true;
        let include_response = super::run_context_pack(&include_options)
            .map_err(|error| format!("include future context pack failed: {error:?}"))?;
        let included_item = include_response
            .data
            .pack
            .items
            .iter()
            .find(|item| item.memory_id.to_string() == future_memory_id)
            .ok_or_else(|| "include_future should keep not-yet-valid memory".to_owned())?;
        assert_eq!(
            included_item
                .lifecycle
                .as_ref()
                .map(|lifecycle| lifecycle.validity_status.as_str()),
            Some("future")
        );

        let mut include_expired_options = base_options.clone();
        include_expired_options.include_expired = true;
        let include_expired_response = super::run_context_pack(&include_expired_options)
            .map_err(|error| format!("include expired context pack failed: {error:?}"))?;
        let included_expired_item = include_expired_response
            .data
            .pack
            .items
            .iter()
            .find(|item| item.memory_id.to_string() == expired_memory_id)
            .ok_or_else(|| "include_expired should keep expired memory".to_owned())?;
        assert_eq!(
            included_expired_item
                .lifecycle
                .as_ref()
                .map(|lifecycle| lifecycle.validity_status.as_str()),
            Some("expired")
        );

        let mut replay_options = base_options;
        replay_options.as_of = Some(query_time("2099-06-15T00:00:00Z"));
        let replay_response = super::run_context_pack(&replay_options)
            .map_err(|error| format!("as-of replay context pack failed: {error:?}"))?;
        assert!(
            replay_response
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == future_memory_id),
            "as_of after valid_from should include the memory"
        );
        Ok(())
    }

    #[test]
    fn access_level_default_is_none() {
        assert_eq!(AccessLevel::default(), AccessLevel::None);
    }

    #[test]
    fn access_level_ordering_is_none_lt_read_lt_write() {
        assert!(AccessLevel::None < AccessLevel::Read);
        assert!(AccessLevel::Read < AccessLevel::Write);
        assert!(AccessLevel::None < AccessLevel::Write);
    }

    #[test]
    fn access_level_strings_are_stable() {
        assert_eq!(AccessLevel::None.as_str(), "none");
        assert_eq!(AccessLevel::Read.as_str(), "read");
        assert_eq!(AccessLevel::Write.as_str(), "write");
    }

    #[test]
    fn access_level_allows_read_and_write_predicates() {
        assert!(!AccessLevel::None.allows_read());
        assert!(!AccessLevel::None.allows_write());
        assert!(AccessLevel::Read.allows_read());
        assert!(!AccessLevel::Read.allows_write());
        assert!(AccessLevel::Write.allows_read());
        assert!(AccessLevel::Write.allows_write());
    }

    #[test]
    fn access_level_min_const_returns_lesser() {
        assert_eq!(
            AccessLevel::min_const(AccessLevel::None, AccessLevel::Write),
            AccessLevel::None,
        );
        assert_eq!(
            AccessLevel::min_const(AccessLevel::Read, AccessLevel::Write),
            AccessLevel::Read,
        );
        assert_eq!(
            AccessLevel::min_const(AccessLevel::Read, AccessLevel::Read),
            AccessLevel::Read,
        );
    }

    #[test]
    fn capability_set_constructors_are_consistent() {
        let n = CapabilitySet::none();
        assert_eq!(n.db, AccessLevel::None);
        assert_eq!(n.network, AccessLevel::None);

        let r = CapabilitySet::read_only();
        assert_eq!(r.db, AccessLevel::Read);
        assert_eq!(r.search_index, AccessLevel::Read);
        assert_eq!(r.graph_snapshot, AccessLevel::Read);
        assert_eq!(r.cass_subprocess, AccessLevel::Read);
        assert_eq!(r.filesystem, AccessLevel::Read);
        assert_eq!(r.audit_log, AccessLevel::Read);
        // Network stays None even in read_only because v1 is
        // local-first and outbound network is opt-in per adapter.
        assert_eq!(r.network, AccessLevel::None);

        let f = CapabilitySet::full_local();
        assert_eq!(f.db, AccessLevel::Write);
        assert_eq!(f.search_index, AccessLevel::Write);
        assert_eq!(f.graph_snapshot, AccessLevel::Write);
        assert_eq!(f.cass_subprocess, AccessLevel::Write);
        assert_eq!(f.filesystem, AccessLevel::Write);
        assert_eq!(f.audit_log, AccessLevel::Write);
        assert_eq!(f.network, AccessLevel::None);
    }

    #[test]
    fn narrow_against_full_returns_self() {
        // full_local has Write everywhere except network; narrowing a
        // read_only set against it must leave the read_only set
        // unchanged because every slot of read_only is already <= the
        // matching full_local slot.
        let r = CapabilitySet::read_only();
        assert_eq!(r.narrow(CapabilitySet::full_local()), r);
    }

    #[test]
    fn narrow_against_none_zeroes_every_slot() {
        let f = CapabilitySet::full_local();
        assert_eq!(f.narrow(CapabilitySet::none()), CapabilitySet::none());
    }

    #[test]
    fn narrow_with_mixed_mask_is_elementwise_min() {
        let original = CapabilitySet {
            db: AccessLevel::Write,
            search_index: AccessLevel::Write,
            graph_snapshot: AccessLevel::Write,
            cass_subprocess: AccessLevel::Write,
            filesystem: AccessLevel::Write,
            network: AccessLevel::Write,
            audit_log: AccessLevel::Write,
        };
        let mask = CapabilitySet {
            db: AccessLevel::Read,
            search_index: AccessLevel::None,
            graph_snapshot: AccessLevel::Write,
            cass_subprocess: AccessLevel::Read,
            filesystem: AccessLevel::None,
            network: AccessLevel::None,
            audit_log: AccessLevel::Write,
        };
        let narrowed = original.narrow(mask);
        assert_eq!(narrowed.db, AccessLevel::Read);
        assert_eq!(narrowed.search_index, AccessLevel::None);
        assert_eq!(narrowed.graph_snapshot, AccessLevel::Write);
        assert_eq!(narrowed.cass_subprocess, AccessLevel::Read);
        assert_eq!(narrowed.filesystem, AccessLevel::None);
        assert_eq!(narrowed.network, AccessLevel::None);
        assert_eq!(narrowed.audit_log, AccessLevel::Write);
    }

    #[test]
    fn narrow_is_monotone_and_never_widens() {
        // Repeated narrowing is monotone non-increasing on every axis.
        let starting = CapabilitySet::full_local();
        let mask_a = CapabilitySet::read_only();
        let mask_b = CapabilitySet {
            db: AccessLevel::None,
            ..CapabilitySet::read_only()
        };
        let once = starting.narrow(mask_a);
        let twice = once.narrow(mask_b);

        // Sanity: once is read_only because full_local was at or above
        // read_only on every slot.
        assert_eq!(once, mask_a);
        // After narrowing again with mask_b (which zeros db), the db
        // axis must drop and no other axis may widen.
        assert!(twice.db <= once.db);
        assert!(twice.search_index <= once.search_index);
        assert!(twice.graph_snapshot <= once.graph_snapshot);
        assert!(twice.cass_subprocess <= once.cass_subprocess);
        assert!(twice.filesystem <= once.filesystem);
        assert!(twice.network <= once.network);
        assert!(twice.audit_log <= once.audit_log);
        assert_eq!(twice.db, AccessLevel::None);
    }

    #[test]
    fn narrow_property_holds_for_a_curated_corpus() {
        // Property restated as a deterministic table so the test runs
        // without a property-test crate dependency. Each row is
        // (initial, mask); for every row, narrow(initial, mask).slot
        // <= initial.slot && narrow(initial, mask).slot <= mask.slot.
        let levels = [AccessLevel::None, AccessLevel::Read, AccessLevel::Write];
        for db_a in levels {
            for db_b in levels {
                for fs_a in levels {
                    for fs_b in levels {
                        let initial = CapabilitySet {
                            db: db_a,
                            filesystem: fs_a,
                            ..CapabilitySet::full_local()
                        };
                        let mask = CapabilitySet {
                            db: db_b,
                            filesystem: fs_b,
                            ..CapabilitySet::full_local()
                        };
                        let narrowed = initial.narrow(mask);
                        assert!(narrowed.db <= initial.db);
                        assert!(narrowed.db <= mask.db);
                        assert!(narrowed.filesystem <= initial.filesystem);
                        assert!(narrowed.filesystem <= mask.filesystem);
                    }
                }
            }
        }
    }

    #[test]
    fn command_context_exposes_workspace_and_budget() {
        let context = ctx(CapabilitySet::read_only());
        assert_eq!(
            context.workspace_root(),
            PathBuf::from("/tmp/ee-test-workspace")
        );
        assert!(context.budget().remaining_wall_clock().is_none());
        assert_eq!(context.capabilities(), CapabilitySet::read_only());
    }

    #[test]
    fn budget_mut_lets_handlers_record_consumption() {
        let mut context = ctx(CapabilitySet::read_only());
        context.budget_mut().record_tokens(42);
        context.budget_mut().record_io_bytes(1024);
        assert_eq!(context.budget().tokens_used(), 42);
        assert_eq!(context.budget().io_used_bytes(), 1024);
    }

    // Bead bd-17c65.1.3 (A3) — per-item `why` is a one-line actionable
    // reason, not the old 350-char math identity. The math identity
    // (unit_score(field) = clamp(field, 0.0, 1.0)) applies uniformly to
    // every item and is emitted once at pack.meta.algorithm.scoringFormula.

    #[test]
    fn candidate_selection_why_is_one_line_reason() {
        let why = candidate_selection_why("prepare release", "lexical", 0.812_34, 0.456_78, None);
        // Compact single-line shape with the same numerical content as
        // the old paragraph.
        assert_eq!(
            why,
            "matched 'prepare release' via lexical (relevance 0.8123, utility 0.4568)"
        );
    }

    #[test]
    fn candidate_selection_why_appends_artifact_provenance() {
        let why = candidate_selection_why(
            "prepare release",
            "hybrid",
            0.912_34,
            0.556_78,
            Some("art_0123456789abcdef01234567"),
        );
        assert_eq!(
            why,
            "matched 'prepare release' via hybrid (relevance 0.9123, utility 0.5568); via registered artifact art_0123456789abcdef01234567"
        );
    }

    #[test]
    fn candidate_selection_why_truncates_long_queries() {
        let long_query = "abcdefghij".repeat(15); // 150 chars
        let why = candidate_selection_why(&long_query, "lexical", 0.5, 0.5, None);
        // Truncation marker present; total why stays under 200 chars
        // (well below the bead's 120-char per-item target — extra
        // room for the source + scores).
        assert!(why.contains("..."));
        assert!(why.len() < 200, "got {} chars: {why}", why.len());
    }

    #[test]
    fn candidate_selection_why_excludes_qualitative_terms() {
        // AGENTS.md determinism principle: no "believes", "thinks", etc.
        let why = candidate_selection_why("prepare release", "lexical", 0.812_34, 0.456_78, None);
        let lower = why.to_ascii_lowercase();
        for forbidden in [
            "believes",
            "understands",
            "intends",
            "inferred intent",
            "story",
        ] {
            assert!(
                !lower.contains(forbidden),
                "why used qualitative term `{forbidden}`: {why}"
            );
        }
    }

    #[test]
    fn candidate_selection_why_per_item_size_is_compact() {
        // Lock in the token-savings target: per-item why ≤ 120 chars
        // for typical queries. The old form averaged ~350 chars.
        let why = candidate_selection_why(
            "how do I cut a release safely",
            "semantic_fast",
            0.149,
            0.5,
            None,
        );
        assert!(
            why.len() < 120,
            "per-item why exceeds 120 char budget: {} chars\n  {why}",
            why.len()
        );
    }

    #[test]
    fn focus_candidate_why_declares_passive_context_influence() -> Result<(), String> {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(44));
        let mut state = FocusState::new(
            WorkspaceId::from_uuid(uuid::Uuid::from_u128(1)),
            3,
            "2026-05-04T00:00:00Z",
        )
        .map_err(|error| error.to_string())?
        .with_focal_memory_id(memory_id);
        let item = FocusItem::new(
            memory_id,
            "Resume the failing test context.",
            "2026-05-04T00:00:00Z",
        )
        .map_err(|error| error.to_string())?
        .pinned(true)
        .with_provenance("ee focus set");
        state = state
            .with_item(item.clone())
            .map_err(|error| error.to_string())?;

        let why = focus_candidate_why(&item, &state, "blake3:test");
        assert!(why.contains("focus_state_hash=blake3:test"), "{why}");
        assert!(why.contains("focal=true"), "{why}");
        assert!(why.contains("pinned=true"), "{why}");
        assert!(why.contains("source=ee_focus_state"), "{why}");
        assert!(why.contains("no hidden mutation"), "{why}");
        assert!(why.contains("agent-plan inference"), "{why}");

        let relevance = focus_relevance(&item, &state).map(|score| score.into_inner());
        assert_eq!(relevance, Some(1.0));
        Ok(())
    }

    #[test]
    fn unit_score_clamps_non_finite_and_bounds() {
        assert!(
            matches!(unit_score(-0.25), Some(score) if (score.into_inner() - 0.0).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(0.50), Some(score) if (score.into_inner() - 0.50).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(1.25), Some(score) if (score.into_inner() - 1.0).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(f32::NAN), Some(score) if (score.into_inner() - 0.0).abs() <= f32::EPSILON)
        );
        assert!(
            matches!(unit_score(f32::INFINITY), Some(score) if (score.into_inner() - 0.0).abs() <= f32::EPSILON)
        );
    }

    #[test]
    fn with_narrowed_capabilities_preserves_workspace_and_budget() {
        let mut context = ctx(CapabilitySet::full_local());
        context.budget_mut().record_tokens(7);
        let narrowed = context.with_narrowed_capabilities(CapabilitySet::read_only());

        // Capabilities narrowed.
        assert_eq!(narrowed.capabilities().db, AccessLevel::Read);
        assert_eq!(narrowed.capabilities().filesystem, AccessLevel::Read);
        // Workspace identity preserved.
        assert_eq!(narrowed.workspace_root(), context.workspace_root());
        // Budget state preserved (tokens recorded before narrow are
        // still recorded after narrow).
        assert_eq!(narrowed.budget().tokens_used(), 7);
    }

    #[test]
    fn with_narrowed_capabilities_composes() {
        let context = ctx(CapabilitySet::full_local());
        let mask_a = CapabilitySet::read_only();
        let mask_b = CapabilitySet {
            db: AccessLevel::None,
            ..CapabilitySet::read_only()
        };
        // narrow(narrow(c, mask_a), mask_b) == narrow(c, narrow(mask_a, mask_b))
        let chained = context
            .with_narrowed_capabilities(mask_a)
            .with_narrowed_capabilities(mask_b);
        let combined = context.with_narrowed_capabilities(mask_a.narrow(mask_b));
        assert_eq!(chained.capabilities(), combined.capabilities());
    }

    #[test]
    fn pack_hash_includes_content_provenance_and_degradation() -> Result<(), String> {
        use super::{
            ContextPackOutputOptions, ContextPackOutputProfile, ContextResponseDegradation,
            ContextResponseSeverity, compute_pack_hash, compute_pack_hash_with_output_options,
        };
        use crate::models::{ProvenanceUri, TrustClass, UnitScore};
        use crate::pack::{
            ContextRequest, PackDraft, PackDraftItem, PackOmission, PackOmissionReason,
            PackProvenance, PackRejectionStage, PackSection, PackSelectionAudit,
            PackSelectionObjective, PackSelectionPhase, PackTrustSignal, TokenBudget,
        };

        let request =
            ContextRequest::from_query("test query").map_err(|error| error.to_string())?;

        let mem_a = MemoryId::from_uuid(uuid::Uuid::from_u128(1));
        let mem_b = MemoryId::from_uuid(uuid::Uuid::from_u128(2));
        let mem_c = MemoryId::from_uuid(uuid::Uuid::from_u128(3));
        let mem_d = MemoryId::from_uuid(uuid::Uuid::from_u128(4));
        let budget = TokenBudget::default_context();

        let base_item = PackDraftItem {
            rank: 1,
            memory_id: mem_a,
            section: PackSection::ProceduralRules,
            content: "original content".to_string(),
            estimated_tokens: 10,
            relevance: crate::models::UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            utility: crate::models::UnitScore::parse(0.7).map_err(|error| error.to_string())?,
            proximity_to_seed: None,
            score_breakdown: None,
            provenance: vec![
                PackProvenance::new(ProvenanceUri::EeMemory(mem_b), "source note")
                    .map_err(|error| error.to_string())?,
            ],
            why: "test explanation".to_string(),
            diversity_key: None,
            trust: PackTrustSignal::new(TrustClass::AgentAssertion, None),
            redactions: Vec::new(),
            tombstoned_at: None,
            lifecycle: None,
            selected_in: PackSelectionPhase::StrictMmr,
        };

        let base_draft = PackDraft {
            query: "test query".to_string(),
            budget,
            used_tokens: 10,
            items: vec![base_item.clone()],
            omitted: vec![],
            selection_audit: PackSelectionAudit {
                profile: request.profile,
                objective: PackSelectionObjective::MmrRedundancy,
                algorithm_id: "test_deterministic_selection",
                algorithm_description: "Test-only deterministic selection audit.",
                candidate_count: 1,
                selected_count: 1,
                omitted_count: 0,
                budget_limit: budget.max_tokens(),
                budget_used: 10,
                total_objective_value: 1.0,
                monotone: false,
                submodular: false,
                selected_items: Vec::new(),
                steps: Vec::new(),
            },
            hash: None,
        };

        let base_degraded: Vec<ContextResponseDegradation> = vec![];

        let hash_base = compute_pack_hash(&request, &base_draft, &base_degraded);
        let hash_lean = compute_pack_hash_with_output_options(
            &request,
            &base_draft,
            &base_degraded,
            ContextPackOutputOptions::for_profile(ContextPackOutputProfile::Lean),
        );
        assert_ne!(
            hash_base, hash_lean,
            "pack hash must include output-profile field omissions"
        );
        let hash_swarm_heavy = compute_pack_hash_with_output_options(
            &request,
            &base_draft,
            &base_degraded,
            ContextPackOutputOptions::default()
                .with_resource_profile(crate::pack::PackResourceProfile::SwarmHeavy),
        );
        assert_ne!(
            hash_base, hash_swarm_heavy,
            "pack hash must include resource-profile SLO output"
        );
        let rendered_base =
            crate::pack::render_context_markdown(&request, &base_draft, &base_degraded);
        assert!(
            rendered_base.contains("original content"),
            "pack hash fixture should render item content into markdown text"
        );

        // Different content produces different hash.
        let mut draft_content = base_draft.clone();
        draft_content.items[0].content = "different content".to_string();
        let hash_content = compute_pack_hash(&request, &draft_content, &base_degraded);
        let rendered_content =
            crate::pack::render_context_markdown(&request, &draft_content, &base_degraded);
        assert_ne!(
            rendered_base, rendered_content,
            "rendered pack text change must be visible to the hash input"
        );
        assert_ne!(hash_base, hash_content, "content change must alter hash");

        // Different provenance produces different hash.
        let mut draft_provenance = base_draft.clone();
        draft_provenance.items[0].provenance = vec![
            PackProvenance::new(ProvenanceUri::EeMemory(mem_c), "different source")
                .map_err(|error| error.to_string())?,
        ];
        let hash_provenance = compute_pack_hash(&request, &draft_provenance, &base_degraded);
        assert_ne!(
            hash_base, hash_provenance,
            "provenance change must alter hash"
        );

        // Different why explanation produces different hash.
        let mut draft_why = base_draft.clone();
        draft_why.items[0].why = "different explanation".to_string();
        let hash_why = compute_pack_hash(&request, &draft_why, &base_degraded);
        assert_ne!(hash_base, hash_why, "why change must alter hash");

        // Different trust signal produces different hash.
        let mut draft_trust = base_draft.clone();
        draft_trust.items[0].trust =
            PackTrustSignal::new(TrustClass::AgentValidated, Some("verified".to_string()));
        let hash_trust = compute_pack_hash(&request, &draft_trust, &base_degraded);
        assert_ne!(hash_base, hash_trust, "trust change must alter hash");

        // Different omissions produce different hash.
        let mut draft_omission = base_draft.clone();
        draft_omission.omitted = vec![PackOmission {
            memory_id: mem_d,
            estimated_tokens: 50,
            relevance: UnitScore::parse(0.5).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.4).map_err(|error| error.to_string())?,
            reason: PackOmissionReason::TokenBudgetExceeded,
            rejected_at: PackRejectionStage::Selection,
            feasible: false,
            could_fit_with_budget: Some(60),
        }];
        let hash_omission = compute_pack_hash(&request, &draft_omission, &base_degraded);
        assert_ne!(hash_base, hash_omission, "omission change must alter hash");

        // Different degradations produce different hash.
        let degraded_with_issue = vec![ContextResponseDegradation {
            code: "test_degradation".to_string(),
            severity: ContextResponseSeverity::Medium,
            message: "Something degraded".to_string(),
            repair: Some("ee fix something".to_string()),
        }];
        let hash_degraded = compute_pack_hash(&request, &base_draft, &degraded_with_issue);
        assert_ne!(
            hash_base, hash_degraded,
            "degradation change must alter hash"
        );
        let degraded_with_two_issues = vec![
            ContextResponseDegradation {
                code: "search_index_stale".to_string(),
                severity: ContextResponseSeverity::Medium,
                message: "Search index is stale.".to_string(),
                repair: Some("ee index rebuild --workspace .".to_string()),
            },
            ContextResponseDegradation {
                code: "low_recall_after_floor".to_string(),
                severity: ContextResponseSeverity::Low,
                message: "Only one candidate passed the relevance floor.".to_string(),
                repair: Some("broaden query".to_string()),
            },
        ];
        let hash_degraded_two = compute_pack_hash(&request, &base_draft, &degraded_with_two_issues);
        assert_ne!(
            hash_degraded, hash_degraded_two,
            "distinct degradation lists must produce distinct hashes"
        );

        for (label, degraded) in [
            ("empty", base_degraded.as_slice()),
            ("one", degraded_with_issue.as_slice()),
            ("two", degraded_with_two_issues.as_slice()),
        ] {
            let first = compute_pack_hash(&request, &base_draft, degraded);
            let second = compute_pack_hash(&request, &base_draft, degraded);
            let third = compute_pack_hash(&request, &base_draft, degraded);
            assert_eq!(
                first, second,
                "fixed pack hash input should reproduce for {label} degraded entries"
            );
            assert_eq!(
                second, third,
                "fixed pack hash input should reproduce across a third call for {label} degraded entries"
            );
        }

        // Same inputs produce same hash (determinism check).
        let hash_repeat = compute_pack_hash(&request, &base_draft, &base_degraded);
        assert_eq!(hash_base, hash_repeat, "same inputs must produce same hash");
        Ok(())
    }

    #[test]
    fn pack_l2_cache_key_tracks_canonical_inputs() -> Result<(), String> {
        use super::{ContextPackOutputOptions, PackL2CacheKeyInput, compute_pack_l2_cache_key};
        use crate::models::{MemoryScope, RedactionLevel};
        use crate::pack::{
            ContextPackProfile, ContextRequest, ContextRequestInput, PackResourceProfile,
            PackSection,
        };

        let request = ContextRequest::new(ContextRequestInput {
            query: " prepare release ".to_string(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(4_000),
            candidate_pool: Some(64),
            max_results: Some(12),
            sections: vec![PackSection::ProceduralRules, PackSection::Evidence],
        })
        .map_err(|error| error.to_string())?;
        let base = PackL2CacheKeyInput {
            workspace_id: "wsp_test_001".to_string(),
            database_generation: 10,
            index_generation: 20,
            graph_generation: Some(30),
            redaction_level: RedactionLevel::Standard,
            request,
            output_options: ContextPackOutputOptions::default()
                .with_resource_profile(PackResourceProfile::SwarmHeavy),
            memory_scope: MemoryScope::Swarm,
            strict_scope: true,
            context_feature_flags_hash: "blake3:features-a".to_string(),
            personalization_generation: Some(40),
        };

        let key = compute_pack_l2_cache_key(&base);
        assert!(
            key.starts_with("blake3:"),
            "L2 cache key should use the existing BLAKE3 key prefix"
        );
        assert_eq!(
            key,
            compute_pack_l2_cache_key(&base),
            "same canonical inputs must reproduce the same key"
        );

        let mut changed_query = base.clone();
        changed_query.request = ContextRequest::new(ContextRequestInput {
            query: "prepare hotfix".to_string(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(4_000),
            candidate_pool: Some(64),
            max_results: Some(12),
            sections: vec![PackSection::ProceduralRules, PackSection::Evidence],
        })
        .map_err(|error| error.to_string())?;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_query),
            "normalized query changes must alter the L2 key"
        );

        let mut changed_profile = base.clone();
        changed_profile.request = ContextRequest::new(ContextRequestInput {
            query: "prepare release".to_string(),
            profile: Some(ContextPackProfile::Thorough),
            max_tokens: Some(4_000),
            candidate_pool: Some(64),
            max_results: Some(12),
            sections: vec![PackSection::ProceduralRules, PackSection::Evidence],
        })
        .map_err(|error| error.to_string())?;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_profile),
            "context profile changes must alter the L2 key"
        );

        let mut changed_tokens = base.clone();
        changed_tokens.request = ContextRequest::new(ContextRequestInput {
            query: "prepare release".to_string(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(2_000),
            candidate_pool: Some(64),
            max_results: Some(12),
            sections: vec![PackSection::ProceduralRules, PackSection::Evidence],
        })
        .map_err(|error| error.to_string())?;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_tokens),
            "max token budget changes must alter the L2 key"
        );

        let mut changed_redaction = base.clone();
        changed_redaction.redaction_level = RedactionLevel::Strict;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_redaction),
            "redaction level changes must alter the L2 key"
        );

        for (label, changed) in [
            ("database generation", {
                let mut changed = base.clone();
                changed.database_generation = 11;
                changed
            }),
            ("index generation", {
                let mut changed = base.clone();
                changed.index_generation = 21;
                changed
            }),
            ("graph generation", {
                let mut changed = base.clone();
                changed.graph_generation = Some(31);
                changed
            }),
            ("personalization generation", {
                let mut changed = base.clone();
                changed.personalization_generation = Some(41);
                changed
            }),
            ("feature flag set hash", {
                let mut changed = base.clone();
                changed.context_feature_flags_hash = "blake3:features-b".to_string();
                changed
            }),
        ] {
            assert_ne!(
                key,
                compute_pack_l2_cache_key(&changed),
                "{label} changes must alter the L2 key"
            );
        }

        Ok(())
    }

    #[test]
    fn persist_pack_record_preserves_item_provenance_and_trust() -> Result<(), String> {
        use std::path::Path;
        use std::str::FromStr;

        use super::{compute_pack_hash, persist_pack_record};
        use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
        use crate::models::{ProvenanceUri, TrustClass, UnitScore};
        use crate::pack::{
            ContextRequest, PackCandidate, PackCandidateInput, PackProvenance, PackSection,
            PackTrustSignal, TokenBudget, assemble_draft, pack_item_provenance_json,
        };

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_01234567890123456789088888";
        let workspace_path = "/tmp/ee-context-persist-signals";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string(),
                    name: Some("context persist signals".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(88));
        connection
            .insert_memory(
                &memory_id.to_string(),
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Run cargo fmt before release.".to_string(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("file://AGENTS.md#L42".to_string()),
                    trust_class: TrustClass::AgentValidated.as_str().to_string(),
                    trust_subclass: Some("reviewed".to_string()),
                    tags: vec!["release".to_string()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let provenance = vec![
            PackProvenance::new(
                ProvenanceUri::from_str("file://AGENTS.md#L42")
                    .map_err(|error| error.to_string())?,
                "project rule source",
            )
            .map_err(|error| error.to_string())?,
            PackProvenance::new(
                ProvenanceUri::from_str("cass-session://session-a#L20-22")
                    .map_err(|error| error.to_string())?,
                "session confirmation",
            )
            .map_err(|error| error.to_string())?,
        ];
        let candidate = PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: "Run cargo fmt before release.".to_string(),
            estimated_tokens: 9,
            relevance: UnitScore::parse(0.95).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            provenance: provenance.clone(),
            why: "Selected because the task is release formatting.".to_string(),
        })
        .map_err(|error| error.to_string())?
        .with_trust_signal(PackTrustSignal::new(
            TrustClass::AgentValidated,
            Some("reviewed".to_string()),
        ));
        let request =
            ContextRequest::from_query("prepare release").map_err(|error| error.to_string())?;
        let mut draft = assemble_draft(
            "prepare release",
            TokenBudget::default_context(),
            [candidate],
        )
        .map_err(|error| error.to_string())?;
        draft.hash = Some(compute_pack_hash(&request, &draft, &[]));

        persist_pack_record(
            &connection,
            Path::new(workspace_path),
            &request,
            &draft,
            &[],
        )?;

        let history = connection
            .list_pack_records_for_memory(&memory_id.to_string(), 10)
            .map_err(|error| error.to_string())?;
        assert_eq!(history.len(), 1);
        let stored_item = &history[0].1;
        assert_eq!(
            stored_item.provenance_json,
            pack_item_provenance_json(&provenance)
        );
        assert_eq!(stored_item.trust_class, "agent_validated");
        assert_eq!(stored_item.trust_subclass.as_deref(), Some("reviewed"));

        connection.close().map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn persist_pack_record_seeded_replays_pack_id() -> Result<(), String> {
        use std::path::Path;

        use super::{compute_pack_hash, persist_pack_record_seeded};
        use crate::db::{CreateWorkspaceInput, DbConnection};
        use crate::pack::{ContextRequest, PackCandidate, TokenBudget, assemble_draft};
        use crate::runtime::determinism::Deterministic;

        fn persisted_pack_id(seed: u64) -> Result<String, String> {
            let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            let workspace_path = "/tmp/ee-context-seeded-pack-id";
            connection
                .insert_workspace(
                    "wsp_01234567890123456789077777",
                    &CreateWorkspaceInput {
                        path: workspace_path.to_string(),
                        name: Some("seeded pack id".to_string()),
                    },
                )
                .map_err(|error| error.to_string())?;

            let request =
                ContextRequest::from_query("seeded pack id").map_err(|error| error.to_string())?;
            let mut draft = assemble_draft(
                "seeded pack id",
                TokenBudget::default_context(),
                Vec::<PackCandidate>::new(),
            )
            .map_err(|error| error.to_string())?;
            draft.hash = Some(compute_pack_hash(&request, &draft, &[]));

            let determinism = Deterministic::from_seed(seed);
            let pack_id = persist_pack_record_seeded(
                &connection,
                Path::new(workspace_path),
                &request,
                &draft,
                &[],
                &determinism,
            )?;
            let stored = connection
                .get_pack_record(&pack_id)
                .map_err(|error| error.to_string())?;
            assert!(stored.is_some(), "seeded pack record should be stored");
            connection.close().map_err(|error| error.to_string())?;
            Ok(pack_id)
        }

        let first = persisted_pack_id(77)?;
        let replay = persisted_pack_id(77)?;
        let other_seed = persisted_pack_id(78)?;

        assert_eq!(first, replay);
        assert_ne!(first, other_seed);
        assert!(first.starts_with("pack_"));
        Ok(())
    }
}
