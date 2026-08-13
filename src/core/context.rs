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

#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
#[cfg(unix)]
use rustix::fs::{FlockOperation, flock};
#[cfg(unix)]
use rustix::io::Errno;
use sqlmodel_core::Value as SqlValue;

use crate::cache::hotset::{
    MemoryStorageTier, MemoryTierAssignment, MemoryTierInput, MemoryTierPolicyConfig,
    assign_memory_storage_tiers,
};
use crate::cache::pack_l2::{
    DEFAULT_MAX_BYTES as PACK_L2_DEFAULT_MAX_BYTES, PackL2Cache, PackL2CacheError,
    PackL2CacheLookup, PackL2CacheMiss, PackL2CacheMissReason, PackL2CacheOptions,
    PackL2CompressionHit, PackL2WriteOutcome,
};
use crate::config::{
    ConfigFile, EnvVar, GRAPH_FEATURE_PACK_DNA_ENABLED_KEY, GRAPH_FEATURE_PPR_ENABLED_KEY,
    GRAPH_FEATURE_PROXIMITY_ENABLED_KEY, GRAPH_PACK_DNA_MAX_EDGES_KEY,
    GRAPH_PACK_DNA_MAX_ITEMS_KEY, ReadPoolConfig, WorkspaceLocation, env_var_is_set,
    parse_env_bool_flag, read_env_var,
};
use crate::core::budget::RequestBudget;
use crate::core::focus::{focus_state_hash, focus_state_path, read_active_focus_state};
use crate::core::index::{
    index_corpus_compatibility_is_current, prepare_search_embedder_for_workspace,
};
use crate::core::memory_drift::{MemoryDriftSelectionHint, memory_drift_selection_hint_for_memory};
use crate::core::memory_scope::{
    MemoryScopeContext, MeshDisplayProvenance, MeshQueryVisibility, mesh_query_visibility,
};
use crate::core::profile::{RuntimeProfileReport, runtime_profile_for_workspace};
use crate::core::search::{
    PERFORMANCE_EXPLAIN_SCHEMA_V1, ScoreSource, SearchAdvisoryDeliveryReservation,
    SearchAdvisorySession, SearchDegradation, SearchError, SearchHit, SearchOptions,
    SearchPerformanceTrace, SearchReport, SearchSourceMode, SearchStatus,
    SearchWorkspaceProbeState, elapsed_timing_json, map_frankensearch_error,
    performance_redaction_json, query_observation_json, reconcile_search_index_before_read_with_cx,
    resolve_search_rerank_runtime_posture, run_context_search_with_preloaded_memories,
    run_context_search_with_preloaded_memories_and_workspace_state_with_cx,
    search_advisory_snapshot_data_json_with_delivery_reservation, search_degraded_data_json,
};
use crate::db::read_pool::{
    PoolConfig, PoolStats, READ_POOL_ACQUIRE_TIMEOUT_CODE, READ_POOL_UNDERSIZED_CODE,
    READ_POOL_UNDERSIZED_P99_THRESHOLD, READ_POOL_UNDERSIZED_SAMPLE_FLOOR, ReadConnectionPool,
    SnapshotPin, SnapshotPinMetadata, registered_process_read_pool,
};
use crate::db::{
    CreatePackBaselineInput, CreatePackEvidenceItemInput, CreatePackItemInput,
    CreatePackOmissionInput, CreatePackRecordInput, CreatePackTaskLensInput, DatabaseConfig,
    DbConnection, PackRecordInsertTimings, StoredAgentContextProfileForPack, StoredMemory,
};
use crate::models::degradation::{
    GRAPH_PACK_DNA_TIMEOUT_CODE, GRAPH_PPR_EMPTY_SEED_SET_CODE, GRAPH_PPR_SNAPSHOT_STALE_CODE,
};
use crate::models::{
    AGENT_CONTEXT_PROFILE_SCHEMA_V1, AGENT_PROFILE_BIAS_CAP, AGENT_PROFILE_COLD_START_OUTCOMES,
    AgentContextProfileCounts, EmbedBackend, EvidenceId, GLOBAL_MEMORY_SCOPE_TAG, MemoryId,
    MemoryScope, MemoryScopeStats, MemorySentinelResultStatus, PACK_SCHEMA_V2, PackId,
    ProvenanceUri, RedactionLevel, RuleId, TrustClass, UnitScore, WorkspaceId,
    posture_for_trust_class,
};
use crate::pack::{
    ConflictKind, ConflictRecommendedAction, ConsensusConflictReport, ContextPackProfile,
    ContextRequest, ContextRequestInput, ContextResponse, ContextResponseDegradation,
    ContextResponsePagination, ContextResponseSeverity, PACK_ATTEMPT_FAMILY_MULTIPLICITY_SCHEMA_V1,
    PACK_COMMAND, PackAdmissionPosture, PackAssemblySlo, PackAssemblySloActuals,
    PackAttemptFamilyMembershipSnapshot, PackAttemptFamilyMultiplicitySnapshot, PackCandidate,
    PackCandidateInput, PackCoordinationSnapshot, PackDraft, PackDraftItem, PackEvidenceItem,
    PackFreshnessAnchorFacet, PackFreshnessFacet, PackItemLifecycle, PackOmission,
    PackOmissionReason, PackProvenance, PackRejectionStage, PackResourceProfile,
    PackScoreBreakdown, PackSection, PackTrustSignal, TokenBudget, WhyNotSelectedInput,
    WhyNotSelectedReport, assemble_draft_with_profile_and_options_seeded,
    budget_classifier::{AdaptiveBudgetDecision, AdaptiveBudgetInput, classify_adaptive_budget},
    estimate_tokens_default, explain_why_not_selected, pack_item_provenance_json,
    redact_pack_provenance_text,
};
use crate::runtime::determinism::{Deterministic, Seed};
use crate::search::RuleIndexProjection;
use crate::util::radix_ulid_sort::sort_by_ulid_payload_or_lexical;

static PACK_HASH_LOG_RUN_INDEX: AtomicU64 = AtomicU64::new(0);
static PACK_SLOT_PROCESS_GATES: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
static CONTEXT_PROXIMITY_TREE_CACHE: OnceLock<RwLock<Option<CachedContextProximityTree>>> =
    OnceLock::new();
const PACK_SLOT_RETRY_AFTER_MS: u64 = 250;
#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
pub(crate) const PACK_L2_CACHE_KEY_SCHEMA_V4: &str = "ee.pack.l2_cache_key.v4";
const PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3: &str = "ee.pack.l2_context_response.v3";
const CONTEXT_SEARCH_ADVISORY_SNAPSHOT_SCHEMA_V1: &str = "ee.context.search_advisory_snapshot.v1";
pub const DEFAULT_CONTEXT_PPR_WEIGHT: f32 = 0.30;
const CONTEXT_CHANGED_SYMBOL_BOOST: f32 = 0.05;
const CONTEXT_MEMORY_TIER_HOT_BOOST: f32 = 0.025;
const CONTEXT_MEMORY_TIER_WARM_BOOST: f32 = 0.010;
const CONTEXT_CHANGED_SYMBOL_ADJACENCY_LINE_WINDOW: u32 = 20;

#[derive(Clone, Debug)]
struct CachedContextProximityTree {
    generation: u64,
    tree: Arc<crate::graph::gomory_hu::GomoryHuTree>,
}

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
    Acquired {
        guard: PackSlotGuard,
        queue_depth: usize,
        concurrent_pack_max: usize,
    },
    LimitReached {
        retry_after_ms: u64,
        queue_depth: usize,
        concurrent_pack_max: usize,
    },
    Unavailable {
        path: PathBuf,
        message: String,
    },
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

    let mut queue_depth = 0_usize;
    for slot_index in 0..budget.concurrent_pack_max {
        let slot_path = slots_dir.join(format!("{}-{slot_index:02}.lock", profile.as_str()));
        if let Err(message) = ensure_pack_slot_path_is_not_symlink(&slot_path, "pack slot lock") {
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message,
            };
        }
        if let Err(message) =
            ensure_pack_slot_path_is_regular_or_missing(&slot_path, "pack slot lock")
        {
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message,
            };
        }
        if !try_acquire_pack_slot_process_gate(&slot_path) {
            queue_depth = queue_depth.saturating_add(1);
            continue;
        }
        if let Err(message) = ensure_pack_slot_path_is_not_symlink(&slot_path, "pack slot lock") {
            release_pack_slot_process_gate(&slot_path);
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message,
            };
        }
        if let Err(message) =
            ensure_pack_slot_path_is_regular_or_missing(&slot_path, "pack slot lock")
        {
            release_pack_slot_process_gate(&slot_path);
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message,
            };
        }

        let file = match open_pack_slot_lock_file(&slot_path) {
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
                queue_depth = queue_depth.saturating_add(1);
                continue;
            }
            return PackSlotAcquisition::Unavailable {
                path: slot_path,
                message: format!("Failed to acquire pack slot lock: {error}"),
            };
        }

        return PackSlotAcquisition::Acquired {
            guard: PackSlotGuard {
                path: slot_path,
                _file: file,
            },
            queue_depth,
            concurrent_pack_max: budget.concurrent_pack_max,
        };
    }

    PackSlotAcquisition::LimitReached {
        retry_after_ms: PACK_SLOT_RETRY_AFTER_MS,
        queue_depth,
        concurrent_pack_max: budget.concurrent_pack_max,
    }
}

fn open_pack_slot_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    configure_pack_slot_lock_options(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_pack_slot_lock_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_pack_slot_lock_options(_options: &mut OpenOptions) {}

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

fn ensure_pack_slot_path_is_regular_or_missing(path: &Path, path_type: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(format!(
            "Refusing to use {} '{}': path is not a regular file",
            path_type,
            path.display()
        )),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(format!(
            "Failed to inspect {} '{}': {error}",
            path_type,
            path.display()
        )),
    }
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
    /// `ee status`, `ee search`, `ee why`, and `ee pack`.
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

/// A command checkpoint can fail because an ee resource budget was exceeded
/// or because Asupersync cancelled the caller-owned task context.
///
/// Keeping the two cases typed prevents a user, deadline, or parent
/// cancellation from being rewritten as a fabricated wall-clock budget
/// breach at the CLI boundary.
#[derive(Clone, Debug)]
pub enum CommandCancellation {
    BudgetExceeded(crate::core::budget::BudgetExceeded),
    Cancelled(asupersync::CancelReason),
}

impl std::fmt::Display for CommandCancellation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExceeded(error) => std::fmt::Display::fmt(error, formatter),
            Self::Cancelled(reason) => {
                formatter.write_str(&crate::core::outcome::cancel_message(reason))
            }
        }
    }
}

impl std::error::Error for CommandCancellation {}

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

    /// Checks the request budget and the Cx cooperative-cancellation signal.
    ///
    /// Resource-budget exhaustion wins when both signals are already set, but
    /// an Asupersync cancellation otherwise retains its complete
    /// [`asupersync::CancelReason`] provenance.
    pub fn check_cancellation(&self, cx: &asupersync::Cx) -> Result<(), CommandCancellation> {
        self.budget
            .check()
            .map_err(CommandCancellation::BudgetExceeded)?;
        cx.checkpoint().map_err(|_| {
            CommandCancellation::Cancelled(cx.cancel_reason().unwrap_or_else(|| {
                crate::core::outcome::attributed_cancel_reason(
                    cx,
                    asupersync::CancelKind::User,
                    "command checkpoint cancelled without a recorded reason",
                )
            }))
        })
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextTaskLens {
    pub id: String,
    pub version: u32,
    pub lens_hash: String,
}

#[derive(Clone, Debug)]
pub struct ContextPackOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub query: String,
    pub speed: crate::search::SpeedMode,
    pub source_mode: crate::core::search::SearchSourceMode,
    pub strict_source_mode: bool,
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
    pub relevance_floor: Option<f32>,
    pub redaction_level: crate::models::RedactionLevel,
    pub memory_scope: MemoryScope,
    pub strict_scope: bool,
    pub ppr_weight: Option<f32>,
    pub changed_symbols: Vec<String>,
    pub changed_symbols_from_git: bool,
    pub pagination: Option<ContextPagination>,
    pub coordination_snapshot_path: Option<PathBuf>,
    pub coordination_stale_after_ms: u64,
    pub task_lens: Option<ContextTaskLens>,
    pub require_fresh_sentinels: bool,
    pub output_options: ContextPackOutputOptions,
    pub persist_pack: bool,
    /// bd-7lvbg.6: when set, a per-agent baseline row is recorded after
    /// the pack persists, making `--since last` resolvable next session.
    /// `None` (no agent identity, `--no-baseline-write`, or any read-only
    /// path) writes nothing.
    pub baseline_write: Option<PackBaselineWrite>,
    /// bd-1n0np.5.8 (E5): when `true` (via `pack --no-lod`), the
    /// level-of-detail tiering is disabled and the pack is assembled with
    /// `lod_budget_shares: None` — the legacy flat selector that places
    /// every selected candidate at the `Full` tier. This reproduces
    /// pre-LOD packs byte-for-byte (zero `truncated_preview`/`link_only`
    /// budget ⇒ `has_compressed_tiers()` is false ⇒ heap selector), giving
    /// callers a deterministic escape hatch when LOD compression is
    /// undesirable. Defaults to `false` (LOD on, the post-bd-1n0np.5.2
    /// behavior).
    pub no_lod: bool,
}

/// One stored memory admitted through the context-pack policy path for a
/// bounded, recency-ordered caller such as `ee orient --fast`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AdmittedContextMemory {
    pub item: PackDraftItem,
    pub created_at: String,
    pub tags: Vec<String>,
}

/// Per-agent baseline ledger write request (bd-7lvbg.6): rides the pack
/// persistence chokepoint, so read-only / no-persist paths skip it for
/// free.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackBaselineWrite {
    /// Agent identity from `EE_AGENT_NAME`.
    pub agent_name: String,
    /// Optional task scope from `--task-key`.
    pub task_key: Option<String>,
}

/// In-code default for `[pack] baseline_ledger_max_rows` (bd-7lvbg.6).
pub const DEFAULT_PACK_BASELINE_LEDGER_MAX_ROWS: u32 = 32;

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
    pub cache_json_response: bool,
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
                cache_json_response: false,
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
                cache_json_response: false,
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
                cache_json_response: false,
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
            cache_json_response: self.cache_json_response,
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

    #[must_use]
    pub const fn with_cache_json_response(mut self, cache_json_response: bool) -> Self {
        self.cache_json_response = cache_json_response;
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

#[derive(Clone, Debug)]
pub struct ContextPackPerformanceRun {
    pub response: ContextResponse,
    pub performance: serde_json::Value,
    /// The authoritative search observation that produced this pack. Long-lived
    /// transports retain advisory delivery state and render this report only
    /// when the response is ready to be written. L2 cache hits retain the
    /// minimal authoritative observation required for the same delivery-aware
    /// rendering without repeating retrieval.
    pub(crate) search_report: Option<SearchReport>,
    pub(crate) search_advisory_snapshot: ContextSearchAdvisorySnapshot,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextSearchAdvisorySnapshot {
    rerank_configured_mode: crate::config::SearchRerankMode,
    rerank_configured_top_k: usize,
    rerank_runtime_available: bool,
    rerank_score_count: usize,
    degraded: Vec<SearchDegradation>,
}

impl ContextSearchAdvisorySnapshot {
    pub(crate) fn from_search_report(report: &SearchReport) -> Self {
        let rerank_score_count = report
            .data_json()
            .pointer("/rerank/rerankScoreCount")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        Self {
            rerank_configured_mode: report.rerank_configured_mode,
            rerank_configured_top_k: report.rerank_configured_top_k,
            rerank_runtime_available: report.rerank_runtime_available,
            rerank_score_count,
            degraded: report.degraded.clone(),
        }
    }

    fn cache_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": CONTEXT_SEARCH_ADVISORY_SNAPSHOT_SCHEMA_V1,
            "rerankConfiguredMode": self.rerank_configured_mode.as_str(),
            "rerankConfiguredTopK": self.rerank_configured_top_k,
            "rerankRuntimeAvailable": self.rerank_runtime_available,
            "rerankScoreCount": self.rerank_score_count,
            "degraded": self.degraded.iter().map(|entry| serde_json::json!({
                "code": entry.code,
                "severity": entry.severity,
                "message": entry.message,
                "repair": entry.repair,
            })).collect::<Vec<_>>(),
        })
    }

    fn refresh_rerank_posture_from(&mut self, current: &Self) {
        self.rerank_configured_mode = current.rerank_configured_mode;
        self.rerank_configured_top_k = current.rerank_configured_top_k;
        self.rerank_runtime_available = current.rerank_runtime_available;
        self.degraded
            .retain(|entry| entry.code != "rerank_model_unavailable");
        self.degraded.extend(
            current
                .degraded
                .iter()
                .filter(|entry| entry.code == "rerank_model_unavailable")
                .cloned(),
        );
    }

    fn from_current_rerank_posture(
        posture: crate::core::search::SearchRerankRuntimePosture,
    ) -> Self {
        Self {
            rerank_configured_mode: posture.configured_mode,
            rerank_configured_top_k: posture.configured_top_k,
            rerank_runtime_available: posture.runtime_available,
            rerank_score_count: 0,
            degraded: posture.degraded,
        }
    }

    fn from_cache_json(value: &serde_json::Value) -> Result<Self, String> {
        if value.get("schema").and_then(serde_json::Value::as_str)
            != Some(CONTEXT_SEARCH_ADVISORY_SNAPSHOT_SCHEMA_V1)
        {
            return Err(
                "L2 pack cache search advisory snapshot has an unexpected schema".to_owned(),
            );
        }
        let configured_mode = value
            .get("rerankConfiguredMode")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "L2 pack cache search advisory snapshot is missing rerankConfiguredMode".to_owned())?
            .parse::<crate::config::SearchRerankMode>()
            .map_err(|error| format!("L2 pack cache search advisory snapshot has an invalid rerankConfiguredMode: {error}"))?;
        let configured_top_k = value
            .get("rerankConfiguredTopK")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                "L2 pack cache search advisory snapshot is missing rerankConfiguredTopK".to_owned()
            })?;
        let runtime_available = value
            .get("rerankRuntimeAvailable")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                "L2 pack cache search advisory snapshot is missing rerankRuntimeAvailable"
                    .to_owned()
            })?;
        let rerank_score_count = value
            .get("rerankScoreCount")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                "L2 pack cache search advisory snapshot is missing rerankScoreCount".to_owned()
            })?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "L2 pack cache search advisory snapshot is missing degraded".to_owned())?
            .iter()
            .map(|entry| {
                let field = |name| {
                    entry
                        .get(name)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| format!("L2 pack cache search advisory snapshot degradation is missing {name}"))
                };
                let repair = match entry.get("repair") {
                    Some(value) if value.is_null() => None,
                    Some(value) => Some(value.as_str().ok_or_else(|| {
                        "L2 pack cache search advisory snapshot degradation repair is invalid".to_owned()
                    })?.to_owned()),
                    None => return Err("L2 pack cache search advisory snapshot degradation is missing repair".to_owned()),
                };
                Ok(SearchDegradation {
                    code: field("code")?,
                    severity: field("severity")?,
                    message: field("message")?,
                    repair,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            rerank_configured_mode: configured_mode,
            rerank_configured_top_k: configured_top_k,
            rerank_runtime_available: runtime_available,
            rerank_score_count,
            degraded,
        })
    }

    fn data_json_with_delivery_reservation(
        &self,
        session: &mut SearchAdvisorySession,
        workspace_id: &str,
        reservation: &mut SearchAdvisoryDeliveryReservation,
    ) -> serde_json::Value {
        search_advisory_snapshot_data_json_with_delivery_reservation(
            self.rerank_score_count,
            &self.degraded,
            self.rerank_configured_mode,
            self.rerank_configured_top_k,
            self.rerank_runtime_available,
            session,
            workspace_id,
            Some(reservation),
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ContextPerformanceTrace {
    db_open_count: usize,
    index_status_checks: usize,
    pack_record_writes: usize,
    read_snapshot: Option<ReadSnapshotTrace>,
    filter_input_count: usize,
    filtered_count: usize,
    focus_state_read_attempts: usize,
    focus_state_hits: usize,
    focus_candidate_count: usize,
    search: SearchPerformanceTrace,
    candidate_resolution: CandidateResolutionMetrics,
    pack_persistence: PackPersistenceSubspans,
    timings: Vec<PerformanceTiming>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReadSnapshotTrace {
    pinned: bool,
    slot_id: Option<u64>,
    snapshot_generation: Option<u64>,
    lease_held_ms: u64,
    expired: bool,
    poisoned: bool,
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
    tier_boosted_candidates: usize,
    tier_cold_candidates: usize,
    tier_required_cold_candidates: usize,
    converted_candidates: usize,
    skipped_candidates: usize,
    subspans: CandidateResolutionSubspans,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CandidateResolutionSubspans {
    hit_id_resolution: Duration,
    memory_id_dedupe: Duration,
    memory_tag_batch_load: Duration,
    filtering: Duration,
    freshness_provenance: Duration,
    candidate_construction: Duration,
    graph_hints: Duration,
    scoring_ordering: Duration,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PackPersistenceSubspans {
    attempted: bool,
    succeeded: bool,
    item_count: usize,
    omission_count: usize,
    item_write_batches: usize,
    omission_write_batches: usize,
    connection_open: Duration,
    workspace_lookup: Duration,
    pack_hash: Duration,
    degraded_serialization: Duration,
    item_input_build: Duration,
    omission_input_build: Duration,
    ledger_serialization: Duration,
    transaction: Duration,
    record_write: Duration,
    item_writes: Duration,
    omission_writes: Duration,
    audit: Duration,
}

impl PackPersistenceSubspans {
    fn apply_insert_timings(&mut self, timings: &PackRecordInsertTimings) {
        self.ledger_serialization = timings.ledger_serialization;
        self.transaction = timings.transaction;
        self.record_write = timings.record_write;
        self.item_writes = timings.item_writes;
        self.omission_writes = timings.omission_writes;
        self.item_write_batches = timings.item_write_batches;
        self.omission_write_batches = timings.omission_write_batches;
    }

    fn transaction_overhead(&self) -> Duration {
        self.transaction
            .checked_sub(self.record_write + self.item_writes + self.omission_writes)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PerformanceTiming {
    name: &'static str,
    elapsed: std::time::Duration,
}

impl ContextPerformanceTrace {
    fn record_elapsed(&mut self, name: &'static str, start: Instant) {
        self.record_duration(name, start.elapsed());
    }

    fn record_duration(&mut self, name: &'static str, elapsed: Duration) {
        self.timings.push(PerformanceTiming { name, elapsed });
    }

    fn record_pack_persistence_subspans(&mut self) {
        if !self.pack_persistence.attempted {
            return;
        }
        let spans = self.pack_persistence.clone();
        self.record_duration("packPersistence::connectionOpen", spans.connection_open);
        self.record_duration("packPersistence::workspaceLookup", spans.workspace_lookup);
        self.record_duration("packPersistence::packHash", spans.pack_hash);
        self.record_duration(
            "packPersistence::degradedSerialization",
            spans.degraded_serialization,
        );
        self.record_duration("packPersistence::itemInputBuild", spans.item_input_build);
        self.record_duration(
            "packPersistence::omissionInputBuild",
            spans.omission_input_build,
        );
        self.record_duration(
            "packPersistence::ledgerSerialization",
            spans.ledger_serialization,
        );
        self.record_duration("packPersistence::recordWrite", spans.record_write);
        self.record_duration("packPersistence::itemWrites", spans.item_writes);
        self.record_duration("packPersistence::omissionWrites", spans.omission_writes);
        self.record_duration("packPersistence::transaction", spans.transaction);
        self.record_duration(
            "packPersistence::transactionOverhead",
            spans.transaction_overhead(),
        );
        self.record_duration("packPersistence::audit", spans.audit);
    }

    fn record_search_subspans(&mut self, search: SearchPerformanceTrace) {
        for (name, elapsed) in search.timings() {
            self.record_duration(name, elapsed);
        }
        self.search = search;
    }

    fn record_candidate_resolution_subspans(&mut self, subspans: &CandidateResolutionSubspans) {
        self.record_duration(
            "candidateResolution::hitIdResolution",
            subspans.hit_id_resolution,
        );
        self.record_duration(
            "candidateResolution::memoryIdDedupe",
            subspans.memory_id_dedupe,
        );
        self.record_duration(
            "candidateResolution::memoryTagBatchLoad",
            subspans.memory_tag_batch_load,
        );
        self.record_duration("candidateResolution::filtering", subspans.filtering);
        self.record_duration(
            "candidateResolution::freshnessProvenance",
            subspans.freshness_provenance,
        );
        self.record_duration(
            "candidateResolution::candidateConstruction",
            subspans.candidate_construction,
        );
        self.record_duration("candidateResolution::graphHints", subspans.graph_hints);
        self.record_duration(
            "candidateResolution::scoringOrdering",
            subspans.scoring_ordering,
        );
    }

    fn record_read_snapshot(
        &mut self,
        snapshot: &SnapshotPin<'_>,
        snapshot_generation: Option<u64>,
    ) {
        self.read_snapshot = Some(ReadSnapshotTrace {
            pinned: snapshot.is_pinned(),
            slot_id: snapshot.slot_id(),
            snapshot_generation,
            lease_held_ms: duration_millis_u64(snapshot.age()),
            expired: snapshot.is_expired(),
            poisoned: snapshot.is_poisoned(),
        });
    }

    fn elapsed_ms(&self, name: &str) -> u64 {
        self.timings
            .iter()
            .find(|timing| timing.name == name)
            .map_or(0, |timing| duration_millis_u64(timing.elapsed))
    }
}

fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug)]
pub enum ContextPackError {
    Storage(String),
    /// The addressed workspace has no store at the looked-for database path
    /// (bd-workspace-miss-init-suggestion-sfjvq). Kept distinct from
    /// [`Self::Storage`] so the CLI can surface the dedicated
    /// `workspace_store_missing` identity and exit code.
    WorkspaceStoreMissing(std::path::PathBuf),
    Search(SearchError),
    Pack(String),
    PolicyDenied(String),
    DeadlineExceeded(asupersync::CancelReason),
    Cancelled(asupersync::CancelReason),
}

impl ContextPackError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Storage(_) => Some("ee init --workspace ."),
            // The full dynamic repair (exact looked-for path, nearby stores,
            // conditional init LAST) is built by the CLI mapping via
            // `core::storeless_workspace_error`; this static hint only backs
            // surfaces that cannot carry a computed string.
            Self::WorkspaceStoreMissing(_) => {
                Some("Re-check --workspace addressing; only if you intended a NEW store: ee init")
            }
            Self::Search(error) => error.repair_hint(),
            Self::Pack(_) => Some("ee context --help"),
            Self::PolicyDenied(_) | Self::DeadlineExceeded(_) | Self::Cancelled(_) => None,
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
            Self::WorkspaceStoreMissing(path) => {
                write!(formatter, "Database not found at {}", path.display())
            }
            Self::DeadlineExceeded(reason) | Self::Cancelled(reason) => {
                formatter.write_str(&crate::core::outcome::cancel_message(reason))
            }
            Self::Search(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for ContextPackError {}

fn context_pack_cancellation_error(reason: asupersync::CancelReason) -> ContextPackError {
    match reason.kind {
        asupersync::CancelKind::Deadline | asupersync::CancelKind::Timeout => {
            ContextPackError::DeadlineExceeded(reason)
        }
        _ => ContextPackError::Cancelled(reason),
    }
}

fn context_pack_persist_failed_message_and_repair(persist_error: &str) -> (String, String) {
    if context_pack_persist_error_is_contention(persist_error) {
        (
            format!(
                "Pack assembled, but the pack ledger write was skipped because another process held the database write lock: {persist_error}"
            ),
            "Retry after a short delay, or use `ee pack \"<task>\" --read-only --json` when you only need prompt context and do not need a persisted pack ledger."
                .to_owned(),
        )
    } else {
        (
            format!("Pack assembled but persistence failed: {persist_error}"),
            "Run `ee status --json` and inspect storage/index posture; use `--read-only` if prompt context is sufficient for this run."
                .to_owned(),
        )
    }
}

fn context_pack_persist_error_is_contention(persist_error: &str) -> bool {
    persist_error.contains("could not acquire database write lock")
        || persist_error.contains("database transaction begin failed")
        || persist_error.contains("Resource temporarily unavailable")
        || persist_error.contains("contention timeout")
}

pub fn run_context_pack(options: &ContextPackOptions) -> Result<ContextResponse, ContextPackError> {
    run_context_pack_with_performance(options, PACK_COMMAND).map(|run| run.response)
}

/// Admit the newest live workspace memories through the same policy and pack
/// machinery used by normal context assembly.
///
/// This deliberately returns pack items rather than stored memory bodies. The
/// caller only receives content after temporal, workspace-scope, provenance,
/// secret-screening, tier-admission, and output-redaction checks have run.
pub(crate) fn admit_recent_context_memories(
    options: &ContextPackOptions,
    limit: usize,
) -> Result<Vec<AdmittedContextMemory>, ContextPackError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    if !database_path.exists() {
        return Err(ContextPackError::WorkspaceStoreMissing(database_path));
    }
    let connection = DbConnection::open_file_read_only(&database_path)
        .map_err(|error| ContextPackError::Storage(error.to_string()))?;
    let mut degraded = Vec::new();
    let reference_time = options.as_of.unwrap_or_else(Utc::now);
    // Match the RFC 3339 representation used when memory rows are inserted so
    // SQLite's deterministic text ordering compares like-formatted instants.
    let reference_time_text = reference_time.to_rfc3339();
    let candidate_cap = limit.saturating_mul(4).max(limit);
    let mut memories = BTreeMap::new();
    for workspace_id in context_workspace_ids(&connection, &options.workspace_path, &mut degraded) {
        let remaining = candidate_cap.saturating_sub(memories.len());
        if remaining == 0 {
            break;
        }
        let workspace_memories = connection
            .list_recent_current_memories_for_retrieval(
                &workspace_id,
                &reference_time_text,
                u32::try_from(remaining).unwrap_or(u32::MAX),
            )
            .map_err(|error| ContextPackError::Storage(error.to_string()))?;
        for memory in workspace_memories {
            memories.insert(memory.id.clone(), memory);
        }
    }

    let mut ordered = memories.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        parse_stored_memory_timestamp(&right.created_at)
            .cmp(&parse_stored_memory_timestamp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });

    let ordered_ids = ordered
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    let tags_by_memory = connection
        .get_memory_tags_batch(&ordered_ids)
        .map_err(|error| ContextPackError::Storage(error.to_string()))?;
    let mut metadata = BTreeMap::new();
    let mut candidates = Vec::with_capacity(candidate_cap);
    for (recency_rank, memory) in ordered.into_iter().enumerate() {
        if candidates.len() >= candidate_cap {
            break;
        }
        if !matches!(
            context_memory_seal_admission(
                &connection,
                &memory,
                &mut degraded,
                "context_candidate_memory_batch_unavailable",
                ContextResponseSeverity::Medium,
                "Orient-fast candidate admission",
            ),
            ContextMemorySealAdmission::Admit
        ) || !matches!(
            fallback_memory_validity_visibility(&memory, reference_time, false, false, false),
            FallbackMemoryVisibility::Visible
        ) || !crate::policy::redact_secret_like_content(&memory.content)
            .redacted_reasons
            .is_empty()
        {
            continue;
        }
        let Ok(memory_id) = MemoryId::from_str(&memory.id) else {
            continue;
        };
        let tags = tags_by_memory
            .get(&memory.id)
            .cloned()
            .unwrap_or_else(Vec::new);
        let Some(provenance) =
            provenance_for_memory(&memory, memory_id, &options.workspace_path, &mut degraded)
        else {
            continue;
        };
        let relevance = unit_score(1.0 - (recency_rank.min(500) as f32 * 0.001))
            .ok_or_else(|| ContextPackError::Pack("invalid recent relevance score".to_owned()))?;
        let utility = unit_score(memory.utility)
            .ok_or_else(|| ContextPackError::Pack("invalid recent utility score".to_owned()))?;
        let content = orient_fast_snippet_source(&memory.content);
        let candidate = PackCandidate::new(PackCandidateInput {
            memory_id,
            section: section_for_memory(&memory),
            estimated_tokens: estimate_tokens_default(&content),
            content,
            relevance,
            utility,
            provenance: vec![provenance],
            why: "Selected by the bounded orient-fast recency strategy after context admission."
                .to_owned(),
        })
        .map_err(|error| ContextPackError::Pack(error.to_string()))?
        .with_diversity_key(diversity_key_for_memory(&memory, &tags))
        .with_trust_signal(trust_signal_for_memory(&memory, memory_id, &mut degraded))
        .with_lifecycle(pack_lifecycle_for_memory(&memory, Some(reference_time)));
        metadata.insert(memory.id, (memory.created_at, tags));
        candidates.push(candidate);
    }

    let scope_context = MemoryScopeContext::for_workspace(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
    );
    filter_candidates_by_memory_scope(
        &connection,
        &mut candidates,
        &scope_context,
        &mut degraded,
        None,
        &BTreeSet::new(),
    );
    if context_memory_tier_admission_enabled(&options.workspace_path).unwrap_or(false) {
        apply_memory_tier_candidate_admission(&connection, &mut candidates, &mut degraded);
    }
    annotate_attempt_family_multiplicity(&connection, &mut candidates)?;
    candidates.sort_by(|left, right| {
        let left_created_at = metadata
            .get(&left.memory_id.to_string())
            .map(|(created_at, _)| created_at.as_str())
            .and_then(parse_stored_memory_timestamp);
        let right_created_at = metadata
            .get(&right.memory_id.to_string())
            .map(|(created_at, _)| created_at.as_str())
            .and_then(parse_stored_memory_timestamp);
        right_created_at
            .cmp(&left_created_at)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    candidates.truncate(limit);

    let budget = TokenBudget::new(options.max_tokens.unwrap_or(4_000))
        .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    let output_redaction_enabled =
        crate::config::workspace_output_redaction_enabled(&options.workspace_path);
    let draft = assemble_draft_with_profile_and_options_seeded(
        ContextPackProfile::Orientation,
        "orient fast recent memories",
        budget,
        candidates,
        crate::pack::PackAssemblyOptions {
            redaction_level: options.redaction_level,
            include_coverage_fill: options.output_options.include_coverage_fill,
            include_anti_pattern_first: true,
            output_redaction_enabled,
            lod_budget_shares: if options.no_lod {
                None
            } else {
                crate::pack::PackAssemblyOptions::default().lod_budget_shares
            },
            arena_mode: crate::pack::ArenaMode::Disabled,
        },
        &Deterministic::from_seed(0),
    )
    .map_err(|error| ContextPackError::Pack(error.to_string()))?;

    Ok(draft
        .items
        .into_iter()
        .filter_map(|item| {
            let (created_at, tags) = metadata.get(&item.memory_id.to_string())?.clone();
            Some(AdmittedContextMemory {
                item,
                created_at,
                tags,
            })
        })
        .collect())
}

fn orient_fast_snippet_source(content: &str) -> String {
    const MAX_CHARS: usize = 480;
    let truncated = content.chars().count() > MAX_CHARS;
    let kept_chars = if truncated {
        MAX_CHARS.saturating_sub(1)
    } else {
        MAX_CHARS
    };
    let mut snippet = content.chars().take(kept_chars).collect::<String>();
    if truncated {
        snippet.push('…');
    }
    snippet
}

pub fn run_context_pack_seeded(
    options: &ContextPackOptions,
    determinism: Deterministic<Seed>,
) -> Result<ContextResponse, ContextPackError> {
    run_context_pack_with_performance_seeded(options, PACK_COMMAND, determinism)
        .map(|run| run.response)
}

const PACK_DNA_SERIAL_GRAPH_TASK_COUNT: u64 = 1;
const DEFAULT_CONTEXT_PACK_DNA_MAX_ITEMS: usize = 10;
const DEFAULT_CONTEXT_PACK_DNA_MAX_EDGES: usize = 30;
const PACK_DNA_SERIAL_MERGE_ORDER_KEY: &str = concat!(
    "serial:normalize_inputs>voronoi_dominator>community_of_mass>",
    "ego_subgraph>ppr_neighbors>degraded"
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContextPackDnaConfig {
    enabled: bool,
    max_items: usize,
    max_edges: usize,
}

fn elapsed_millis_u64(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn trace_pack_dna_explain_orchestration(
    graph_explain_start: Instant,
    pack_dna_degraded_code: &str,
    graph_task_count: u64,
) {
    trace_pack_dna_explain_orchestration_with_timeout(
        graph_explain_start,
        pack_dna_degraded_code,
        graph_task_count,
        0,
    );
}

fn trace_pack_dna_explain_orchestration_with_timeout(
    graph_explain_start: Instant,
    pack_dna_degraded_code: &str,
    graph_task_count: u64,
    pack_dna_timeout_ms: u64,
) {
    tracing::debug!(
        target: "ee::context::pack_dna",
        explain_enabled = true,
        selection_latency_ms = 0_u64,
        graph_explain_latency_ms = elapsed_millis_u64(graph_explain_start),
        overlap_latency_ms = 0_u64,
        pack_dna_timeout_ms = pack_dna_timeout_ms,
        pack_dna_degraded_code = pack_dna_degraded_code,
        graph_task_count = graph_task_count,
        graph_merge_order_key = PACK_DNA_SERIAL_MERGE_ORDER_KEY,
        "pack DNA explain orchestration completed on serial path"
    );
}

#[cfg(test)]
type AfterPackPersistenceHook = Box<dyn FnOnce(&asupersync::Cx, bool)>;

#[cfg(test)]
thread_local! {
    static CONTEXT_PACK_DNA_COMPUTE_ERROR: RefCell<Option<crate::graph::GraphError>> = const { RefCell::new(None) };
    static AFTER_PACK_PERSISTENCE_HOOK: RefCell<Option<AfterPackPersistenceHook>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn set_context_pack_dna_compute_error(error: Option<crate::graph::GraphError>) {
    CONTEXT_PACK_DNA_COMPUTE_ERROR.with(|slot| {
        *slot.borrow_mut() = error;
    });
}

#[cfg(test)]
fn install_after_pack_persistence_hook(hook: impl FnOnce(&asupersync::Cx, bool) + 'static) {
    AFTER_PACK_PERSISTENCE_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_pack_persistence_hook(cx: &asupersync::Cx, succeeded: bool) {
    AFTER_PACK_PERSISTENCE_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(cx, succeeded);
        }
    });
}

#[cfg(not(test))]
fn run_after_pack_persistence_hook(_cx: &asupersync::Cx, _succeeded: bool) {}

fn compute_context_pack_dna(
    projection: &crate::graph::MemoryGraphProjection,
    input: &crate::graph::pack_dna::PackDnaInput,
) -> crate::graph::GraphResult<crate::graph::pack_dna::PackDna> {
    #[cfg(test)]
    {
        if let Some(error) = CONTEXT_PACK_DNA_COMPUTE_ERROR.with(|slot| slot.borrow_mut().take()) {
            return Err(error);
        }
    }
    crate::graph::pack_dna::compute_pack_dna(projection, input)
}

pub fn attach_pack_dna_to_context_response(database_path: &Path, response: &mut ContextResponse) {
    let workspace_path = workspace_path_from_database_path(database_path);
    let pack_dna_config = match workspace_path
        .as_deref()
        .map(context_pack_dna_config)
        .unwrap_or(Ok(ContextPackDnaConfig {
            enabled: false,
            max_items: DEFAULT_CONTEXT_PACK_DNA_MAX_ITEMS,
            max_edges: DEFAULT_CONTEXT_PACK_DNA_MAX_EDGES,
        })) {
        Ok(config) if config.enabled => config,
        Ok(_) => {
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
    };

    let graph_explain_start = Instant::now();
    let connection = match DbConnection::open_file_read_only(database_path) {
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
            trace_pack_dna_explain_orchestration(
                graph_explain_start,
                "context_graph_snapshot_unavailable",
                0,
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
                TrustClass::HumanExplicit
                    | TrustClass::PeerHumanAttested
                    | TrustClass::AgentValidated
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
    let projection_seed_ids = pack_dna_projection_seed_ids(&input, pack_dna_config.max_items);
    let projection = match crate::graph::build_memory_graph_for_frontier(
        &connection,
        &projection_seed_ids,
        &crate::graph::FrontierProjectionOptions {
            max_depth: input.ego_radius,
            max_edges: pack_dna_config.max_edges,
            min_weight: None,
            min_confidence: None,
        },
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
            trace_pack_dna_explain_orchestration(
                graph_explain_start,
                "context_graph_snapshot_unavailable",
                0,
            );
            return;
        }
    };
    let pack_dna = match compute_context_pack_dna(&projection, &input) {
        Ok(pack_dna) => pack_dna,
        Err(crate::graph::GraphError::AlgorithmTimeout { timeout_ms, .. }) => {
            let pack_dna = crate::graph::pack_dna::PackDna {
                schema: crate::graph::pack_dna::PACK_DNA_SCHEMA_V1,
                snapshot_version: projection.snapshot_version,
                pack_memory_count: input.pack_memory_ids.len(),
                query_seed_count: input.query_seed_weights.len(),
                trust_anchor_count: input.trust_anchor_memory_ids.len(),
                dominator: None,
                community_of_mass: None,
                ego_subgraph: None,
                ppr_neighbors: Vec::new(),
                degraded: vec![crate::graph::pack_dna::pack_dna_timeout_degradation(
                    timeout_ms,
                )],
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
            trace_pack_dna_explain_orchestration_with_timeout(
                graph_explain_start,
                GRAPH_PACK_DNA_TIMEOUT_CODE,
                PACK_DNA_SERIAL_GRAPH_TASK_COUNT,
                timeout_ms,
            );
            response.data.pack_dna =
                Some(serde_json::to_value(&pack_dna).unwrap_or(serde_json::Value::Null));
            return;
        }
        Err(error) => {
            response.data.pack_dna = Some(serde_json::Value::Null);
            push_degradation(
                &mut response.data.degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!("Pack DNA computation failed: {error}"),
                Some("ee graph centrality-refresh --workspace .".to_string()),
            );
            trace_pack_dna_explain_orchestration(
                graph_explain_start,
                "context_graph_snapshot_unavailable",
                PACK_DNA_SERIAL_GRAPH_TASK_COUNT,
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

    let pack_dna_degraded_codes = pack_dna
        .degraded
        .iter()
        .map(|degradation| degradation.code.as_str())
        .collect::<Vec<_>>()
        .join(",");
    trace_pack_dna_explain_orchestration(
        graph_explain_start,
        if pack_dna_degraded_codes.is_empty() {
            "none"
        } else {
            pack_dna_degraded_codes.as_str()
        },
        PACK_DNA_SERIAL_GRAPH_TASK_COUNT,
    );
    response.data.pack_dna =
        Some(serde_json::to_value(&pack_dna).unwrap_or(serde_json::Value::Null));
}

fn workspace_path_from_database_path(database_path: &Path) -> Option<PathBuf> {
    let ee_dir = database_path.parent()?;
    (ee_dir.file_name()? == ".ee").then(|| ee_dir.parent().map(Path::to_path_buf))?
}

fn context_pack_dna_config(workspace_path: &Path) -> Result<ContextPackDnaConfig, String> {
    let config = context_workspace_config(workspace_path, "Pack DNA")?;
    let Some(config) = config else {
        return Ok(ContextPackDnaConfig {
            enabled: false,
            max_items: DEFAULT_CONTEXT_PACK_DNA_MAX_ITEMS,
            max_edges: DEFAULT_CONTEXT_PACK_DNA_MAX_EDGES,
        });
    };
    Ok(ContextPackDnaConfig {
        enabled: config.graph.feature.pack_dna_enabled.unwrap_or(false),
        max_items: pack_dna_usize_config(
            config.graph.pack_dna.max_items,
            GRAPH_PACK_DNA_MAX_ITEMS_KEY,
        )?
        .unwrap_or(DEFAULT_CONTEXT_PACK_DNA_MAX_ITEMS),
        max_edges: pack_dna_usize_config(
            config.graph.pack_dna.max_edges,
            GRAPH_PACK_DNA_MAX_EDGES_KEY,
        )?
        .unwrap_or(DEFAULT_CONTEXT_PACK_DNA_MAX_EDGES),
    })
}

fn pack_dna_usize_config(value: Option<u64>, key: &str) -> Result<Option<usize>, String> {
    value
        .map(|value| {
            usize::try_from(value).map_err(|_| {
                format!("Pack DNA skipped because {key}={value} does not fit this platform")
            })
        })
        .transpose()
}

fn pack_dna_projection_seed_ids(
    input: &crate::graph::pack_dna::PackDnaInput,
    max_items: usize,
) -> Vec<String> {
    let mut seeds = Vec::new();
    let mut seen = BTreeSet::new();
    pack_dna_push_seed_ids(
        &mut seeds,
        &mut seen,
        input.trust_anchor_memory_ids.iter().copied(),
        max_items,
    );
    pack_dna_push_seed_ids(
        &mut seeds,
        &mut seen,
        input.query_seed_weights.keys().copied(),
        max_items,
    );
    pack_dna_push_seed_ids(
        &mut seeds,
        &mut seen,
        input.pack_memory_ids.iter().copied(),
        max_items,
    );
    seeds
}

fn pack_dna_push_seed_ids(
    seeds: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    memory_ids: impl IntoIterator<Item = MemoryId>,
    max_items: usize,
) {
    if seeds.len() >= max_items {
        return;
    }
    let mut memory_ids = memory_ids
        .into_iter()
        .map(|memory_id| memory_id.to_string())
        .collect::<Vec<_>>();
    sort_by_ulid_payload_or_lexical(&mut memory_ids, String::as_str);
    for memory_id in memory_ids {
        if seeds.len() >= max_items {
            return;
        }
        if seen.insert(memory_id.clone()) {
            seeds.push(memory_id);
        }
    }
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

fn context_severity_from_pack_dna(severity: &str) -> ContextResponseSeverity {
    ContextResponseSeverity::parse_lossy(severity)
}

pub fn run_context_pack_with_performance(
    options: &ContextPackOptions,
    command: &'static str,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        run_context_pack_with_performance_with_cx(&cx, options, command).await
    })
    .map_err(|error| ContextPackError::Pack(format!("Failed to start pack runtime: {error}")))?
}

pub async fn run_context_pack_with_performance_with_cx(
    cx: &asupersync::Cx,
    options: &ContextPackOptions,
    command: &'static str,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    let determinism = Deterministic::from_seed(0);
    run_context_pack_with_performance_inner(
        options,
        command,
        determinism,
        PackRecordPersistence::Ambient,
        ContextPackControl::new(cx, None, None),
    )
    .await
}

pub fn context_request_from_options(
    options: &ContextPackOptions,
) -> Result<ContextRequest, ContextPackError> {
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);
    Ok(context_request_from_options_with_runtime_profile(options, &runtime_profile)?.request)
}

struct RuntimeProfileCappedRequest {
    request: ContextRequest,
    effective_max_tokens: u32,
    tokens_capped: bool,
    effective_candidate_pool: u32,
    candidate_pool_capped: bool,
}

fn context_request_from_options_with_runtime_profile(
    options: &ContextPackOptions,
    runtime_profile: &RuntimeProfileReport,
) -> Result<RuntimeProfileCappedRequest, ContextPackError> {
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
    Ok(RuntimeProfileCappedRequest {
        request,
        effective_max_tokens,
        tokens_capped,
        effective_candidate_pool,
        candidate_pool_capped,
    })
}

pub fn run_context_pack_with_performance_controlled(
    options: &ContextPackOptions,
    command: &'static str,
    deadline: Option<Duration>,
    cancellation_flag: Option<&AtomicBool>,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    let runtime_timeout = deadline
        .unwrap_or(Duration::from_secs(60))
        .max(Duration::from_secs(60));
    crate::core::run_cli_with_cx(runtime_timeout, |cx| async move {
        let determinism = Deterministic::from_seed(0);
        run_context_pack_with_performance_inner(
            options,
            command,
            determinism,
            PackRecordPersistence::Ambient,
            ContextPackControl::new(&cx, deadline, cancellation_flag),
        )
        .await
    })
    .map_err(|error| ContextPackError::Pack(format!("Failed to start pack runtime: {error}")))?
}

pub fn run_context_pack_with_performance_seeded(
    options: &ContextPackOptions,
    command: &'static str,
    determinism: Deterministic<Seed>,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        run_context_pack_with_performance_inner(
            options,
            command,
            determinism,
            PackRecordPersistence::Seeded,
            ContextPackControl::new(&cx, None, None),
        )
        .await
    })
    .map_err(|error| ContextPackError::Pack(format!("Failed to start pack runtime: {error}")))?
}

#[derive(Clone, Copy)]
enum PackRecordPersistence {
    Ambient,
    Seeded,
}

#[derive(Clone, Copy)]
struct ContextPackControl<'a> {
    cx: &'a asupersync::Cx,
    deadline: Option<Instant>,
    cancellation_flag: Option<&'a AtomicBool>,
}

impl<'a> ContextPackControl<'a> {
    fn new(
        cx: &'a asupersync::Cx,
        deadline: Option<Duration>,
        cancellation_flag: Option<&'a AtomicBool>,
    ) -> Self {
        let now = Instant::now();
        Self {
            cx,
            deadline: deadline.and_then(|duration| now.checked_add(duration)),
            cancellation_flag,
        }
    }

    fn check(self) -> Result<(), ContextPackError> {
        if let Some(flag) = self.cancellation_flag
            && flag.load(Ordering::SeqCst)
        {
            return Err(ContextPackError::Cancelled(
                crate::core::outcome::attributed_cancel_reason(
                    self.cx,
                    asupersync::CancelKind::Shutdown,
                    "context pack cancelled by caller shutdown signal",
                ),
            ));
        }
        if let Some(deadline) = self.deadline
            && Instant::now() >= deadline
        {
            return Err(ContextPackError::DeadlineExceeded(
                crate::core::outcome::attributed_cancel_reason(
                    self.cx,
                    asupersync::CancelKind::Deadline,
                    "context pack deadline expired before the next execution checkpoint",
                ),
            ));
        }
        self.cx.checkpoint().map_err(|_| {
            context_pack_cancellation_error(self.cx.cancel_reason().unwrap_or_else(|| {
                crate::core::outcome::attributed_cancel_reason(
                    self.cx,
                    asupersync::CancelKind::User,
                    "context pack cancelled without a recorded reason",
                )
            }))
        })
    }
}

/// Explain why a target memory was (or was not) selected for a context pack.
///
/// This is the read-only counterfactual of [`run_context_pack_with_performance`]
/// (`ee why-not`, the reverse of `ee why`). It resolves the exact candidate
/// universe the task+workspace would trigger, locates the target memory among
/// those candidates (or reconstructs a not-retrieved candidate when the memory
/// never reached the candidate pool), and delegates to
/// [`crate::pack::explain_why_not_selected`]. The cost is one `ee pack`-shaped
/// retrieval; nothing is persisted, mutated, or cached.
///
/// When the target is in the candidate pool the report's `reason_source` is
/// `authoritative`; for a reconstructed not-retrieved candidate it is
/// `reconstructed` (E1.4 — handled by the library's `reason_source` mapping).
///
/// # Errors
///
/// Returns [`ContextPackError`] when the database is missing, the search
/// backend fails, the target memory does not exist, or the report cannot be
/// assembled.
pub fn explain_why_not(
    options: &ContextPackOptions,
    target_memory_id: MemoryId,
    determinism: &Deterministic<Seed>,
) -> Result<WhyNotSelectedReport, ContextPackError> {
    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    if !database_path.exists() {
        return Err(ContextPackError::WorkspaceStoreMissing(database_path));
    }

    let index_dir = options
        .index_dir
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("index"));
    let fast_embedder_override = if options.source_mode.uses_embeddings()
        && index_dir.exists()
        && index_corpus_compatibility_is_current(&index_dir)
    {
        let embedder_database_path = database_path.clone();
        let preparation = crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            prepare_search_embedder_for_workspace(
                &cx,
                &options.workspace_path,
                &embedder_database_path,
            )
            .await
            .map_err(|error| {
                ContextPackError::Search(map_frankensearch_error(
                    &cx,
                    "why-not embedder preparation",
                    error,
                ))
            })
        })
        .map_err(|error| {
            ContextPackError::Pack(format!(
                "Failed to start why-not embedder preparation: {error}"
            ))
        })??;
        Some(preparation.fast_embedder)
    } else {
        None
    };

    let mut effective_filters = options.filters.clone();
    if effective_filters.temporal.as_of.is_none() {
        effective_filters.temporal.as_of = options.as_of;
    }

    let mut degraded = Vec::new();
    let (read_pool_config, pin_snapshot) =
        context_read_pool_config(&options.workspace_path, &mut degraded);
    let read_pool = registered_process_read_pool(
        DatabaseConfig::file(database_path.clone()),
        read_pool_config,
    );

    let request = ContextRequest::new(ContextRequestInput {
        query: options.query.clone(),
        profile: options.profile,
        max_tokens: options.max_tokens,
        candidate_pool: options.candidate_pool,
        max_results: options.max_results,
        sections: Vec::new(),
    })
    .map_err(|error| ContextPackError::Pack(error.to_string()))?;

    let read_snapshot = if pin_snapshot {
        read_pool.pin_snapshot_with_metadata(context_snapshot_pin_metadata(&request))
    } else {
        read_pool.acquire_snapshot(false)
    }
    .map_err(|error| ContextPackError::Storage(format!("Failed to open database: {error}")))?;

    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let mut search_preloaded_memories = BTreeMap::new();
    let mut search_report = match run_context_search_with_preloaded_memories(
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
            relevance_floor: Some(options.relevance_floor.unwrap_or(0.0)),
            dedup_mode: crate::core::search::SearchDedupMode::DocId,
            source_mode: options.source_mode,
            strict_source_mode: options.strict_source_mode,
            memory_scope: options.memory_scope,
            strict_scope: options.strict_scope,
        },
        read_connection,
        None,
        determinism,
        fast_embedder_override,
    ) {
        Ok(context_search) => {
            search_preloaded_memories = context_search.preloaded_memories;
            context_search.report
        }
        Err(SearchError::NoIndex) => missing_index_search_report(
            &request.query,
            request.candidate_pool,
            runtime_profile_for_workspace(&options.workspace_path),
        ),
        Err(error) => return Err(ContextPackError::Search(error)),
    };

    // Mirror the production pack path: when the derived index is missing or
    // errored, resolve candidates from a deterministic lexical memory fallback so
    // why-not reflects the same candidate universe `ee pack` would actually use
    // (otherwise a memory the pack would include via fallback is misreported as
    // not_retrieved/reconstructed).
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
        search_report.results = fallback_hits;
        search_report.status = if search_report.results.is_empty() {
            SearchStatus::NoResults
        } else {
            SearchStatus::Success
        };
    }

    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let (candidates, _candidate_metrics) = candidates_from_search_with_metrics(
        read_connection,
        &options.workspace_path,
        &search_report,
        &effective_filters,
        options.include_tombstoned,
        &mut degraded,
        Some(&search_preloaded_memories),
    );

    let profile = options.profile.unwrap_or(ContextPackProfile::Balanced);
    let budget = match options.max_tokens {
        Some(max_tokens) => TokenBudget::new(max_tokens)
            .map_err(|error| ContextPackError::Pack(error.to_string()))?,
        None => TokenBudget::default_context(),
    };

    // Locate the target among the real candidate pool (authoritative path). When
    // it never reached the pool, reconstruct a not-retrieved candidate so scores
    // still render and the library reports reason_source=reconstructed.
    // bd-1n0np.1.9: when the target was filtered out of the candidate pool,
    // classify *why* by re-running the SAME candidate filters the pack applies
    // (see candidates_from_search_with_metrics) against the target memory, so a
    // memory dropped by tag/validity/trust/redaction reports the authoritative
    // `excluded_by_*` reason instead of collapsing into a bare `not_retrieved`.
    let mut why_not_exclusions: Vec<crate::pack::WhyNotSelectionExclusion> = Vec::new();
    let target = match candidates
        .iter()
        .find(|candidate| candidate.memory_id == target_memory_id)
        .cloned()
    {
        Some(candidate) => candidate,
        None => {
            let classify_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
            if let Ok(Some(target_memory)) =
                classify_connection.get_memory(&target_memory_id.to_string())
            {
                let target_tags = classify_connection
                    .get_memory_tags(&target_memory.id)
                    .unwrap_or_default();
                if !effective_filters.tags.is_empty()
                    && !effective_filters.matches_tags(&target_tags)
                {
                    why_not_exclusions.push(crate::pack::WhyNotSelectionExclusion::new(
                        crate::pack::WhyNotSelectionExclusionKind::Filter,
                        "excluded_by_tag_filter",
                        "The memory did not match the requested tag filter.",
                        None,
                    ));
                }
                if !effective_filters.temporal.is_empty()
                    && matches!(
                        temporal_memory_outcome(&target_memory, &effective_filters.temporal),
                        TemporalCandidateOutcome::Exclude
                    )
                {
                    why_not_exclusions.push(crate::pack::WhyNotSelectionExclusion::new(
                        crate::pack::WhyNotSelectionExclusionKind::ValidityWindow,
                        "excluded_by_validity_window",
                        "The memory fell outside the requested temporal validity window.",
                        None,
                    ));
                }
                if !effective_filters.trust.is_empty()
                    && !effective_filters.trust.matches(
                        &target_memory.trust_class,
                        posture_for_trust_class(&target_memory.trust_class),
                    )
                {
                    why_not_exclusions.push(crate::pack::WhyNotSelectionExclusion::new(
                        crate::pack::WhyNotSelectionExclusionKind::Filter,
                        "excluded_by_trust_filter",
                        "The memory's trust class did not match the requested trust filter.",
                        None,
                    ));
                }
                if !effective_filters.redaction.allow_categories.is_empty()
                    && !redaction_allow_categories(
                        &target_memory.content,
                        &effective_filters.redaction,
                    )
                {
                    why_not_exclusions.push(crate::pack::WhyNotSelectionExclusion::new(
                        crate::pack::WhyNotSelectionExclusionKind::Redaction,
                        "excluded_by_redaction",
                        "The memory was withheld by the redaction allow-category filter.",
                        None,
                    ));
                }
            }
            let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
            reconstruct_not_retrieved_candidate(
                read_connection,
                &options.workspace_path,
                target_memory_id,
                &mut degraded,
            )?
        }
    };

    // bd-1n0np.1.8: surface the retrieval-time degradations collected above (e.g.
    // a degraded/missing search index that forced the lexical fallback) so a miss
    // caused by a broken index reports `not_retrieved_due_to_degraded_index`
    // (stage `degraded_index`) instead of a bare `not_retrieved` — the
    // honest-vs-misleading distinction. Map the context degradations onto the
    // why-not degradation contract.
    let why_not_degraded: Vec<crate::pack::WhyNotSelectionDegradation> = degraded
        .iter()
        .map(|degradation| {
            crate::pack::WhyNotSelectionDegradation::new(
                degradation.code.clone(),
                degradation.severity.as_str(),
                degradation.message.clone(),
                degradation.repair.clone(),
            )
        })
        .collect();
    let input =
        WhyNotSelectedInput::new(options.query.clone(), target, budget, profile, candidates)
            .with_degraded(why_not_degraded)
            .with_exclusions(why_not_exclusions);
    explain_why_not_selected(input).map_err(|error| ContextPackError::Pack(error.to_string()))
}

/// CLI-facing convenience wrapper for [`explain_why_not`] that uses the same
/// fixed determinism seed as [`run_context_pack_with_performance`], so the
/// counterfactual reflects the exact selection the default `ee pack` would make.
///
/// # Errors
///
/// Propagates every [`ContextPackError`] from [`explain_why_not`].
pub fn explain_why_not_default(
    options: &ContextPackOptions,
    target_memory_id: MemoryId,
) -> Result<WhyNotSelectedReport, ContextPackError> {
    let determinism = Deterministic::from_seed(0);
    explain_why_not(options, target_memory_id, &determinism)
}

/// Build a `PackCandidate` for a memory that did not appear in the retrieved
/// candidate pool, so `ee why-not` can still render its scores and provenance.
///
/// The candidate is intentionally kept out of the candidate list passed to
/// [`crate::pack::explain_why_not_selected`]; its absence drives the
/// `not_retrieved` primary reason and `reconstructed` reason source.
fn reconstruct_not_retrieved_candidate(
    connection: &DbConnection,
    workspace_path: &Path,
    memory_id: MemoryId,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Result<PackCandidate, ContextPackError> {
    let memory = connection
        .get_memory(&memory_id.to_string())
        .map_err(|error| ContextPackError::Storage(error.to_string()))?
        .ok_or_else(|| {
            ContextPackError::Pack(format!(
                "Memory {memory_id} not found; cannot explain why it was not selected."
            ))
        })?;
    let tags = connection.get_memory_tags(&memory.id).unwrap_or_default();
    let mut provenance = Vec::new();
    if let Some(memory_provenance) =
        provenance_for_memory(&memory, memory_id, workspace_path, degraded)
    {
        provenance.push(memory_provenance);
    }
    let relevance = unit_score(0.0)
        .ok_or_else(|| ContextPackError::Pack("invalid relevance score".to_string()))?;
    let utility = unit_score(memory.utility)
        .ok_or_else(|| ContextPackError::Pack("invalid utility score".to_string()))?;
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id,
        section: section_for_memory(&memory),
        content: memory.content.clone(),
        estimated_tokens: estimate_tokens_default(&memory.content),
        relevance,
        utility,
        provenance,
        why: format!(
            "Reconstructed candidate: memory {memory_id} was not in the retrieved candidate pool for this task."
        ),
    })
    .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    let candidate = candidate
        .with_diversity_key(diversity_key_for_memory(&memory, &tags))
        .with_trust_signal(trust_signal_for_memory(&memory, memory_id, degraded))
        .with_lifecycle(pack_lifecycle_for_memory(&memory, None));
    let candidate = match memory.tombstoned_at.as_ref() {
        Some(tombstoned_at) => candidate.with_tombstoned_at(tombstoned_at.clone()),
        None => candidate,
    };
    Ok(candidate)
}

#[allow(clippy::expect_used)]
async fn run_context_pack_with_performance_inner(
    options: &ContextPackOptions,
    command: &'static str,
    determinism: Deterministic<Seed>,
    pack_record_persistence: PackRecordPersistence,
    control: ContextPackControl<'_>,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    let total_start = Instant::now();
    control.check()?;
    let mut trace = ContextPerformanceTrace::default();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);

    let request_start = Instant::now();
    let RuntimeProfileCappedRequest {
        mut request,
        effective_max_tokens,
        tokens_capped,
        effective_candidate_pool,
        candidate_pool_capped,
    } = context_request_from_options_with_runtime_profile(options, &runtime_profile)?;
    control.check()?;
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
        return Err(ContextPackError::WorkspaceStoreMissing(database_path));
    }
    control.check()?;

    let mut degraded = Vec::new();

    let index_dir = options
        .index_dir
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("index"));
    let search_options = SearchOptions {
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
        // Keep the default candidate pool broad so an exact single-memory match
        // is not dropped by the interactive search command's presentation floor.
        // An explicit caller floor still applies for diagnostic/e2e paths.
        relevance_floor: Some(options.relevance_floor.unwrap_or(0.0)),
        dedup_mode: crate::core::search::SearchDedupMode::DocId,
        source_mode: options.source_mode,
        strict_source_mode: options.strict_source_mode,
        memory_scope: options.memory_scope,
        strict_scope: options.strict_scope,
    };
    reconcile_search_index_before_read_with_cx(control.cx, &search_options).await;
    let embedder_preparation = if options.source_mode.uses_embeddings()
        && index_dir.exists()
        && index_corpus_compatibility_is_current(&index_dir)
    {
        let preparation = prepare_search_embedder_for_workspace(
            control.cx,
            &options.workspace_path,
            &database_path,
        )
        .await
        .map_err(|error| {
            ContextPackError::Search(map_frankensearch_error(
                control.cx,
                "context embedder preparation",
                error,
            ))
        })?;
        trace.record_duration("embedderPrepare", preparation.elapsed);
        control.check()?;
        Some(preparation)
    } else {
        None
    };
    let prepared_embed_backend = embedder_preparation
        .as_ref()
        .map_or_else(crate::core::index::active_embed_backend, |preparation| {
            preparation.backend
        });

    let (read_pool_config, pin_snapshot) =
        context_read_pool_config(&options.workspace_path, &mut degraded);
    let snapshot_open_start = Instant::now();
    let read_pool = registered_process_read_pool(
        DatabaseConfig::file(database_path.clone()),
        read_pool_config,
    );
    let read_pool_ad_hoc_bypass_baseline = read_pool.stats().ad_hoc_bypass_count;
    let read_snapshot = if pin_snapshot {
        read_pool.pin_snapshot_with_metadata(context_snapshot_pin_metadata(&request))
    } else {
        read_pool.acquire_snapshot(false)
    }
    .map_err(|error| ContextPackError::Storage(format!("Failed to open database: {error}")))?;
    trace.db_open_count = trace.db_open_count.saturating_add(1);
    trace.record_elapsed("dbOpen", snapshot_open_start);
    control.check()?;
    let read_snapshot_generation = checked_context_read_snapshot(&read_pool, &read_snapshot)
        .ok()
        .and_then(|connection| context_read_snapshot_generation(connection).ok());

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
    // NOTE: the `context_profile_budget_capped` degradation is emitted later, after the
    // pack draft is assembled, so it only fires when the cap was actually *binding*
    // (content was omitted or the capped budget was filled). Emitting it here — whenever
    // a profile lowers the configured ceiling — falsely flags healthy packs that used a
    // tiny fraction of the budget as degraded. See the post-assembly emission below.

    let l2_cache_context = if options.output_options.cache_json_response {
        let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
        let l2_context = context_pack_l2_prepare(
            options,
            read_connection,
            &request,
            &effective_filters,
            output_redaction_enabled,
            prepared_embed_backend,
            &mut degraded,
        );
        if let Some(context) = &l2_context
            && let Some(cached_run) = context_pack_l2_try_hit(
                context,
                command,
                options,
                &search_options,
                read_connection,
                &request,
                total_start,
                &mut trace,
                &mut degraded,
            )
        {
            control.check()?;
            return Ok(cached_run);
        }
        l2_context
    } else {
        None
    };

    control.check()?;
    let search_start = Instant::now();
    let mut context_write_connection = if options.persist_pack {
        DbConnection::open_file(&database_path).ok()
    } else {
        None
    };
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let mut search_preloaded_memories = BTreeMap::new();
    let mut search_report =
        match run_context_search_with_preloaded_memories_and_workspace_state_with_cx(
            control.cx,
            &search_options,
            read_connection,
            context_write_connection.as_ref(),
            Some(&SearchWorkspaceProbeState {
                runtime_profile: runtime_profile.clone(),
                output_redaction_enabled,
            }),
            determinism.shared_child("search.rerank"),
            embedder_preparation
                .as_ref()
                .map(|preparation| Arc::clone(&preparation.fast_embedder)),
        )
        .await
        {
            Ok(context_search) => {
                search_preloaded_memories = context_search.preloaded_memories;
                trace.record_search_subspans(context_search.performance);
                context_search.report
            }
            Err(SearchError::NoIndex) => missing_index_search_report(
                &request.query,
                request.candidate_pool,
                runtime_profile.clone(),
            ),
            Err(SearchError::Cancelled(reason)) => {
                return Err(context_pack_cancellation_error(reason));
            }
            Err(error) => return Err(ContextPackError::Search(error)),
        };
    trace.index_status_checks = trace.index_status_checks.saturating_add(1);
    trace.record_elapsed("search", search_start);
    control.check()?;

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
        control.check()?;
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
    if search_report.status == SearchStatus::NoResults
        && !degraded
            .iter()
            .any(|entry| entry.code == "no_relevant_results")
    {
        push_degradation(
            &mut degraded,
            "context_no_results",
            ContextResponseSeverity::Low,
            "Search completed but returned no candidate memories.",
            Some("ee remember --workspace . --level procedural --kind rule \"...\"".to_string()),
        );
    }

    let mut adaptive_budget_decision = None;
    match adaptive_budget_decision_for_context(
        &options.workspace_path,
        options.max_tokens,
        &request,
        &search_report,
        &runtime_profile,
    ) {
        Ok(Some(decision)) => {
            request = ContextRequest::new(ContextRequestInput {
                query: request.query.clone(),
                profile: Some(request.profile),
                max_tokens: Some(decision.computed_tokens),
                candidate_pool: Some(request.candidate_pool),
                max_results: request.max_results,
                sections: request.sections.clone(),
            })
            .map_err(|error| ContextPackError::Pack(error.to_string()))?;
            adaptive_budget_decision = Some(decision);
        }
        Ok(None) => {}
        Err(message) => push_degradation(
            &mut degraded,
            "context_config_unavailable",
            ContextResponseSeverity::Medium,
            message,
            Some("Fix or remove .ee/config.toml.".to_string()),
        ),
    }
    control.check()?;

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
        Some(&search_preloaded_memories),
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
    control.check()?;
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let graph_hint_start = Instant::now();
    let graph_metrics = apply_graph_hints(
        read_connection,
        &options.workspace_path,
        &effective_filters,
        options.include_tombstoned,
        &mut candidates,
        &mut degraded,
    );
    candidate_metrics.subspans.graph_hints = graph_hint_start.elapsed();
    candidate_metrics.graph_boosted_candidates = graph_metrics.boosted_candidates;
    candidate_metrics.graph_expanded_candidates = graph_metrics.expanded_candidates;
    candidate_metrics.graph_filtered_candidates = graph_metrics.filtered_candidates;
    candidate_metrics.graph_missing_seeds = graph_metrics.missing_seeds;
    candidate_metrics.graph_traversed_edges = graph_metrics.traversed_edges;
    trace.record_elapsed("candidateResolution", candidate_start);
    control.check()?;

    let focus_start = Instant::now();
    trace.focus_state_read_attempts = trace.focus_state_read_attempts.saturating_add(1);
    match read_active_focus_state(&options.workspace_path) {
        Ok(Some(focus_state)) => {
            trace.focus_state_hits = trace.focus_state_hits.saturating_add(1);
            let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
            let focus_workspace_ids =
                context_workspace_ids(read_connection, &options.workspace_path, &mut degraded)
                    .into_iter()
                    .collect::<BTreeSet<_>>();
            let focus_candidates = focus_candidates_from_state(
                read_connection,
                &options.workspace_path,
                &focus_state,
                options.include_tombstoned,
                context_include_expired(options, &effective_filters),
                context_include_future(options, &effective_filters),
                context_validity_reference_time(options, &effective_filters)
                    .unwrap_or_else(Utc::now),
                &focus_workspace_ids,
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
    control.check()?;

    let scope_filter_input_count =
        candidate_filter_input_count.saturating_add(trace.focus_candidate_count);
    let scope_context = MemoryScopeContext::for_workspace(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
    );
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let global_store_memory_ids = global_store_search_memory_ids(&search_report);
    let scope_stats = filter_candidates_by_memory_scope(
        read_connection,
        &mut candidates,
        &scope_context,
        &mut degraded,
        Some(&search_preloaded_memories),
        &global_store_memory_ids,
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
    let global_fan_in_filtered = apply_global_store_pack_policy(
        &mut candidates,
        &global_store_memory_ids,
        request.budget.max_tokens(),
        &mut degraded,
    );
    if global_fan_in_filtered > 0 {
        trace.filter_input_count = trace.filter_input_count.max(scope_filter_input_count);
        trace.filtered_count = trace.filtered_count.saturating_add(global_fan_in_filtered);
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

    let tier_admission_start = Instant::now();
    match context_memory_tier_admission_enabled(&options.workspace_path) {
        Ok(true) => {
            let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
            let tier_metrics = apply_memory_tier_candidate_admission(
                read_connection,
                &mut candidates,
                &mut degraded,
            );
            candidate_metrics.tier_boosted_candidates = tier_metrics.boosted_candidates;
            candidate_metrics.tier_cold_candidates = tier_metrics.cold_candidates;
            candidate_metrics.tier_required_cold_candidates = tier_metrics.required_cold_candidates;
        }
        Ok(false) => {}
        Err(message) => push_degradation(
            &mut degraded,
            "context_config_unavailable",
            ContextResponseSeverity::Medium,
            message,
            Some("Fix or remove .ee/config.toml.".to_string()),
        ),
    }
    trace.record_elapsed("memoryTierAdmission", tier_admission_start);

    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    annotate_attempt_family_multiplicity_in_current_snapshot(read_connection, &mut candidates)?;

    let sentinel_omissions = if options.require_fresh_sentinels {
        let reference_time =
            context_validity_reference_time(options, &effective_filters).unwrap_or_else(Utc::now);
        let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
        filter_candidates_by_required_fresh_sentinels(
            read_connection,
            &mut candidates,
            reference_time,
            &mut degraded,
        )?
    } else {
        Vec::new()
    };
    control.check()?;

    let ppr_rerank_start = Instant::now();
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let configured_ppr_weight = if options.ppr_weight.is_some() {
        None
    } else {
        match configured_context_ppr_weight(&options.workspace_path) {
            Ok(weight) => weight,
            Err(message) => {
                push_degradation(
                    &mut degraded,
                    "context_config_unavailable",
                    ContextResponseSeverity::Medium,
                    message,
                    Some("Fix or remove .ee/config.toml.".to_string()),
                );
                None
            }
        }
    };
    let ppr_metrics = apply_personalized_pagerank_rerank(
        read_connection,
        &options.workspace_path,
        &search_report,
        &mut candidates,
        effective_context_ppr_weight(options.ppr_weight, configured_ppr_weight),
        &mut degraded,
    );
    trace.record_elapsed("pprRerank", ppr_rerank_start);
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
    let changed_symbol_metrics = apply_changed_symbol_context_boost(
        &options.workspace_path,
        &options.changed_symbols,
        options.changed_symbols_from_git,
        &mut candidates,
        &mut degraded,
    );
    candidate_metrics.graph_boosted_candidates = candidate_metrics
        .graph_boosted_candidates
        .saturating_add(changed_symbol_metrics.boosted_candidates);
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let mut agent_profile =
        apply_agent_context_profile_bias(read_connection, &options.workspace_path, &mut candidates);
    apply_attempt_family_multiplicity_discount(&mut candidates)?;
    control.check()?;

    let scoring_ordering_start = Instant::now();
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

    let pagination_info = apply_pagination(&mut candidates, &options.pagination, &mut degraded);
    candidate_metrics.subspans.scoring_ordering = scoring_ordering_start.elapsed();
    trace.record_candidate_resolution_subspans(&candidate_metrics.subspans);
    trace.candidate_resolution = candidate_metrics;
    control.check()?;

    let pack_slot_acquisition = try_acquire_pack_slot(
        &options.workspace_path,
        options.output_options.resource_profile,
    );
    let (pack_slot_guard, admission_posture, concurrent_limit_retry_after_ms) =
        match pack_slot_acquisition {
            PackSlotAcquisition::Acquired {
                guard,
                queue_depth,
                concurrent_pack_max,
            } => (
                Some(guard),
                Some(PackAdmissionPosture::admitted(
                    queue_depth,
                    concurrent_pack_max,
                )),
                None,
            ),
            PackSlotAcquisition::LimitReached {
                retry_after_ms,
                queue_depth,
                concurrent_pack_max,
            } => (
                None,
                Some(PackAdmissionPosture::backoff(
                    queue_depth,
                    concurrent_pack_max,
                    retry_after_ms,
                )),
                Some(retry_after_ms),
            ),
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
                (None, None, None)
            }
        };

    let pack_start = Instant::now();
    control.check()?;
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
            include_anti_pattern_first: true,
            output_redaction_enabled,
            // bd-1n0np.5.2: apply the [pack.lod_*] tier-ratio config override when
            // all three basis points are configured (and fit u16); otherwise keep
            // the in-code default so existing pack goldens stay byte-identical.
            // bd-1n0np.5.8: `pack --no-lod` forces `None`, the legacy flat
            // selector that assembles every candidate at the `Full` tier
            // (byte-identical to pre-LOD packs); it overrides any config.
            lod_budget_shares: if options.no_lod {
                None
            } else {
                match context_lod_budget_shares(&options.workspace_path) {
                    Ok(Some(shares)) => Some(shares),
                    _ => crate::pack::PackAssemblyOptions::default().lod_budget_shares,
                }
            },
            // bd-1prrl.7.3: arena mode is plumbed through the
            // `PackAssemblyOptions` surface. Context orchestration
            // selects `Disabled` for now — the parity-gated swap to
            // `RequestScoped` lands with bd-1prrl.7.4 once the
            // golden harness proves byte-identical output.
            arena_mode: crate::pack::ArenaMode::Disabled,
        },
        &determinism,
    )
    .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    apply_context_pack_contradiction_guard(read_connection, &mut draft);
    append_direct_evidence_pack_items(
        read_connection,
        &options.workspace_path,
        &search_report,
        &request,
        &mut draft,
        &mut degraded,
    );
    if !sentinel_omissions.is_empty() {
        let omitted_count = sentinel_omissions.len();
        draft.omitted.extend(sentinel_omissions);
        draft.selection_audit.candidate_count = draft
            .selection_audit
            .candidate_count
            .saturating_add(omitted_count);
        draft.selection_audit.omitted_count = draft.omitted.len();
        draft.hash = None;
    }
    let candidate_token_costs_min = draft
        .selection_audit
        .steps
        .iter()
        .map(|step| step.token_cost)
        .chain(
            draft
                .omitted
                .iter()
                .map(|omission| omission.estimated_tokens),
        )
        .min();
    push_pack_budget_too_small_degradation(
        &mut degraded,
        draft.selection_audit.candidate_count,
        draft.items.len().saturating_add(draft.evidence_items.len()),
        draft.used_tokens,
        draft.budget.max_tokens(),
        candidate_token_costs_min,
    );
    // Only report that the operating profile capped the budget when the cap was actually
    // binding on this pack: either the (capped) token budget was filled, or the (capped)
    // candidate pool was the limiting factor. A profile lowering a ceiling that the pack
    // never came close to using is not a per-response degradation and must not flip the
    // advisory banner. (Fix: false "degraded" banner on healthy packs.)
    let budget_cap_was_binding = (tokens_capped && draft.used_tokens >= effective_max_tokens)
        || (candidate_pool_capped
            && draft.selection_audit.candidate_count >= effective_candidate_pool as usize);
    if (tokens_capped || candidate_pool_capped) && budget_cap_was_binding {
        push_degradation(
            &mut degraded,
            "context_profile_budget_capped",
            ContextResponseSeverity::Low,
            format!(
                "Context request budget was capped by the active {} operating profile and the cap limited this pack (the capped budget was filled or candidates were dropped).",
                runtime_profile.active_profile.as_str()
            ),
            Some("ee profile config plan --json".to_string()),
        );
    }
    let tombstoned_item_count = draft
        .items
        .iter()
        .filter(|item| item.tombstoned_at.is_some())
        .count();
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    push_selected_context_memory_drift_degradations(
        read_connection,
        &options.workspace_path,
        &mut draft,
        &mut degraded,
    );
    let read_pool_stats = read_pool.stats();
    let read_pool_request_ad_hoc_bypass_count = read_pool_stats
        .ad_hoc_bypass_count
        .saturating_sub(read_pool_ad_hoc_bypass_baseline);
    push_context_read_pool_degradations(
        &mut degraded,
        &read_pool_stats,
        read_pool_request_ad_hoc_bypass_count,
    );
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

    trace.record_elapsed("packAssembly", pack_start);
    control.check()?;
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
            admission_posture
                .map(|posture| posture.queue_depth)
                .unwrap_or_else(|| {
                    options
                        .output_options
                        .resource_profile
                        .budget_class()
                        .concurrent_pack_max
                }),
        )
    } else {
        let mut slo = pack_assembly_slo_for_run(
            options.output_options.resource_profile,
            &draft,
            &search_report,
            &trace,
        );
        slo.admission = admission_posture;
        slo
    };
    let _pack_slot_guard = pack_slot_guard;

    let mut response_degraded = degraded.clone();
    response_degraded.extend(slo.context_degradations());
    let consensus_conflicts = crate::pack::analyze_pack_consensus_conflicts(&draft);
    push_consensus_conflict_degradations(
        &mut response_degraded,
        &consensus_conflicts,
        draft.items.len(),
    );
    refresh_context_pack_hash(
        &request,
        &mut draft,
        &response_degraded,
        options.output_options,
        coordination.as_ref(),
        read_snapshot_generation,
        options.task_lens.as_ref(),
    );

    let persist_start = Instant::now();
    control.check()?;
    if options.persist_pack {
        trace.pack_record_writes = trace.pack_record_writes.saturating_add(1);
    }
    let mut pack_persistence = PackPersistenceSubspans::default();
    let mut persist_connection = None;
    let persist_result = if options.persist_pack {
        match context_write_connection.take() {
            Some(connection) => {
                let result = match pack_record_persistence {
                    PackRecordPersistence::Ambient => persist_pack_record_measured(
                        &connection,
                        &options.workspace_path,
                        &request,
                        &draft,
                        &response_degraded,
                        options.task_lens.as_ref(),
                        options.baseline_write.as_ref(),
                        &mut pack_persistence,
                    )
                    .map_err(|error| error.to_string()),
                    PackRecordPersistence::Seeded => persist_pack_record_seeded_measured(
                        &connection,
                        &options.workspace_path,
                        &request,
                        &draft,
                        &response_degraded,
                        &determinism,
                        options.task_lens.as_ref(),
                        options.baseline_write.as_ref(),
                        &mut pack_persistence,
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                };
                persist_connection = Some(connection);
                result
            }
            None => {
                let connection_open_start = Instant::now();
                match DbConnection::open_file(&database_path) {
                    Ok(connection) => {
                        pack_persistence.connection_open = connection_open_start.elapsed();
                        let result = match pack_record_persistence {
                            PackRecordPersistence::Ambient => persist_pack_record_measured(
                                &connection,
                                &options.workspace_path,
                                &request,
                                &draft,
                                &response_degraded,
                                options.task_lens.as_ref(),
                                options.baseline_write.as_ref(),
                                &mut pack_persistence,
                            )
                            .map_err(|error| error.to_string()),
                            PackRecordPersistence::Seeded => persist_pack_record_seeded_measured(
                                &connection,
                                &options.workspace_path,
                                &request,
                                &draft,
                                &response_degraded,
                                &determinism,
                                options.task_lens.as_ref(),
                                options.baseline_write.as_ref(),
                                &mut pack_persistence,
                            )
                            .map(|_| ())
                            .map_err(|error| error.to_string()),
                        };
                        persist_connection = Some(connection);
                        result
                    }
                    Err(error) => {
                        pack_persistence.attempted = true;
                        pack_persistence.connection_open = connection_open_start.elapsed();
                        Err(error.to_string())
                    }
                }
            }
        }
    } else {
        Ok(())
    };
    if options.persist_pack {
        run_after_pack_persistence_hook(control.cx, persist_result.is_ok());
    }
    control.check()?;
    let persist_succeeded = options.persist_pack && persist_result.is_ok();
    pack_persistence.succeeded = persist_succeeded;
    if let Err(persist_error) = persist_result {
        let (message, repair) = context_pack_persist_failed_message_and_repair(&persist_error);
        push_degradation(
            &mut response_degraded,
            "context_pack_persist_failed",
            ContextResponseSeverity::Medium,
            message,
            Some(repair),
        );
        refresh_context_pack_hash(
            &request,
            &mut draft,
            &response_degraded,
            options.output_options,
            coordination.as_ref(),
            read_snapshot_generation,
            options.task_lens.as_ref(),
        );
    }
    if let Some(profile) = agent_profile.as_mut() {
        set_agent_profile_base_pack_hash(profile, draft.hash.as_deref());
    }
    trace.pack_persistence = pack_persistence;
    trace.record_elapsed("packPersistence", persist_start);
    trace.record_read_snapshot(&read_snapshot, read_snapshot_generation);

    let mut response = ContextResponse::new(request, draft, response_degraded)
        .map_err(|error| ContextPackError::Pack(error.to_string()))?;
    response.data.command = command;
    response.data.embed_backend = search_report.embed_backend;
    response.data.adaptive_budget = adaptive_budget_decision;
    response.data.agent_profile = agent_profile;
    response.data.slo = Some(slo);
    response.data.scope_stats = Some(scope_stats);
    response.data.consensus = consensus_conflicts.consensus;
    response.data.conflicts = consensus_conflicts.conflicts;
    response.data.coordination = coordination;
    if pagination_info.applied {
        response.data.pagination = Some(pagination_info.into_response());
    }

    control.check()?;
    if let Some(l2_context) = &l2_cache_context {
        let degraded_count_before_l2_store = response.data.degraded.len();
        context_pack_l2_store(l2_context, options, &search_report, &mut response);
        if response.data.degraded.len() != degraded_count_before_l2_store {
            refresh_context_pack_hash(
                &response.data.request,
                &mut response.data.pack,
                &response.data.degraded,
                options.output_options,
                response.data.coordination.as_ref(),
                read_snapshot_generation,
                options.task_lens.as_ref(),
            );
            if let Some(profile) = response.data.agent_profile.as_mut() {
                set_agent_profile_base_pack_hash(profile, response.data.pack.hash.as_deref());
            }
        }
    }

    control.check()?;
    // Bead bd-17c65.7.7 (G8): best-effort audit-log instrumentation for
    // pack assembly. One `pack.assembled` row per call + one
    // `pack.included_mem` row per selected item. Privacy: only the
    // BLAKE3 prefix of the query reaches the audit log. Failures are
    // swallowed so an audit append never blocks a successful pack.
    let audit_start = Instant::now();
    if options.persist_pack && persist_succeeded {
        if let Some(connection) = persist_connection.as_ref() {
            audit_context_pack_assembly_with_connection(
                connection,
                &options.workspace_path,
                &response,
            );
        } else {
            audit_context_pack_assembly(&database_path, &options.workspace_path, &response);
        }
    }
    trace.pack_persistence.audit = audit_start.elapsed();
    trace.record_pack_persistence_subspans();
    trace.record_elapsed("total", total_start);

    let performance = context_performance_json(
        command,
        options,
        &response.data.request,
        &search_report,
        &response.data.pack,
        &response.data.degraded,
        &trace,
        response
            .data
            .slo
            .as_ref()
            .expect("context response carries pack SLO before performance JSON"),
    );

    control.check()?;
    let search_advisory_snapshot =
        ContextSearchAdvisorySnapshot::from_search_report(&search_report);
    Ok(ContextPackPerformanceRun {
        response,
        performance,
        search_report: Some(search_report),
        search_advisory_snapshot,
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
    audit_context_pack_assembly_with_connection(&conn, workspace_path, response);
}

fn audit_context_pack_assembly_with_connection(
    conn: &DbConnection,
    workspace_path: &Path,
    response: &ContextResponse,
) {
    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_id = crate::core::curate::stable_workspace_id(&canonical_workspace);
    if conn.get_workspace(&workspace_id).ok().flatten().is_none() {
        return;
    }
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
        "adaptiveBudget": response.data.adaptive_budget.as_ref(),
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
    let redaction_count: usize = response
        .data
        .pack
        .items
        .iter()
        .map(|item| item.redactions.len())
        .sum();
    let mut audit_entries =
        Vec::with_capacity(1 + response.data.pack.items.len() + redaction_count);
    audit_entries.push((crate::db::generate_audit_id(), assembled_input));

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
        audit_entries.push((crate::db::generate_audit_id(), item_input));

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
            audit_entries.push((crate::db::generate_audit_id(), redaction_input));
        }
    }

    if conn.insert_audit_batch(&audit_entries).is_err() {
        for (audit_id, input) in audit_entries {
            let _ = conn.insert_audit(&audit_id, &input);
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
                "sourceModeRequested": options.source_mode.as_str(),
                "sourceModeApplied": search_report.source_mode_applied.as_str(),
                "strictSourceMode": search_report.strict_source_mode,
                "fallbackApplied": search_report.source_mode_fallback,
                "memoryScope": options.memory_scope.as_str(),
                "strictScope": options.strict_scope,
            },
            "profileRuntime": search_report.runtime_profile.data_json(),
            "dbReads": context_db_reads_json(trace),
            "search": context_search_json(search_report, options.speed, &trace.search),
            "candidates": candidate_resolution_json(trace),
            "pack": context_pack_json(draft, slo, trace),
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
        "readSnapshot": context_read_snapshot_json(trace.read_snapshot.as_ref()),
        "indexStatusChecks": trace.index_status_checks,
        "memoryBatchReads": trace.candidate_resolution.memory_batch_reads,
        "tagBatchReads": trace.candidate_resolution.tag_batch_reads,
        "artifactLinkReads": trace.candidate_resolution.artifact_link_lookups,
        "focusStateReads": trace.focus_state_read_attempts,
        "packRecordWrites": trace.pack_record_writes,
    })
}

fn context_read_snapshot_json(snapshot: Option<&ReadSnapshotTrace>) -> serde_json::Value {
    match snapshot {
        Some(snapshot) => serde_json::json!({
            "surface": "read_snapshot",
            "pinned": snapshot.pinned,
            "slotId": snapshot.slot_id,
            "leaseHeldMs": snapshot.lease_held_ms,
            "expired": snapshot.expired,
            "poisoned": snapshot.poisoned,
            "snapshotGeneration": snapshot.snapshot_generation,
            "pageCacheHitRatio": null,
            "forkCostUs": null,
        }),
        None => serde_json::json!({
            "surface": "read_snapshot",
            "pinned": false,
            "slotId": null,
            "leaseHeldMs": 0,
            "expired": false,
            "poisoned": false,
            "snapshotGeneration": null,
            "pageCacheHitRatio": null,
            "forkCostUs": null,
        }),
    }
}

fn context_search_json(
    search_report: &SearchReport,
    speed: crate::search::SpeedMode,
    performance: &SearchPerformanceTrace,
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
        "timings": performance.timings_json(),
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
        "tierBoostedCandidates": metrics.tier_boosted_candidates,
        "tierColdCandidates": metrics.tier_cold_candidates,
        "tierRequiredColdCandidates": metrics.tier_required_cold_candidates,
        "filteredBeforeResolution": trace.filtered_count,
        "filterInputCount": trace.filter_input_count,
        "focusStateHits": trace.focus_state_hits,
        "focusCandidateCount": trace.focus_candidate_count,
        "subspans": candidate_resolution_subspans_json(&metrics.subspans),
    })
}

fn candidate_resolution_subspans_json(subspans: &CandidateResolutionSubspans) -> serde_json::Value {
    serde_json::json!({
        "hitIdResolution": duration_timing_json(subspans.hit_id_resolution),
        "memoryIdDedupe": duration_timing_json(subspans.memory_id_dedupe),
        "memoryTagBatchLoad": duration_timing_json(subspans.memory_tag_batch_load),
        "filtering": duration_timing_json(subspans.filtering),
        "freshnessProvenance": duration_timing_json(subspans.freshness_provenance),
        "candidateConstruction": duration_timing_json(subspans.candidate_construction),
        "graphHints": duration_timing_json(subspans.graph_hints),
        "scoringOrdering": duration_timing_json(subspans.scoring_ordering),
    })
}

fn context_pack_json(
    draft: &crate::pack::PackDraft,
    slo: &PackAssemblySlo,
    trace: &ContextPerformanceTrace,
) -> serde_json::Value {
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
        "persistence": pack_persistence_json(trace),
        "hashPresent": draft.hash.is_some(),
    })
}

fn pack_persistence_json(trace: &ContextPerformanceTrace) -> serde_json::Value {
    let subspans = &trace.pack_persistence;
    serde_json::json!({
        "attempted": subspans.attempted,
        "succeeded": subspans.succeeded,
        "packRecordWrites": trace.pack_record_writes,
        "itemCount": subspans.item_count,
        "omissionCount": subspans.omission_count,
        "itemWriteBatches": subspans.item_write_batches,
        "omissionWriteBatches": subspans.omission_write_batches,
        "subspans": {
            "connectionOpen": duration_timing_json(subspans.connection_open),
            "workspaceLookup": duration_timing_json(subspans.workspace_lookup),
            "packHash": duration_timing_json(subspans.pack_hash),
            "degradedSerialization": duration_timing_json(subspans.degraded_serialization),
            "itemInputBuild": duration_timing_json(subspans.item_input_build),
            "omissionInputBuild": duration_timing_json(subspans.omission_input_build),
            "ledgerSerialization": duration_timing_json(subspans.ledger_serialization),
            "recordWrite": duration_timing_json(subspans.record_write),
            "itemWrites": duration_timing_json(subspans.item_writes),
            "omissionWrites": duration_timing_json(subspans.omission_writes),
            "transaction": duration_timing_json(subspans.transaction),
            "transactionOverhead": duration_timing_json(subspans.transaction_overhead()),
            "audit": duration_timing_json(subspans.audit),
        },
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
        "admission": slo.admission.map(|admission| {
            serde_json::json!({
                "outcome": admission.outcome.as_str(),
                "queueDepth": admission.queue_depth,
                "concurrentPackMax": admission.concurrent_pack_max,
                "retryAfterMs": admission.retry_after_ms,
                "waitedMs": admission.waited_ms,
            })
        }),
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
    duration_timing_json(timing.elapsed)
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

fn duration_timing_json(duration: Duration) -> serde_json::Value {
    elapsed_timing_json(duration.as_secs_f64() * 1000.0)
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
        index_freshness: None,
        status: SearchStatus::IndexNotFound,
        embed_backend: crate::core::index::active_embed_backend(),
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
        rerank_configured_mode: crate::config::SearchRerankMode::Auto,
        rerank_configured_top_k: 50,
        rerank_runtime_available: false,
        relevance_floor_applied: None,
        candidates_below_floor: 0,
        query_assist: None,
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
    for entry in search_degraded.iter().filter(|entry| !entry.is_permanent()) {
        let severity = ContextResponseSeverity::parse_lossy(entry.severity.as_str());
        push_degradation(
            degraded,
            &entry.code,
            severity,
            entry.message.clone(),
            entry.repair.clone(),
        );
    }
}

/// Attach the internal search's structured reranker posture to a context
/// response while sharing the long-lived transport's delivery reservation.
///
/// The search renderer also owns stale/large-gap episode accounting. Replace
/// only those two context degradation entries with its delivery-filtered view
/// so one response cannot observe the same episode twice. All other context
/// degradations, including transient reranker load failures, remain visible on
/// every affected response.
pub(crate) fn attach_context_search_advisories_for_delivery(
    response: &mut serde_json::Value,
    search_report: &SearchReport,
    session: &mut SearchAdvisorySession,
    workspace_id: &str,
    reservation: &mut SearchAdvisoryDeliveryReservation,
) {
    let search_data = search_report.data_json_with_advisory_delivery_reservation(
        session,
        workspace_id,
        reservation,
    );
    attach_context_search_advisory_data(response, &search_data);
}

fn attach_context_search_advisory_data(
    response: &mut serde_json::Value,
    search_data: &serde_json::Value,
) {
    if let Some(rerank) = search_data.get("rerank").cloned()
        && let Some(data) = response
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
    {
        data.insert("rerank".to_owned(), rerank);
    }

    let search_stale_entries = search_data
        .get("degraded")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            matches!(
                entry.get("code").and_then(serde_json::Value::as_str),
                Some("search_index_stale" | "search_index_large_gap")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    for pointer in ["/degraded", "/data/degraded"] {
        let Some(entries) = response
            .pointer_mut(pointer)
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        entries.retain(|entry| {
            !matches!(
                entry.get("code").and_then(serde_json::Value::as_str),
                Some("search_index_stale" | "search_index_large_gap")
            )
        });
        entries.extend(search_stale_entries.iter().cloned());
    }
}

pub(crate) fn attach_context_cached_search_advisories_for_delivery(
    response: &mut serde_json::Value,
    snapshot: &ContextSearchAdvisorySnapshot,
    session: &mut SearchAdvisorySession,
    workspace_id: &str,
    reservation: &mut SearchAdvisoryDeliveryReservation,
) {
    let search_data =
        snapshot.data_json_with_delivery_reservation(session, workspace_id, reservation);
    attach_context_search_advisory_data(response, &search_data);
}

fn push_selected_context_memory_drift_degradations(
    connection: &DbConnection,
    workspace_path: &Path,
    draft: &mut PackDraft,
    degraded: &mut Vec<ContextResponseDegradation>,
) {
    let mut hints = Vec::new();
    let mut read_errors = 0usize;
    for item in &mut draft.items {
        match connection.get_memory(&item.memory_id.to_string()) {
            Ok(Some(memory)) => {
                match memory_drift_selection_hint_for_memory(connection, workspace_path, &memory) {
                    Ok(Some(hint)) => {
                        item.freshness_facets
                            .push(pack_freshness_facet_from_memory_drift_hint(&hint));
                        hints.push(hint);
                    }
                    Ok(None) => {}
                    Err(_) => read_errors = read_errors.saturating_add(1),
                }
            }
            Ok(None) => {}
            Err(_) => read_errors = read_errors.saturating_add(1),
        }
    }

    if let Some(hint) = highest_risk_context_memory_drift_hint(&hints) {
        push_degradation(
            degraded,
            hint.degraded_code
                .as_deref()
                .unwrap_or("memory_drift_source_unverifiable"),
            context_severity_for_memory_drift_hint(hint),
            format!(
                "Context pack selected {count} memor{suffix} with stale provenance evidence; highest-risk status={} memoryId={} reason={} evidenceCount={}.",
                hint.drift_status.as_str(),
                hint.memory_id,
                hint.top_reason,
                hint.evidence_count,
                count = hints.len(),
                suffix = if hints.len() == 1 { "y" } else { "ies" },
            ),
            Some(hint.revalidation_command.clone()),
        );
    }

    if read_errors > 0 {
        push_degradation(
            degraded,
            "memory_drift_source_unverifiable",
            ContextResponseSeverity::Medium,
            format!(
                "Context pack could not inspect provenance drift status for {read_errors} selected memor{suffix}.",
                suffix = if read_errors == 1 { "y" } else { "ies" },
            ),
            Some("ee doctor --json".to_owned()),
        );
    }
}

fn pack_freshness_facet_from_memory_drift_hint(
    hint: &MemoryDriftSelectionHint,
) -> PackFreshnessFacet {
    PackFreshnessFacet {
        kind: if hint.stale_anchor {
            "stale_anchor".to_owned()
        } else {
            "memory_drift".to_owned()
        },
        freshness: hint.freshness.clone(),
        stale_anchor: hint.stale_anchor,
        drift_status: hint.drift_status.as_str().to_owned(),
        severity: hint.severity.clone(),
        top_reason: hint.top_reason.clone(),
        degraded_code: hint.degraded_code.clone(),
        revalidation_command: hint.revalidation_command.clone(),
        captured_at_commit: hint.captured_at_commit.clone(),
        current_commit: hint.current_commit.clone(),
        commit_distance: hint.commit_distance,
        changed_regions: hint.changed_regions.clone(),
        anchors: hint
            .anchors
            .iter()
            .map(|anchor| PackFreshnessAnchorFacet {
                anchor_kind: anchor.anchor_kind.clone(),
                anchor_value_hash: anchor.anchor_value_hash.clone(),
                redacted_anchor_value: anchor.redacted_anchor_value.clone(),
                captured_span_hash: anchor.captured_span_hash.clone(),
                freshness_state: anchor.freshness_state.clone(),
                freshness: anchor.freshness.clone(),
                generation: anchor.generation,
                stale_anchor: anchor.stale_anchor,
            })
            .collect(),
    }
}

fn highest_risk_context_memory_drift_hint(
    hints: &[MemoryDriftSelectionHint],
) -> Option<&MemoryDriftSelectionHint> {
    hints.iter().max_by_key(|hint| {
        (
            hint.drift_status.severity_rank(),
            std::cmp::Reverse(hint.memory_id.as_str()),
        )
    })
}

fn context_severity_for_memory_drift_hint(
    hint: &MemoryDriftSelectionHint,
) -> ContextResponseSeverity {
    ContextResponseSeverity::parse_lossy(hint.severity.as_str())
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
    sort_scored_memories_by_score_then_memory_id(&mut scored);

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
            metadata: Some(public_memory_fallback_metadata(&memory, reference_time)),
            explanation: None,
        })
        .collect()
}

fn sort_scored_memories_by_score_then_memory_id(scored: &mut Vec<(StoredMemory, f32)>) {
    scored.sort_by(|(_, left_score), (_, right_score)| right_score.total_cmp(left_score));
    let mut score_run_start = 0_usize;
    while score_run_start < scored.len() {
        let mut score_run_end = score_run_start + 1;
        while score_run_end < scored.len()
            && scored[score_run_start]
                .1
                .total_cmp(&scored[score_run_end].1)
                == std::cmp::Ordering::Equal
        {
            score_run_end += 1;
        }
        sort_scored_memory_score_tie_by_workspace_then_memory_id(
            &mut scored[score_run_start..score_run_end],
        );
        score_run_start = score_run_end;
    }
}

fn sort_scored_memory_score_tie_by_workspace_then_memory_id(scored: &mut [(StoredMemory, f32)]) {
    scored.sort_by(|(left, _), (right, _)| left.workspace_id.cmp(&right.workspace_id));
    let mut run_start = 0_usize;
    while run_start < scored.len() {
        let mut run_end = run_start + 1;
        while run_end < scored.len()
            && scored[run_start].0.workspace_id == scored[run_end].0.workspace_id
        {
            run_end += 1;
        }
        let mut run_slice: Vec<(StoredMemory, f32)> = scored[run_start..run_end].to_vec();
        sort_by_ulid_payload_or_lexical(&mut run_slice, |(memory, _): &(StoredMemory, f32)| {
            memory.id.as_str()
        });
        scored[run_start..run_end].clone_from_slice(&run_slice);
        run_start = run_end;
    }
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
        match connection.list_memories_for_retrieval_with_global(
            &workspace_id,
            None,
            include_tombstoned,
        ) {
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

fn public_memory_fallback_metadata(
    memory: &StoredMemory,
    reference_time: DateTime<Utc>,
) -> serde_json::Value {
    let mut metadata = memory_fallback_metadata(memory, reference_time);
    if let Some(provenance_uri) = memory.provenance_uri.as_deref() {
        metadata["provenanceUri"] =
            serde_json::Value::String(redact_context_public_source_ref(provenance_uri));
    }
    metadata
}

fn redact_context_public_source_ref(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_context_public_path_like_segments(&secret_redacted)
}

fn redact_context_public_path_like_segments(value: &str) -> String {
    const REDACTED_PATH: &str = "[REDACTED_PATH]";
    const PREFIXES: &[&str] = &[
        "/Users/",
        "/Volumes/",
        "/private/",
        "/var/",
        "/tmp/",
        "/home/",
        "/data/",
        "/dp/",
        "/workspace/",
        "/repo/",
        "/etc/",
    ];

    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some((relative_index, _)) = value[cursor..].char_indices().find(|(_, ch)| *ch == '/')
        else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_index;
        if !PREFIXES
            .iter()
            .any(|prefix| value[start..].starts_with(prefix))
        {
            output.push_str(&value[cursor..=start]);
            cursor = start + 1;
            continue;
        }

        output.push_str(&value[cursor..start]);
        output.push_str(REDACTED_PATH);
        cursor = value[start..]
            .char_indices()
            .find_map(|(index, ch)| {
                context_public_source_path_boundary(ch).then_some(start + index)
            })
            .unwrap_or(value.len());
    }
    output
}

fn context_public_source_path_boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '?' | '#' | '"' | '\'' | ')' | ']' | '}' | ',' | ';')
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
    /// Total candidates before pagination was applied.
    pub total: u32,
    /// Whether there are more results after this page.
    pub has_more: bool,
    /// Next cursor token (if has_more is true).
    pub next_cursor: Option<String>,
}

impl PaginationInfo {
    fn into_response(self) -> ContextResponsePagination {
        ContextResponsePagination {
            offset: self.offset,
            limit: self.limit,
            total: self.total,
            page_size: self.page_size,
            has_more: self.has_more,
            next_cursor: self.next_cursor,
        }
    }
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
            total: u32::try_from(total).unwrap_or(u32::MAX),
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
        total: u32::try_from(total).unwrap_or(u32::MAX),
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
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
}

fn annotate_attempt_family_multiplicity(
    connection: &DbConnection,
    candidates: &mut [PackCandidate],
) -> Result<(), ContextPackError> {
    let memory_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id.to_string())
        .collect::<Vec<_>>();
    let batch = connection
        .get_attempt_family_membership_snapshots_for_memory_ids(&memory_ids)
        .map_err(|error| {
            ContextPackError::Pack(format!(
                "failed to batch-resolve authoritative attempt-family membership: {error}"
            ))
        })?;
    for candidate in candidates {
        let memory_id = candidate.memory_id.to_string();
        candidate.attempt_family_multiplicity = batch
            .by_memory_id
            .get(&memory_id)
            .and_then(pack_attempt_family_multiplicity_snapshot);
    }
    Ok(())
}

fn annotate_attempt_family_multiplicity_in_current_snapshot(
    connection: &DbConnection,
    candidates: &mut [PackCandidate],
) -> Result<(), ContextPackError> {
    let memory_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id.to_string())
        .collect::<Vec<_>>();
    let batch = connection
        .get_attempt_family_membership_snapshots_for_memory_ids_in_current_snapshot(&memory_ids)
        .map_err(|error| {
            ContextPackError::Pack(format!(
                "failed to batch-resolve authoritative attempt-family membership: {error}"
            ))
        })?;
    for candidate in candidates {
        let memory_id = candidate.memory_id.to_string();
        candidate.attempt_family_multiplicity = batch
            .by_memory_id
            .get(&memory_id)
            .and_then(pack_attempt_family_multiplicity_snapshot);
    }
    Ok(())
}

fn pack_attempt_family_multiplicity_snapshot(
    snapshot: &crate::db::AttemptFamilyMembershipSnapshot,
) -> Option<PackAttemptFamilyMultiplicitySnapshot> {
    if snapshot.families.is_empty() {
        return None;
    }

    let overall_posture = snapshot.promotion_posture()?;
    let mut effective_discount_factor = 1.0_f32;
    let mut memberships = snapshot
        .families
        .iter()
        .map(|family| {
            let multiplicity = family.multiplicity();
            let dispositions = family
                .ledger_members
                .iter()
                .filter(|member| member.memory_logical_id == snapshot.memory_logical_id)
                .map(|member| member.disposition.as_str())
                .collect::<BTreeSet<_>>();
            let pointer_only = family
                .pointer_only_logical_ids
                .iter()
                .any(|logical_id| logical_id == &snapshot.memory_logical_id);
            let member_disposition = if dispositions.len() == 1
                && dispositions.contains("selected")
                && !pointer_only
            {
                "selected"
            } else if dispositions.len() == 1 && dispositions.contains("rejected") && !pointer_only
            {
                "rejected"
            } else if dispositions.is_empty() && pointer_only {
                "unslotted"
            } else {
                "conflicted"
            };
            let discount_disposition = dispositions
                .contains("selected")
                .then_some("selected")
                .or_else(|| dispositions.contains("rejected").then_some("rejected"));
            let member_discount_factor = multiplicity.member_discount_factor(discount_disposition);
            effective_discount_factor = effective_discount_factor.min(member_discount_factor);
            let posture = multiplicity.promotion_posture();
            PackAttemptFamilyMembershipSnapshot {
                family_alias: crate::models::public_attempt_family_alias(&family.family_id),
                member_disposition: member_disposition.to_owned(),
                member_discount_factor,
                declared_size: multiplicity.declared_size,
                recorded_slots: multiplicity.recorded_slots,
                selected_count: multiplicity.selected_count,
                rejected_count: multiplicity.rejected_count,
                unslotted_count: multiplicity.unslotted_count,
                duplicate_slot_count: multiplicity.duplicate_slot_count,
                duplicate_member_count: multiplicity.duplicate_member_count,
                out_of_range_slot_count: multiplicity.out_of_range_slot_count,
                unrecorded_count: multiplicity.unrecorded_count(),
                promotion_posture: posture.as_str().to_owned(),
                promotion_reason: posture.reason().to_owned(),
            }
        })
        .collect::<Vec<_>>();
    memberships.sort_by(|left, right| left.family_alias.cmp(&right.family_alias));

    Some(PackAttemptFamilyMultiplicitySnapshot {
        schema: PACK_ATTEMPT_FAMILY_MULTIPLICITY_SCHEMA_V1,
        effective_discount_factor,
        promotion_posture: overall_posture.as_str().to_owned(),
        promotion_reason: overall_posture.reason().to_owned(),
        memberships,
    })
}

fn pack_attempt_family_multiplicity_json(
    snapshot: &PackAttemptFamilyMultiplicitySnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "schema": snapshot.schema,
        "effectiveDiscountFactor": snapshot.effective_discount_factor,
        "promotionPosture": snapshot.promotion_posture,
        "promotionReason": snapshot.promotion_reason,
        "memberships": snapshot.memberships.iter().map(|membership| serde_json::json!({
            "familyAlias": membership.family_alias,
            "memberDisposition": membership.member_disposition,
            "memberDiscountFactor": membership.member_discount_factor,
            "declaredSize": membership.declared_size,
            "recordedSlots": membership.recorded_slots,
            "selectedCount": membership.selected_count,
            "rejectedCount": membership.rejected_count,
            "unslottedCount": membership.unslotted_count,
            "duplicateSlotCount": membership.duplicate_slot_count,
            "duplicateMemberCount": membership.duplicate_member_count,
            "outOfRangeSlotCount": membership.out_of_range_slot_count,
            "unrecordedCount": membership.unrecorded_count,
            "promotionPosture": membership.promotion_posture,
            "promotionReason": membership.promotion_reason,
        })).collect::<Vec<_>>(),
    })
}

fn apply_attempt_family_multiplicity_discount(
    candidates: &mut [PackCandidate],
) -> Result<(), ContextPackError> {
    for candidate in candidates {
        let Some(snapshot) = &candidate.attempt_family_multiplicity else {
            continue;
        };
        let factor = snapshot.effective_discount_factor;
        candidate.relevance = UnitScore::parse(candidate.relevance.into_inner() * factor)
            .map_err(|error| ContextPackError::Pack(error.to_string()))?;
        candidate.utility = UnitScore::parse(candidate.utility.into_inner() * factor)
            .map_err(|error| ContextPackError::Pack(error.to_string()))?;
        if let Some(score_breakdown) = &mut candidate.score_breakdown {
            score_breakdown.combined_score =
                (score_breakdown.combined_score * factor).clamp(0.0, 1.0);
        }
    }
    Ok(())
}

fn apply_global_store_pack_policy(
    candidates: &mut Vec<PackCandidate>,
    global_store_memory_ids: &BTreeSet<String>,
    max_tokens: u32,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> usize {
    if candidates.is_empty() || global_store_memory_ids.is_empty() {
        return 0;
    }

    let conflicts = global_lane_conflicts_for_candidates(candidates, global_store_memory_ids);
    annotate_global_lane_conflicts(candidates, &conflicts);
    push_global_lane_conflict_degradation(degraded, &conflicts);

    let protected_global_ids = conflicts
        .iter()
        .map(|conflict| conflict.global_id.clone())
        .collect::<BTreeSet<_>>();
    let global_positions = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            let memory_id = candidate.memory_id.to_string();
            global_store_memory_ids.contains(&memory_id)
                && !protected_global_ids.contains(&memory_id)
        })
        .map(|(index, candidate)| (index, u64::from(candidate.estimated_tokens)))
        .collect::<Vec<_>>();
    if global_positions.is_empty() {
        return 0;
    }

    let costs = global_positions
        .iter()
        .map(|(_, cost)| *cost)
        .collect::<Vec<_>>();
    let fan_in = crate::core::global_store::bounded_global_fan_in(
        &costs,
        u64::from(max_tokens),
        crate::core::global_store::DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS,
        false,
    );
    let selected_positions = fan_in
        .selected
        .iter()
        .filter_map(|selected| global_positions.get(*selected).map(|(index, _)| *index))
        .collect::<BTreeSet<_>>();
    if selected_positions.len() == global_positions.len() {
        return 0;
    }

    let total_global_candidates = global_positions.len();
    let selected_global_candidates = selected_positions.len();
    let before = candidates.len();
    let mut index = 0_usize;
    candidates.retain(|candidate| {
        let current = index;
        index = index.saturating_add(1);
        let memory_id = candidate.memory_id.to_string();
        !global_store_memory_ids.contains(&memory_id)
            || protected_global_ids.contains(&memory_id)
            || selected_positions.contains(&current)
    });
    let removed = before.saturating_sub(candidates.len());
    if removed > 0 {
        let total_suffix = if total_global_candidates == 1 {
            ""
        } else {
            "s"
        };
        let removed_suffix = if removed == 1 { "" } else { "s" };
        push_degradation(
            degraded,
            "global_lane_fan_in_limited",
            ContextResponseSeverity::Low,
            format!(
                "Global memory fan-in kept {selected_global_candidates}/{total_global_candidates} non-conflict global candidate{total_suffix} within the {} token cap; {removed} global candidate{removed_suffix} omitted before pack selection.",
                fan_in.cap_tokens,
            ),
            Some(
                "Use a narrower query or raise the global fan-in quota before relying on more global memories."
                    .to_string(),
            ),
        );
    }
    removed
}

fn global_lane_conflicts_for_candidates(
    candidates: &[PackCandidate],
    global_store_memory_ids: &BTreeSet<String>,
) -> Vec<crate::core::global_store::LaneConflict> {
    if candidates.is_empty() || global_store_memory_ids.is_empty() {
        return Vec::new();
    }
    let saw_workspace = candidates
        .iter()
        .any(|candidate| !global_store_memory_ids.contains(&candidate.memory_id.to_string()));
    let saw_global = candidates
        .iter()
        .any(|candidate| global_store_memory_ids.contains(&candidate.memory_id.to_string()));
    if !saw_workspace || !saw_global {
        return Vec::new();
    }

    let lane_candidates = candidates
        .iter()
        .map(|candidate| {
            let id = candidate.memory_id.to_string();
            crate::core::global_store::LaneCandidate {
                lane: if global_store_memory_ids.contains(&id) {
                    crate::core::global_store::MemoryLane::Global
                } else {
                    crate::core::global_store::MemoryLane::Workspace
                },
                conflict_key: global_lane_conflict_key(candidate),
                content_hash: global_lane_content_hash(&candidate.content),
                id,
            }
        })
        .collect::<Vec<_>>();
    crate::core::global_store::surface_lane_conflicts(&lane_candidates)
}

fn global_lane_conflict_key(candidate: &PackCandidate) -> String {
    if let Some(token) = first_global_lane_subject_token(&candidate.content) {
        return format!("{}:{token}", candidate.section.as_str());
    }
    candidate
        .diversity_key
        .clone()
        .unwrap_or_else(|| format!("{}:{}", candidate.section.as_str(), candidate.memory_id))
}

fn first_global_lane_subject_token(content: &str) -> Option<String> {
    content
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(str::to_ascii_lowercase)
        .find(|token| !global_lane_subject_stopword(token))
}

fn global_lane_subject_stopword(token: &str) -> bool {
    matches!(
        token,
        "about"
            | "after"
            | "again"
            | "agent"
            | "always"
            | "before"
            | "current"
            | "global"
            | "memory"
            | "must"
            | "never"
            | "policy"
            | "project"
            | "repo"
            | "rule"
            | "shared"
            | "should"
            | "that"
            | "this"
            | "when"
            | "with"
            | "without"
            | "workspace"
    )
}

fn global_lane_content_hash(content: &str) -> String {
    let normalized = content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    blake3::hash(normalized.as_bytes()).to_hex().to_string()
}

fn annotate_global_lane_conflicts(
    candidates: &mut [PackCandidate],
    conflicts: &[crate::core::global_store::LaneConflict],
) {
    if conflicts.is_empty() {
        return;
    }
    let mut by_memory_id: BTreeMap<String, Vec<&crate::core::global_store::LaneConflict>> =
        BTreeMap::new();
    for conflict in conflicts {
        by_memory_id
            .entry(conflict.workspace_id.clone())
            .or_default()
            .push(conflict);
        by_memory_id
            .entry(conflict.global_id.clone())
            .or_default()
            .push(conflict);
    }
    for candidate in candidates {
        let memory_id = candidate.memory_id.to_string();
        let Some(conflicts) = by_memory_id.get(&memory_id) else {
            continue;
        };
        let markers = conflicts
            .iter()
            .map(|conflict| {
                format!(
                    "key={} kind={} workspaceId={} globalId={} bothSurfaced={} workspaceOverrides={}",
                    conflict.conflict_key,
                    conflict.kind.as_str(),
                    conflict.workspace_id,
                    conflict.global_id,
                    conflict.both_surfaced,
                    conflict.workspace_overrides,
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        candidate.why = format!("{} globalLane={markers}.", candidate.why);
    }
}

fn push_global_lane_conflict_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    conflicts: &[crate::core::global_store::LaneConflict],
) {
    let contradiction_keys = conflicts
        .iter()
        .filter(|conflict| {
            matches!(
                conflict.kind,
                crate::core::global_store::LaneConflictKind::Contradiction
            )
        })
        .map(|conflict| conflict.conflict_key.clone())
        .collect::<BTreeSet<_>>();
    if contradiction_keys.is_empty() {
        return;
    }
    let keys = contradiction_keys
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let subject_suffix = if contradiction_keys.len() == 1 {
        ""
    } else {
        "s"
    };
    push_degradation(
        degraded,
        "global_lane_conflict_deferred",
        ContextResponseSeverity::Info,
        format!(
            "Global/workspace lane contradiction detected for {} subject{subject_suffix} ({keys}); both sides remain in the pack candidate pool with globalLane markers.",
            contradiction_keys.len(),
        ),
        Some(
            "Review the conflicting workspace/global memories and tombstone or revise the stale lane row."
                .to_string(),
        ),
    );
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
        let adjusted_relevance = if bias.weight.is_nan() {
            base_relevance
        } else {
            (f64::from(base_relevance) + bias.weight).clamp(0.0, 1.0) as f32
        };
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

#[cfg(test)]
fn persist_pack_record(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
) -> Result<(), String> {
    let mut subspans = PackPersistenceSubspans::default();
    persist_pack_record_measured(
        connection,
        workspace_path,
        request,
        draft,
        degraded,
        None,
        None,
        &mut subspans,
    )
}

fn persist_pack_record_measured(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    task_lens: Option<&ContextTaskLens>,
    baseline: Option<&PackBaselineWrite>,
    subspans: &mut PackPersistenceSubspans,
) -> Result<(), String> {
    persist_pack_record_with_pack_id(
        connection,
        workspace_path,
        request,
        draft,
        degraded,
        task_lens,
        baseline,
        PackId::now(),
        subspans,
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
    let mut subspans = PackPersistenceSubspans::default();
    persist_pack_record_seeded_measured(
        connection,
        workspace_path,
        request,
        draft,
        degraded,
        determinism,
        None,
        None,
        &mut subspans,
    )
}

fn persist_pack_record_seeded_measured(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    determinism: &Deterministic<Seed>,
    task_lens: Option<&ContextTaskLens>,
    baseline: Option<&PackBaselineWrite>,
    subspans: &mut PackPersistenceSubspans,
) -> Result<String, String> {
    let mut pack_id_token = determinism.shared_child("ulid.pack");
    persist_pack_record_with_pack_id(
        connection,
        workspace_path,
        request,
        draft,
        degraded,
        task_lens,
        baseline,
        PackId::now_seeded(&mut pack_id_token),
        subspans,
    )
}

fn persist_pack_record_with_pack_id(
    connection: &DbConnection,
    workspace_path: &Path,
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    task_lens: Option<&ContextTaskLens>,
    baseline: Option<&PackBaselineWrite>,
    pack_id: PackId,
    subspans: &mut PackPersistenceSubspans,
) -> Result<String, String> {
    subspans.attempted = true;
    subspans.item_count = draft.items.len().saturating_add(draft.evidence_items.len());
    subspans.omission_count = draft.omitted.len();

    // Bead bd-17c65.1.9 (A9). Pre-overhaul this surface emitted
    // `context_pack_persist_failed: workspace not found` on every call
    // because the lookup used the raw path. `ee init` / `ee remember`
    // canonicalize before registering, so on macOS `/tmp/...` queries
    // miss the registered `/private/tmp/...` row. Try the raw form
    // first (for tests / pre-registered raw paths), then the canonical
    // (symlink-resolved) form. Matches the pattern in G1's
    // resolve_workspace_id_with_fallback.
    let workspace_lookup_start = Instant::now();
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
    subspans.workspace_lookup = workspace_lookup_start.elapsed();

    let pack_hash_start = Instant::now();
    let pack_hash = draft
        .hash
        .clone()
        .unwrap_or_else(|| compute_pack_hash(request, draft, degraded));
    subspans.pack_hash = pack_hash_start.elapsed();

    let degraded_serialization_start = Instant::now();
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
    subspans.degraded_serialization = degraded_serialization_start.elapsed();

    let input = CreatePackRecordInput {
        workspace_id: workspace.id.clone(),
        query: request.query.clone(),
        profile: request.profile.as_str().to_string(),
        max_tokens: request.budget.max_tokens(),
        used_tokens: draft.used_tokens,
        item_count: u32::try_from(draft.items.len().saturating_add(draft.evidence_items.len()))
            .unwrap_or(u32::MAX),
        omitted_count: draft.omitted.len() as u32,
        pack_hash,
        degraded_json,
        created_by: Some("ee context".to_string()),
    };

    let item_input_start = Instant::now();
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
            combined_score: item.score_breakdown.map(|score| score.combined_score),
            attempt_family_multiplicity: item
                .attempt_family_multiplicity
                .as_ref()
                .map(pack_attempt_family_multiplicity_json),
            why: item.why.clone(),
            diversity_key: item.diversity_key.clone(),
            provenance_json: pack_item_provenance_json(&item.provenance),
            trust_class: item.trust.class.as_str().to_string(),
            trust_subclass: item.trust.subclass.clone(),
        })
        .collect();
    let evidence_items: Vec<CreatePackEvidenceItemInput> = draft
        .evidence_items
        .iter()
        .map(|item| CreatePackEvidenceItemInput {
            pack_id: pack_id.to_string(),
            evidence_id: item.evidence_id.clone(),
            entity_revision: item.entity_revision.clone(),
            rank: item.rank,
            section: item.section.as_str().to_owned(),
            estimated_tokens: item.estimated_tokens,
            relevance: item.relevance.into_inner(),
            utility: item.utility.into_inner(),
            why: item.why.clone(),
            provenance_json: pack_item_provenance_json(&item.provenance),
            trust_class: item.trust.class.as_str().to_owned(),
            trust_subclass: item.trust.subclass.clone(),
        })
        .collect();
    subspans.item_input_build = item_input_start.elapsed();

    let omission_input_start = Instant::now();
    let omissions: Vec<CreatePackOmissionInput> = draft
        .omitted
        .iter()
        .map(|omission| CreatePackOmissionInput {
            pack_id: pack_id.to_string(),
            memory_id: omission.memory_id.to_string(),
            estimated_tokens: omission.estimated_tokens,
            reason: omission.reason.as_str().to_string(),
            attempt_family_multiplicity: omission
                .attempt_family_multiplicity
                .as_ref()
                .map(pack_attempt_family_multiplicity_json),
        })
        .collect();
    subspans.omission_input_build = omission_input_start.elapsed();

    let db_task_lens = task_lens.map(|task_lens| CreatePackTaskLensInput {
        id: task_lens.id.clone(),
        version: task_lens.version,
        lens_hash: task_lens.lens_hash.clone(),
    });

    connection
        .insert_pack_record_with_timings_task_lens_and_evidence(
            &pack_id.to_string(),
            &input,
            &items,
            &evidence_items,
            &omissions,
            db_task_lens.as_ref(),
        )
        .map(|timings| subspans.apply_insert_timings(&timings))
        .map_err(|e| format!("insert failed: {e}"))?;

    // bd-7lvbg.6: record the per-agent `--since last` baseline. The pack
    // record above is the durable outcome; a ledger failure must not
    // unwind it, so this is warn-and-continue rather than an error path.
    if let Some(baseline) = baseline {
        let max_rows = context_workspace_config(workspace_path, "Pack baseline ledger")
            .ok()
            .flatten()
            .and_then(|config| config.pack.baseline_ledger_max_rows)
            .and_then(|rows| u32::try_from(rows).ok())
            .unwrap_or(DEFAULT_PACK_BASELINE_LEDGER_MAX_ROWS);
        if let Err(error) = connection.insert_pack_baseline(
            &CreatePackBaselineInput {
                workspace_id: workspace.id.clone(),
                agent_name: baseline.agent_name.clone(),
                task_key: baseline.task_key.clone(),
                pack_id: pack_id.to_string(),
                pack_hash: input.pack_hash.clone(),
            },
            max_rows,
            Some(baseline.agent_name.as_str()),
        ) {
            tracing::warn!(
                target: "ee::pack::baseline",
                pack_id = %pack_id,
                agent = %baseline.agent_name,
                %error,
                "pack baseline ledger write failed; --since last will not see this pack"
            );
        }
    }
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
    read_context_file_to_string_no_follow(path).map_err(|error| error.to_string())
}

fn read_context_file_to_string_no_follow(path: &Path) -> io::Result<String> {
    let mut file = open_context_file_for_read_no_follow(path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

fn open_context_file_for_read_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_context_file_read_options(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_context_file_read_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_context_file_read_options(_options: &mut OpenOptions) {}

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

#[derive(Clone, Debug)]
struct ContextPackL2Context {
    cache: PackL2Cache,
    key: String,
    key_input: PackL2CacheKeyInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContextPackL2SourceModeMetadata {
    requested: SearchSourceMode,
    applied: SearchSourceMode,
    strict: bool,
    fallback: bool,
}

impl ContextPackL2SourceModeMetadata {
    fn from_options(options: &ContextPackOptions) -> Self {
        Self {
            requested: options.source_mode,
            applied: options.source_mode,
            strict: options.strict_source_mode,
            fallback: false,
        }
    }

    fn from_search_report(report: &SearchReport) -> Self {
        Self {
            requested: report.source_mode_requested,
            applied: report.source_mode_applied,
            strict: report.strict_source_mode,
            fallback: report.source_mode_fallback,
        }
    }
}

struct ContextPackL2HitCacheMetadata<'a> {
    key: &'a str,
    byte_len: u64,
    compression: Option<&'a PackL2CompressionHit>,
    source_mode: ContextPackL2SourceModeMetadata,
}

fn context_pack_l2_prepare(
    options: &ContextPackOptions,
    connection: &DbConnection,
    request: &ContextRequest,
    filters: &crate::models::QueryFilters,
    output_redaction_enabled: bool,
    embed_backend: EmbedBackend,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<ContextPackL2Context> {
    let workspace_id = context_pack_l2_workspace_id(connection, &options.workspace_path);
    let cache = match context_pack_l2_cache(&options.workspace_path, &workspace_id) {
        Ok(Some(cache)) => cache,
        Ok(None) => return None,
        Err(message) => {
            push_pack_l2_unavailable(degraded, message);
            return None;
        }
    };
    let database_generation =
        match context_pack_l2_database_generation(connection, Some(&workspace_id)) {
            Ok(generation) => generation,
            Err(message) => {
                push_pack_l2_unavailable(
                    degraded,
                    format!(
                        "L2 pack cache key generation could not read database posture: {message}"
                    ),
                );
                return None;
            }
        };
    let graph_generation = match context_pack_l2_graph_generation(connection) {
        Ok(generation) => generation,
        Err(message) => {
            push_pack_l2_unavailable(
                degraded,
                format!("L2 pack cache key generation could not read graph posture: {message}"),
            );
            return None;
        }
    };
    let personalization_generation = match context_pack_l2_personalization_generation(connection) {
        Ok(generation) => generation,
        Err(message) => {
            push_pack_l2_unavailable(
                degraded,
                format!(
                    "L2 pack cache key generation could not read personalization posture: {message}"
                ),
            );
            return None;
        }
    };
    let key_input = PackL2CacheKeyInput {
        workspace_id,
        database_generation,
        index_generation: context_pack_l2_index_generation(options),
        graph_generation,
        embed_backend,
        redaction_level: options.redaction_level,
        request: request.clone(),
        output_options: options.output_options,
        include_legacy_selection_certificate: env_var_is_set(EnvVar::LegacySelectionCertificate),
        memory_scope: options.memory_scope,
        strict_scope: options.strict_scope,
        source_mode: options.source_mode,
        strict_source_mode: options.strict_source_mode,
        context_feature_flags_hash: context_pack_l2_feature_flags_hash(
            options,
            filters,
            output_redaction_enabled,
        ),
        personalization_generation,
    };
    let key = compute_pack_l2_cache_key(&key_input);

    Some(ContextPackL2Context {
        cache,
        key,
        key_input,
    })
}

fn context_pack_l2_try_hit(
    l2_context: &ContextPackL2Context,
    command: &'static str,
    options: &ContextPackOptions,
    search_options: &SearchOptions,
    connection: &DbConnection,
    request: &ContextRequest,
    total_start: Instant,
    trace: &mut ContextPerformanceTrace,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<ContextPackPerformanceRun> {
    let lookup_start = Instant::now();
    match l2_context.cache.get(&l2_context.key) {
        Ok(PackL2CacheLookup::Hit(hit)) => {
            match context_pack_l2_cached_response_json(
                &hit.pack_json,
                command,
                l2_context.key_input.embed_backend,
            ) {
                Ok(cached_json) => {
                    let mut search_advisory_snapshot =
                        match context_pack_l2_cached_search_advisory_snapshot(&hit.pack_json) {
                            Ok(snapshot) => snapshot,
                            Err(message) => {
                                push_pack_l2_corruption(degraded, message);
                                trace.record_elapsed("packL2Lookup", lookup_start);
                                tracing::warn!(
                                    target: "ee::pack_l2",
                                    event = "pack_l2_cache_corruption",
                                    command,
                                    key = %l2_context.key,
                                    path = %hit.path.display(),
                                    reason = "search_advisory_snapshot_invalid",
                                );
                                return None;
                            }
                        };
                    let source_mode_metadata =
                        context_pack_l2_cached_source_mode_metadata(&hit.pack_json, options);
                    if source_mode_metadata.fallback {
                        trace.record_elapsed("packL2Lookup", lookup_start);
                        tracing::debug!(
                            target: "ee::pack_l2",
                            event = "pack_l2_cache_hit_ignored",
                            command,
                            key = %l2_context.key,
                            path = %hit.path.display(),
                            reason = "cached_source_mode_fallback",
                        );
                        return None;
                    }
                    let current_rerank_posture =
                        ContextSearchAdvisorySnapshot::from_current_rerank_posture(
                            resolve_search_rerank_runtime_posture(
                                search_options,
                                source_mode_metadata.applied,
                                Some(connection),
                            ),
                        );
                    search_advisory_snapshot.refresh_rerank_posture_from(&current_rerank_posture);
                    trace.record_elapsed("packL2Lookup", lookup_start);
                    trace.record_elapsed("total", total_start);
                    tracing::info!(
                        target: "ee::pack_l2",
                        event = "pack_l2_cache_hit",
                        command,
                        key = %l2_context.key,
                        path = %hit.path.display(),
                        byte_len = hit.byte_len,
                        compressed_bytes = hit.compression.as_ref().map(|compression| compression.compressed_bytes).unwrap_or(0),
                        uncompressed_bytes = hit.compression.as_ref().map(|compression| compression.uncompressed_bytes).unwrap_or(hit.byte_len),
                        decompression_latency_ms = hit.compression.as_ref().map(|compression| compression.decompression_latency_ms).unwrap_or(0),
                        dictionary_id = hit.compression.as_ref().and_then(|compression| compression.dictionary_id.as_deref()).unwrap_or("none"),
                        stored_at_epoch_seconds = hit.stored_at_epoch_seconds,
                    );
                    return Some(ContextPackPerformanceRun {
                        response: ContextResponse::from_cached_json_with_command(
                            request.clone(),
                            cached_json,
                            command,
                        ),
                        performance: context_pack_l2_hit_performance_json(
                            command,
                            options,
                            request,
                            trace,
                            ContextPackL2HitCacheMetadata {
                                key: &l2_context.key,
                                byte_len: hit.byte_len,
                                compression: hit.compression.as_ref(),
                                source_mode: source_mode_metadata,
                            },
                        ),
                        search_report: None,
                        search_advisory_snapshot,
                    });
                }
                Err(message) => {
                    push_pack_l2_corruption(degraded, message);
                    tracing::warn!(
                        target: "ee::pack_l2",
                        event = "pack_l2_cache_corruption",
                        command,
                        key = %l2_context.key,
                        path = %hit.path.display(),
                    );
                }
            }
        }
        Ok(PackL2CacheLookup::Miss(miss)) => {
            if pack_l2_miss_is_corruption(&miss) {
                push_pack_l2_corruption(
                    degraded,
                    format!(
                        "L2 pack cache entry {} was rejected: {}",
                        miss.path.display(),
                        pack_l2_miss_reason(&miss.reason)
                    ),
                );
            }
            tracing::debug!(
                target: "ee::pack_l2",
                event = "pack_l2_cache_miss",
                command,
                key = %l2_context.key,
                reason = %pack_l2_miss_reason(&miss.reason),
                fallback_reason = %pack_l2_miss_reason(&miss.reason),
            );
        }
        Err(error) => {
            push_pack_l2_cache_error(degraded, error);
        }
    }
    trace.record_elapsed("packL2Lookup", lookup_start);
    None
}

fn context_pack_l2_store(
    l2_context: &ContextPackL2Context,
    options: &ContextPackOptions,
    search_report: &SearchReport,
    response: &mut ContextResponse,
) {
    let source_mode_metadata = ContextPackL2SourceModeMetadata::from_search_report(search_report);
    let mut store_key_input = l2_context.key_input.clone();
    store_key_input.embed_backend = response.data.embed_backend;
    let store_key = compute_pack_l2_cache_key(&store_key_input);
    if source_mode_metadata.fallback {
        tracing::debug!(
            target: "ee::pack_l2",
            event = "pack_l2_cache_write_skipped",
            key = %store_key,
            reason = "source_mode_fallback",
        );
        return;
    }
    let rendered = crate::output::render_context_response_json_with_options(
        response,
        crate::output::ContextJsonRenderOptions::from(options.output_options),
    );
    let search_advisory_snapshot =
        ContextSearchAdvisorySnapshot::from_search_report(search_report).cache_json();
    let payload = serde_json::json!({
        "schema": PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3,
        "responseJson": rendered,
        "searchAdvisorySnapshot": search_advisory_snapshot,
        "sourceMode": {
            "requested": source_mode_metadata.requested.as_str(),
            "applied": source_mode_metadata.applied.as_str(),
            "strict": source_mode_metadata.strict,
            "fallback": source_mode_metadata.fallback,
        },
    });

    match l2_context.cache.put_compressed(&store_key, &payload) {
        Ok(report) => {
            tracing::info!(
                target: "ee::pack_l2",
                event = "pack_l2_cache_write",
                key = %store_key,
                path = %report.path.display(),
                byte_len = report.byte_len,
                compressed_bytes = report.compression.as_ref().map(|compression| compression.compressed_bytes).unwrap_or(0),
                uncompressed_bytes = report.uncompressed_byte_len,
                compression_latency_ms = report.compression.as_ref().map(|compression| compression.compression_latency_ms).unwrap_or(0),
                dictionary_id = report.compression.as_ref().and_then(|compression| compression.dictionary_id.as_deref()).unwrap_or("none"),
                outcome = %pack_l2_write_outcome(&report.outcome),
                evicted = report.eviction.removed,
                bytes_removed = report.eviction.bytes_removed,
            );
        }
        Err(error) => {
            push_pack_l2_cache_error(&mut response.data.degraded, error);
        }
    }
}

fn context_pack_l2_cache(
    workspace_path: &Path,
    workspace_id: &str,
) -> Result<Option<PackL2Cache>, String> {
    let Some(config) = context_pack_l2_config(workspace_path)? else {
        return Ok(None);
    };
    let root = if config.root.as_os_str().is_empty() {
        context_pack_l2_default_root()
    } else {
        config.root
    };
    let workspace_root = root.join(pack_l2_workspace_component(workspace_id));
    Ok(Some(PackL2Cache::new(
        workspace_root,
        PackL2CacheOptions::new(config.max_bytes, config.max_age)
            .with_max_entry_bytes(config.max_entry_bytes),
    )))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContextPackL2Config {
    root: PathBuf,
    max_bytes: u64,
    max_entry_bytes: u64,
    max_age: Duration,
}

fn context_pack_l2_config(workspace_path: &Path) -> Result<Option<ContextPackL2Config>, String> {
    let project = context_workspace_config(workspace_path, "L2 pack cache")?;
    let project_l2 = project.as_ref().map(|config| &config.cache.pack_l2);
    let disabled_by_env = read_env_bool(EnvVar::L2PackCacheDisable).unwrap_or(false);
    let enabled = !disabled_by_env && project_l2.and_then(|config| config.enabled).unwrap_or(true);
    if !enabled {
        return Ok(None);
    }

    let root = read_env_var(EnvVar::L2PackCacheDir)
        .map(PathBuf::from)
        .or_else(|| project_l2.and_then(|config| config.directory.clone()))
        .unwrap_or_default();
    let max_bytes = read_env_u64(EnvVar::L2PackCacheBytes)
        .or_else(|| project_l2.and_then(|config| config.max_bytes))
        .unwrap_or(PACK_L2_DEFAULT_MAX_BYTES);
    let max_age_days = project_l2
        .and_then(|config| config.max_age_days)
        .unwrap_or(30);

    Ok(Some(ContextPackL2Config {
        root,
        max_bytes,
        max_entry_bytes: crate::cache::pack_l2::DEFAULT_MAX_ENTRY_BYTES,
        max_age: Duration::from_secs(max_age_days.saturating_mul(24 * 60 * 60)),
    }))
}

fn context_pack_l2_default_root() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let shm = Path::new("/dev/shm");
        if shm.is_dir() {
            shm.join("ee").join("pack-l2")
        } else {
            std::env::temp_dir().join("ee").join("pack-l2")
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::temp_dir().join("ee").join("pack-l2")
    }
}

fn context_pack_l2_workspace_id(connection: &DbConnection, workspace_path: &Path) -> String {
    for path in context_workspace_path_keys(workspace_path) {
        if let Ok(Some(workspace)) = connection.get_workspace_by_path(&path.to_string_lossy()) {
            return workspace.id;
        }
    }
    stable_context_workspace_id(workspace_path)
}

fn pack_l2_workspace_component(workspace_id: &str) -> String {
    workspace_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn context_read_snapshot_generation(connection: &DbConnection) -> Result<u64, String> {
    context_pack_l2_database_generation(connection, None)
}

fn context_pack_l2_database_generation(
    connection: &DbConnection,
    workspace_id: Option<&str>,
) -> Result<u64, String> {
    if let Some(generation) = context_pack_l2_workspace_generation(connection, workspace_id)? {
        return Ok(generation);
    }

    context_pack_l2_query_generation(
        connection,
        "SELECT \
            (SELECT COUNT(*) FROM workspaces), \
            (SELECT COALESCE(MAX(updated_at), '') FROM workspaces), \
            (SELECT COUNT(*) FROM memories), \
            (SELECT COALESCE(MAX(updated_at), '') FROM memories), \
            (SELECT COUNT(*) FROM memory_links), \
            (SELECT COALESCE(MAX(created_at), '') FROM memory_links)",
    )
}

fn context_pack_l2_workspace_generation(
    connection: &DbConnection,
    workspace_id: Option<&str>,
) -> Result<Option<u64>, String> {
    let rows = if let Some(workspace_id) = workspace_id {
        connection.query(
            "SELECT generation FROM workspace_generations WHERE workspace_id = ?1",
            &[SqlValue::Text(workspace_id.to_string())],
        )
    } else {
        connection.query(
            "SELECT COALESCE(MAX(generation), 0) FROM workspace_generations",
            &[],
        )
    }
    .map_err(|error| error.to_string())?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let Some(value) = row.get(0).and_then(|value| value.as_i64()) else {
        return Ok(None);
    };
    u64::try_from(value)
        .map(Some)
        .map_err(|_| "workspace generation must fit u64".to_string())
}

fn context_pack_l2_graph_generation(connection: &DbConnection) -> Result<Option<u64>, String> {
    let generation = context_pack_l2_query_generation(
        connection,
        "SELECT \
            COUNT(*), \
            COALESCE(MAX(snapshot_version), 0), \
            COALESCE(MAX(source_generation), 0), \
            COALESCE(MAX(created_at), '') \
         FROM graph_snapshots \
         WHERE status = 'valid'",
    )?;
    Ok((generation != 0).then_some(generation))
}

fn context_pack_l2_personalization_generation(
    connection: &DbConnection,
) -> Result<Option<u64>, String> {
    let generation = context_pack_l2_query_generation(
        connection,
        "SELECT \
            COUNT(*), \
            COALESCE(MAX(last_seen_at), '') \
         FROM agent_context_profiles",
    )?;
    Ok((generation != 0).then_some(generation))
}

fn context_pack_l2_query_generation(connection: &DbConnection, sql: &str) -> Result<u64, String> {
    let rows = connection
        .query(sql, &[])
        .map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    hash_labeled_u64(&mut hasher, "row_count", rows.len() as u64);
    for row in rows {
        for index in 0..8 {
            if let Some(value) = row.get(index) {
                hash_labeled_bytes(
                    &mut hasher,
                    &format!("column_{index}"),
                    format!("{value:?}").as_bytes(),
                );
            }
        }
    }
    Ok(blake3_u64(hasher))
}

fn context_pack_l2_index_generation(options: &ContextPackOptions) -> u64 {
    let index_dir = options
        .index_dir
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("index"));
    let Ok(metadata) = fs::metadata(&index_dir) else {
        return 0;
    };
    let mut hasher = blake3::Hasher::new();
    hash_labeled_bytes(
        &mut hasher,
        "index_dir",
        index_dir.to_string_lossy().as_bytes(),
    );
    hash_labeled_u64(&mut hasher, "len", metadata.len());
    hash_labeled_u64(
        &mut hasher,
        "modified",
        metadata
            .modified()
            .ok()
            .and_then(system_time_epoch_seconds)
            .unwrap_or(0),
    );
    blake3_u64(hasher)
}

fn context_pack_l2_feature_flags_hash(
    options: &ContextPackOptions,
    filters: &crate::models::QueryFilters,
    output_redaction_enabled: bool,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_labeled_bool(
        &mut hasher,
        "output_redaction_enabled",
        output_redaction_enabled,
    );
    hash_labeled_bytes(&mut hasher, "speed", options.speed.as_str().as_bytes());
    hash_labeled_bool(
        &mut hasher,
        "include_tombstoned",
        options.include_tombstoned,
    );
    hash_labeled_bool(&mut hasher, "include_expired", options.include_expired);
    hash_labeled_bool(&mut hasher, "include_future", options.include_future);
    hash_labeled_bool(&mut hasher, "include_stale", options.include_stale);
    hash_labeled_bytes(
        &mut hasher,
        "as_of",
        options
            .as_of
            .map(|timestamp| timestamp.to_rfc3339())
            .unwrap_or_default()
            .as_bytes(),
    );
    hash_labeled_bytes(&mut hasher, "filters", format!("{filters:?}").as_bytes());
    hash_labeled_bytes(
        &mut hasher,
        "ppr_weight",
        options
            .ppr_weight
            .map(|weight| weight.to_bits().to_string())
            .unwrap_or_default()
            .as_bytes(),
    );
    hash_labeled_bool(
        &mut hasher,
        "changed_symbols_from_git",
        options.changed_symbols_from_git,
    );
    hash_labeled_bytes(
        &mut hasher,
        "changed_symbols",
        options.changed_symbols.join("\n").as_bytes(),
    );
    hash_labeled_bytes(
        &mut hasher,
        "pagination",
        format!("{:?}", options.pagination).as_bytes(),
    );
    hash_labeled_bytes(
        &mut hasher,
        "coordination_snapshot",
        options
            .coordination_snapshot_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_bytes(),
    );
    hash_labeled_u64(
        &mut hasher,
        "coordination_stale_after_ms",
        options.coordination_stale_after_ms,
    );
    finalize_blake3(hasher)
}

fn context_pack_l2_cached_response_json(
    payload: &serde_json::Value,
    command: &'static str,
    expected_embed_backend: EmbedBackend,
) -> Result<String, String> {
    let schema = payload
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "L2 pack cache payload is missing schema".to_string())?;
    if schema != PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3 {
        return Err(format!(
            "L2 pack cache payload has unexpected schema {schema}"
        ));
    }
    let response_json = payload
        .get("responseJson")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "L2 pack cache payload is missing responseJson".to_string())?;
    let parsed = serde_json::from_str::<serde_json::Value>(response_json)
        .map_err(|error| format!("L2 pack cache responseJson is malformed: {error}"))?;
    let command_matches = parsed
        .pointer("/data/command")
        .and_then(serde_json::Value::as_str)
        == Some(command);
    let pack_schema_matches = parsed
        .pointer("/data/pack/schema")
        .and_then(serde_json::Value::as_str)
        == Some(PACK_SCHEMA_V2);
    let cached_embed_backend = parsed
        .pointer("/data/embed_backend")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<EmbedBackend>().ok());
    let Some(cached_embed_backend) = cached_embed_backend else {
        return Err("L2 pack cache responseJson is missing a valid data.embed_backend".to_string());
    };
    if cached_embed_backend != expected_embed_backend {
        return Err(format!(
            "L2 pack cache responseJson embed backend {} does not match cache key backend {}",
            cached_embed_backend.as_str(),
            expected_embed_backend.as_str()
        ));
    }
    if command_matches && pack_schema_matches {
        return Ok(response_json.to_owned());
    }
    let mut adjusted = parsed;
    let Some(data) = adjusted
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Err("L2 pack cache responseJson is missing data.command".to_string());
    };
    if !command_matches {
        data.insert(
            "command".to_string(),
            serde_json::Value::String(command.to_string()),
        );
    }
    if !pack_schema_matches {
        let Some(pack) = data
            .get_mut("pack")
            .and_then(serde_json::Value::as_object_mut)
        else {
            return Err("L2 pack cache responseJson is missing data.pack".to_string());
        };
        pack.insert(
            "schema".to_string(),
            serde_json::Value::String(PACK_SCHEMA_V2.to_string()),
        );
    }
    serde_json::to_string(&adjusted)
        .map_err(|error| format!("L2 pack cache responseJson rewrite failed: {error}"))
}

fn context_pack_l2_cached_search_advisory_snapshot(
    payload: &serde_json::Value,
) -> Result<ContextSearchAdvisorySnapshot, String> {
    let snapshot = payload
        .get("searchAdvisorySnapshot")
        .ok_or_else(|| "L2 pack cache payload is missing searchAdvisorySnapshot".to_owned())?;
    ContextSearchAdvisorySnapshot::from_cache_json(snapshot)
}

fn context_pack_l2_cached_source_mode_metadata(
    payload: &serde_json::Value,
    options: &ContextPackOptions,
) -> ContextPackL2SourceModeMetadata {
    let Some(source_mode) = payload.get("sourceMode") else {
        return ContextPackL2SourceModeMetadata::from_options(options);
    };
    let requested = source_mode
        .get("requested")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_cached_search_source_mode)
        .unwrap_or(options.source_mode);
    let applied = source_mode
        .get("applied")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_cached_search_source_mode)
        .unwrap_or(requested);
    let strict = source_mode
        .get("strict")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(options.strict_source_mode);
    let fallback = source_mode
        .get("fallback")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(requested != applied);
    ContextPackL2SourceModeMetadata {
        requested,
        applied,
        strict,
        fallback,
    }
}

fn parse_cached_search_source_mode(value: &str) -> Option<SearchSourceMode> {
    match value {
        "lexical_only" => Some(SearchSourceMode::LexicalOnly),
        "semantic_only" => Some(SearchSourceMode::SemanticOnly),
        "hybrid" => Some(SearchSourceMode::Hybrid),
        _ => None,
    }
}

fn context_pack_l2_hit_performance_json(
    command: &'static str,
    options: &ContextPackOptions,
    request: &ContextRequest,
    trace: &ContextPerformanceTrace,
    cache_hit: ContextPackL2HitCacheMetadata<'_>,
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
                "effectiveCandidatePool": request.candidate_pool,
                "maxTokens": request.budget.max_tokens(),
                "profile": request.profile.as_str(),
                "filtersApplied": !options.filters.is_empty()
                    || options.as_of.is_some()
                    || options.include_expired
                    || options.include_future
                    || options.include_stale,
                "sourceModeRequested": cache_hit.source_mode.requested.as_str(),
                "sourceModeApplied": cache_hit.source_mode.applied.as_str(),
                "strictSourceMode": cache_hit.source_mode.strict,
                "fallbackApplied": cache_hit.source_mode.fallback,
                "memoryScope": options.memory_scope.as_str(),
                "strictScope": options.strict_scope,
            },
            "dbReads": context_db_reads_json(trace),
            "cache": {
                "status": "hit",
                "tier": "l2",
                "key": cache_hit.key,
                "byteLen": cache_hit.byte_len,
                "compressed": cache_hit.compression.is_some(),
                "compressedBytes": cache_hit.compression.map(|compression| compression.compressed_bytes),
                "uncompressedBytes": cache_hit.compression
                    .map(|compression| compression.uncompressed_bytes)
                    .unwrap_or(cache_hit.byte_len),
                "dictionaryId": cache_hit.compression
                    .and_then(|compression| compression.dictionary_id.as_deref()),
                "decompressionLatencyMs": cache_hit.compression
                    .map(|compression| compression.decompression_latency_ms),
                "selectedItemsUnaffected": true,
            },
            "timings": trace.timings.iter().map(performance_timing_json).collect::<Vec<_>>(),
            "fallbacks": [],
            "redaction": performance_redaction_json(),
        },
    })
}

fn pack_l2_miss_is_corruption(miss: &PackL2CacheMiss) -> bool {
    matches!(
        miss.reason,
        PackL2CacheMissReason::Corrupt(_)
            | PackL2CacheMissReason::BodyHashMismatch { .. }
            | PackL2CacheMissReason::KeyMismatch { .. }
            | PackL2CacheMissReason::CompressionDictionaryMissing { .. }
            | PackL2CacheMissReason::CompressionDictionaryCorrupt { .. }
            | PackL2CacheMissReason::CompressionDecode { .. }
    )
}

fn pack_l2_miss_reason(reason: &PackL2CacheMissReason) -> String {
    match reason {
        PackL2CacheMissReason::NotFound => "not_found".to_string(),
        PackL2CacheMissReason::Expired {
            stored_at_epoch_seconds,
        } => format!("expired stored_at_epoch_seconds={stored_at_epoch_seconds}"),
        PackL2CacheMissReason::Corrupt(message) => format!("corrupt {message}"),
        PackL2CacheMissReason::BodyHashMismatch { expected, actual } => {
            format!("body_hash_mismatch expected={expected} actual={actual}")
        }
        PackL2CacheMissReason::KeyMismatch { stored_key } => {
            format!("key_mismatch stored_key={stored_key}")
        }
        PackL2CacheMissReason::TooLarge {
            byte_len,
            max_entry_bytes,
        } => format!("too_large byte_len={byte_len} max_entry_bytes={max_entry_bytes}"),
        PackL2CacheMissReason::CompressionDictionaryMissing { dictionary_id } => {
            format!("compression_dictionary_missing dictionary_id={dictionary_id}")
        }
        PackL2CacheMissReason::CompressionDictionaryCorrupt {
            dictionary_id,
            message,
        } => {
            format!("compression_dictionary_corrupt dictionary_id={dictionary_id} {message}")
        }
        PackL2CacheMissReason::CompressionDecode { message } => {
            format!("compression_decode {message}")
        }
    }
}

fn pack_l2_write_outcome(outcome: &PackL2WriteOutcome) -> &'static str {
    match outcome {
        PackL2WriteOutcome::Stored => "stored",
        PackL2WriteOutcome::SkippedTooLarge { .. } => "skipped_too_large",
    }
}

fn push_pack_l2_cache_error(
    degraded: &mut Vec<ContextResponseDegradation>,
    error: PackL2CacheError,
) {
    push_pack_l2_unavailable(
        degraded,
        format!("L2 pack cache was unavailable; assembled fresh context instead: {error}"),
    );
}

fn push_pack_l2_unavailable(degraded: &mut Vec<ContextResponseDegradation>, message: String) {
    let message = if message.contains("assembled fresh context") {
        message
    } else {
        format!("{message}; assembled fresh context instead.")
    };
    push_degradation(
        degraded,
        "l2_pack_cache_unavailable",
        ContextResponseSeverity::Low,
        message,
        Some("Check [cache.pack_l2] configuration and cache directory permissions.".to_string()),
    );
}

fn push_pack_l2_corruption(degraded: &mut Vec<ContextResponseDegradation>, message: String) {
    let message = if message.contains("rejected") {
        message
    } else {
        format!("L2 pack cache entry rejected: {message}")
    };
    push_degradation(
        degraded,
        "l2_pack_cache_corruption",
        ContextResponseSeverity::Low,
        message,
        Some("Remove the corrupt cache entry or lower the L2 cache TTL.".to_string()),
    );
}

fn system_time_epoch_seconds(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn blake3_u64(hasher: blake3::Hasher) -> u64 {
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackL2CacheKeyInput {
    pub(crate) workspace_id: String,
    pub(crate) database_generation: u64,
    pub(crate) index_generation: u64,
    pub(crate) graph_generation: Option<u64>,
    pub(crate) embed_backend: EmbedBackend,
    pub(crate) redaction_level: RedactionLevel,
    pub(crate) request: ContextRequest,
    pub(crate) output_options: ContextPackOutputOptions,
    pub(crate) include_legacy_selection_certificate: bool,
    pub(crate) memory_scope: MemoryScope,
    pub(crate) strict_scope: bool,
    pub(crate) source_mode: crate::core::search::SearchSourceMode,
    pub(crate) strict_source_mode: bool,
    pub(crate) context_feature_flags_hash: String,
    pub(crate) personalization_generation: Option<u64>,
}

pub(crate) fn compute_pack_l2_cache_key(input: &PackL2CacheKeyInput) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_labeled_bytes(
        &mut hasher,
        "schema",
        PACK_L2_CACHE_KEY_SCHEMA_V4.as_bytes(),
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
        "embed_backend",
        input.embed_backend.as_str().as_bytes(),
    );
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
        "cache_json_response",
        input.output_options.cache_json_response,
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
    hash_labeled_bool(
        &mut hasher,
        "include_legacy_selection_certificate",
        input.include_legacy_selection_certificate,
    );
    hash_labeled_bytes(
        &mut hasher,
        "memory_scope",
        input.memory_scope.as_str().as_bytes(),
    );
    hash_labeled_bool(&mut hasher, "strict_scope", input.strict_scope);
    hash_labeled_bytes(
        &mut hasher,
        "source_mode",
        input.source_mode.as_str().as_bytes(),
    );
    hash_labeled_bool(&mut hasher, "strict_source_mode", input.strict_source_mode);
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

/// Compute the canonical context-pack hash for a draft + request +
/// degraded set, using default output options.
///
/// Exposed (bd-8k08y) so the arena parity harness in
/// `tests/arena_parity_golden.rs` can drive the response/hash path
/// directly rather than only asserting `PackDraft` and
/// `render_context_markdown` byte-equality. The hash is deterministic
/// over `(request, draft, degraded, default ContextPackOutputOptions,
/// no coordination, no read snapshot)`; arena allocation strategy does
/// not participate in any of those inputs.
pub fn compute_pack_hash(
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
    compute_pack_hash_with_output_options_coordination_and_snapshot(
        request,
        draft,
        degraded,
        output_options,
        coordination,
        None,
    )
}

fn compute_pack_hash_with_output_options_coordination_and_snapshot(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
    coordination: Option<&PackCoordinationSnapshot>,
    read_snapshot_generation: Option<u64>,
) -> String {
    compute_pack_hash_with_output_options_coordination_snapshot_and_lens(
        request,
        draft,
        degraded,
        output_options,
        coordination,
        read_snapshot_generation,
        None,
    )
}

fn compute_pack_hash_with_output_options_coordination_snapshot_and_lens(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
    coordination: Option<&PackCoordinationSnapshot>,
    read_snapshot_generation: Option<u64>,
    task_lens: Option<&ContextTaskLens>,
) -> String {
    let components = compute_pack_hash_components(
        request,
        draft,
        degraded,
        output_options,
        coordination,
        read_snapshot_generation,
        task_lens,
    );
    log_pack_hash_components(&components);
    components.composite_hash
}

fn refresh_context_pack_hash(
    request: &ContextRequest,
    draft: &mut crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
    coordination: Option<&PackCoordinationSnapshot>,
    read_snapshot_generation: Option<u64>,
    task_lens: Option<&ContextTaskLens>,
) {
    let hash = compute_pack_hash_with_output_options_coordination_snapshot_and_lens(
        request,
        draft,
        degraded,
        output_options,
        coordination,
        read_snapshot_generation,
        task_lens,
    );
    draft.hash = Some(hash);
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
    read_snapshot_generation: Option<u64>,
    task_lens: Option<&ContextTaskLens>,
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
    hash_labeled_optional_u64(
        &mut request_hasher,
        "read_snapshot_generation",
        read_snapshot_generation,
    );
    hash_context_task_lens(&mut request_hasher, task_lens);

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
    hash_labeled_optional_u64(
        &mut composite_hasher,
        "read_snapshot_generation",
        read_snapshot_generation,
    );
    hash_context_task_lens(&mut composite_hasher, task_lens);
    composite_hasher.update(&draft.used_tokens.to_le_bytes());
    if output_options.include_rendered_text {
        composite_hasher.update(rendered_text.as_bytes());
    }
    if let Some(coordination) = coordination {
        composite_hasher.update(coordination_snapshot_hash_input(coordination).as_bytes());
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
            hash_attempt_family_multiplicity(hasher, item.attempt_family_multiplicity.as_ref());
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
        for facet in &item.freshness_facets {
            for hasher in [&mut draft_hasher, &mut composite_hasher] {
                hasher.update(facet.kind.as_bytes());
                hasher.update(facet.freshness.as_bytes());
                hasher.update(&[u8::from(facet.stale_anchor)]);
                hasher.update(facet.drift_status.as_bytes());
                hasher.update(facet.severity.as_bytes());
                hasher.update(facet.top_reason.as_bytes());
                hasher.update(facet.revalidation_command.as_bytes());
            }
            if let Some(degraded_code) = &facet.degraded_code {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(degraded_code.as_bytes());
                }
            }
            if let Some(captured_at_commit) = &facet.captured_at_commit {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(captured_at_commit.as_bytes());
                }
            }
            if let Some(current_commit) = &facet.current_commit {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(current_commit.as_bytes());
                }
            }
            if let Some(commit_distance) = facet.commit_distance {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(&commit_distance.to_le_bytes());
                }
            }
            for changed_region in &facet.changed_regions {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(changed_region.as_bytes());
                }
            }
            for anchor in &facet.anchors {
                for hasher in [&mut draft_hasher, &mut composite_hasher] {
                    hasher.update(anchor.anchor_kind.as_bytes());
                    hasher.update(anchor.anchor_value_hash.as_bytes());
                    hasher.update(anchor.redacted_anchor_value.as_bytes());
                    hasher.update(anchor.captured_span_hash.as_bytes());
                    hasher.update(anchor.freshness_state.as_bytes());
                    hasher.update(anchor.freshness.as_bytes());
                    hasher.update(&anchor.generation.to_le_bytes());
                    hasher.update(&[u8::from(anchor.stale_anchor)]);
                }
            }
        }
    }
    for item in &draft.evidence_items {
        for hasher in [&mut draft_hasher, &mut composite_hasher] {
            hasher.update(b"evidence_span");
            hasher.update(item.evidence_id.as_bytes());
            hasher.update(item.entity_revision.as_bytes());
            hasher.update(item.session_id.as_bytes());
            hasher.update(&item.start_line.to_le_bytes());
            hasher.update(&item.end_line.to_le_bytes());
            hasher.update(&item.rank.to_le_bytes());
            hasher.update(item.section.as_str().as_bytes());
            hasher.update(item.content.as_bytes());
            hasher.update(&item.estimated_tokens.to_le_bytes());
            hasher.update(&item.relevance.into_inner().to_le_bytes());
            hasher.update(&item.utility.into_inner().to_le_bytes());
            hasher.update(item.why.as_bytes());
            hasher.update(item.trust.class.as_str().as_bytes());
            if let Some(subclass) = &item.trust.subclass {
                hasher.update(subclass.as_bytes());
            }
            for provenance in &item.provenance {
                hasher.update(provenance.uri.to_string().as_bytes());
                hasher.update(provenance.note.as_bytes());
            }
        }
    }
    for omission in &draft.omitted {
        draft_hasher.update(omission.memory_id.to_string().as_bytes());
        draft_hasher.update(&omission.estimated_tokens.to_le_bytes());
        draft_hasher.update(omission.reason.as_str().as_bytes());
        hash_attempt_family_multiplicity(
            &mut draft_hasher,
            omission.attempt_family_multiplicity.as_ref(),
        );
        if output_options.include_skipped {
            composite_hasher.update(omission.memory_id.to_string().as_bytes());
            composite_hasher.update(&omission.estimated_tokens.to_le_bytes());
            composite_hasher.update(omission.reason.as_str().as_bytes());
            hash_attempt_family_multiplicity(
                &mut composite_hasher,
                omission.attempt_family_multiplicity.as_ref(),
            );
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

fn hash_attempt_family_multiplicity(
    hasher: &mut blake3::Hasher,
    snapshot: Option<&PackAttemptFamilyMultiplicitySnapshot>,
) {
    hash_labeled_bool(
        hasher,
        "attempt_family_multiplicity.present",
        snapshot.is_some(),
    );
    let Some(snapshot) = snapshot else {
        return;
    };
    hash_labeled_bytes(
        hasher,
        "attempt_family_multiplicity.schema",
        snapshot.schema.as_bytes(),
    );
    hash_labeled_bytes(
        hasher,
        "attempt_family_multiplicity.effective_discount_factor",
        &snapshot.effective_discount_factor.to_le_bytes(),
    );
    hash_labeled_bytes(
        hasher,
        "attempt_family_multiplicity.promotion_posture",
        snapshot.promotion_posture.as_bytes(),
    );
    hash_labeled_bytes(
        hasher,
        "attempt_family_multiplicity.promotion_reason",
        snapshot.promotion_reason.as_bytes(),
    );
    hash_labeled_u64(
        hasher,
        "attempt_family_multiplicity.membership_count",
        u64::try_from(snapshot.memberships.len()).unwrap_or(u64::MAX),
    );
    for (index, membership) in snapshot.memberships.iter().enumerate() {
        let prefix = format!("attempt_family_multiplicity.membership.{index}");
        hash_labeled_bytes(
            hasher,
            &format!("{prefix}.family_alias"),
            membership.family_alias.as_bytes(),
        );
        hash_labeled_bytes(
            hasher,
            &format!("{prefix}.member_disposition"),
            membership.member_disposition.as_bytes(),
        );
        hash_labeled_bytes(
            hasher,
            &format!("{prefix}.member_discount_factor"),
            &membership.member_discount_factor.to_le_bytes(),
        );
        hash_labeled_optional_u64(
            hasher,
            &format!("{prefix}.declared_size"),
            membership.declared_size.map(u64::from),
        );
        for (label, count) in [
            ("recorded_slots", membership.recorded_slots),
            ("selected_count", membership.selected_count),
            ("rejected_count", membership.rejected_count),
            ("unslotted_count", membership.unslotted_count),
            ("duplicate_slot_count", membership.duplicate_slot_count),
            ("duplicate_member_count", membership.duplicate_member_count),
            (
                "out_of_range_slot_count",
                membership.out_of_range_slot_count,
            ),
            ("unrecorded_count", membership.unrecorded_count),
        ] {
            hash_labeled_u64(hasher, &format!("{prefix}.{label}"), u64::from(count));
        }
        hash_labeled_bytes(
            hasher,
            &format!("{prefix}.promotion_posture"),
            membership.promotion_posture.as_bytes(),
        );
        hash_labeled_bytes(
            hasher,
            &format!("{prefix}.promotion_reason"),
            membership.promotion_reason.as_bytes(),
        );
    }
}

fn hash_context_task_lens(hasher: &mut blake3::Hasher, task_lens: Option<&ContextTaskLens>) {
    hash_labeled_bool(hasher, "task_lens.present", task_lens.is_some());
    if let Some(task_lens) = task_lens {
        hash_labeled_bytes(hasher, "task_lens.id", task_lens.id.as_bytes());
        hash_labeled_u64(hasher, "task_lens.version", u64::from(task_lens.version));
        hash_labeled_bytes(
            hasher,
            "task_lens.lens_hash",
            task_lens.lens_hash.as_bytes(),
        );
    }
}

fn coordination_snapshot_hash_input(coordination: &PackCoordinationSnapshot) -> String {
    serde_json::to_string(coordination)
        .unwrap_or_else(|error| format!("ee_coordination_snapshot_serialization_error:{error}"))
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
    preloaded_memories: Option<&BTreeMap<String, StoredMemory>>,
) -> (Vec<PackCandidate>, CandidateResolutionMetrics) {
    let mut metrics = CandidateResolutionMetrics {
        search_hits: search_report.results.len(),
        ..CandidateResolutionMetrics::default()
    };

    // Phase 1: Resolve all memory IDs from hits (including artifact links).
    // This still does per-hit artifact link lookups but avoids O(k) memory/tag lookups.
    let hit_resolution_start = Instant::now();
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
                    // Procedural-rule hits hydrate through their source
                    // memories the same way artifact hits hydrate through
                    // their memory links (bd-3h6bz).
                    .or_else(|| {
                        rule_linked_memory_id(connection, workspace_path, hit, degraded)
                    })
                    // Imported evidence hits hydrate through the memory the
                    // span was distilled into, when one exists (bd-16imy).
                    .or_else(|| {
                        evidence_linked_memory_id(
                            connection,
                            workspace_path,
                            hit,
                            degraded,
                        )
                    })
            }
        };
        if resolution.is_some() {
            metrics.resolved_memory_ids = metrics.resolved_memory_ids.saturating_add(1);
        }
        hit_resolutions.push((hit, resolution, mesh_provenance));
    }
    metrics.subspans.hit_id_resolution = hit_resolution_start.elapsed();
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
    let memory_id_dedupe_start = Instant::now();
    let memory_ids: Vec<String> = hit_resolutions
        .iter()
        .filter_map(|(_, res, _)| res.as_ref().map(|(mid, _)| mid.to_string()))
        .collect();
    metrics.unique_memory_ids = memory_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let memory_ids_refs: Vec<&str> = memory_ids.iter().map(|s| s.as_str()).collect();
    metrics.subspans.memory_id_dedupe = memory_id_dedupe_start.elapsed();

    // Phase 2: Batch load all memories and tags.
    let batch_load_start = Instant::now();
    let (memories, tags_map, used_preloaded_memories) = load_candidate_batch_maps_with_preloaded(
        connection,
        &memory_ids_refs,
        preloaded_memories,
        degraded,
    );
    metrics.subspans.memory_tag_batch_load = batch_load_start.elapsed();
    metrics.memory_batch_reads =
        usize::from(!memory_ids_refs.is_empty() && !used_preloaded_memories);
    metrics.tag_batch_reads = usize::from(!memory_ids_refs.is_empty());

    // Phase 3: Build candidates from preloaded data.
    let mut candidates = Vec::new();
    let mut freshness_file_cache = crate::core::memory::EvidenceFreshnessFileCache::default();
    for (hit, resolution, mesh_provenance) in hit_resolutions {
        match resolution {
            Some((memory_id, artifact_id)) => {
                let memory_key = memory_id.to_string();
                let filtering_start = Instant::now();
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
                    metrics.subspans.filtering += filtering_start.elapsed();
                    continue;
                }
                if !filters.tags.is_empty() {
                    let tags = tags_map.get(&memory_key).cloned().unwrap_or_default();
                    if !filters.matches_tags(&tags) {
                        metrics.tag_filtered_candidates =
                            metrics.tag_filtered_candidates.saturating_add(1);
                        metrics.subspans.filtering += filtering_start.elapsed();
                        continue;
                    }
                }
                if let Some(memory) = memories.get(&memory_key) {
                    match context_memory_seal_admission(
                        connection,
                        memory,
                        degraded,
                        "context_candidate_memory_batch_unavailable",
                        ContextResponseSeverity::Medium,
                        "Search-hit candidate admission",
                    ) {
                        ContextMemorySealAdmission::Admit => {}
                        ContextMemorySealAdmission::Sealed => {
                            metrics.skipped_candidates =
                                metrics.skipped_candidates.saturating_add(1);
                            push_degradation(
                                degraded,
                                "context_candidate_sealed",
                                ContextResponseSeverity::Info,
                                format!(
                                    "Memory {} is sealed (content committed by hash, not yet revealed) and was excluded from the pack.",
                                    hit.doc_id
                                ),
                                Some(format!(
                                    "ee memory reveal {} --content-file <path> --json",
                                    hit.doc_id
                                )),
                            );
                            metrics.subspans.filtering += filtering_start.elapsed();
                            continue;
                        }
                        ContextMemorySealAdmission::LookupUnavailable => {
                            metrics.skipped_candidates =
                                metrics.skipped_candidates.saturating_add(1);
                            metrics.subspans.filtering += filtering_start.elapsed();
                            continue;
                        }
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
                        metrics.subspans.filtering += filtering_start.elapsed();
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
                        metrics.subspans.filtering += filtering_start.elapsed();
                        continue;
                    }
                    match temporal_memory_outcome(memory, &filters.temporal) {
                        TemporalCandidateOutcome::Include => {}
                        TemporalCandidateOutcome::Exclude => {
                            metrics.temporal_filtered_candidates =
                                metrics.temporal_filtered_candidates.saturating_add(1);
                            metrics.subspans.filtering += filtering_start.elapsed();
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
                        metrics.subspans.filtering += filtering_start.elapsed();
                        continue;
                    };
                    let posture = posture_for_trust_class(&memory.trust_class);
                    if !filters.trust.matches(&memory.trust_class, posture) {
                        metrics.trust_filtered_candidates =
                            metrics.trust_filtered_candidates.saturating_add(1);
                        metrics.subspans.filtering += filtering_start.elapsed();
                        continue;
                    }
                }
                if !filters.redaction.allow_categories.is_empty() {
                    let Some(memory) = memories.get(&memory_key) else {
                        metrics.skipped_candidates = metrics.skipped_candidates.saturating_add(1);
                        metrics.subspans.filtering += filtering_start.elapsed();
                        continue;
                    };
                    if !redaction_allow_categories(&memory.content, &filters.redaction) {
                        metrics.redaction_filtered_candidates =
                            metrics.redaction_filtered_candidates.saturating_add(1);
                        metrics.subspans.filtering += filtering_start.elapsed();
                        continue;
                    }
                }
                metrics.subspans.filtering += filtering_start.elapsed();
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
                    freshness_file_cache: &mut freshness_file_cache,
                };
                match candidate_from_hit_preloaded(
                    preloaded,
                    hit,
                    &memory_key,
                    memory_id,
                    artifact_id,
                    degraded,
                    &mut metrics.subspans,
                ) {
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct MemoryTierCandidateAdmissionMetrics {
    boosted_candidates: usize,
    cold_candidates: usize,
    required_cold_candidates: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GraphHintEvidence {
    seed_memory_id: String,
    depth: u32,
    relation: Option<String>,
    traversal: crate::models::QueryGraphTraversal,
}

fn apply_memory_tier_candidate_admission(
    connection: &DbConnection,
    candidates: &mut [PackCandidate],
    degraded: &mut Vec<ContextResponseDegradation>,
) -> MemoryTierCandidateAdmissionMetrics {
    if candidates.is_empty() {
        return MemoryTierCandidateAdmissionMetrics::default();
    }

    let memory_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id.to_string())
        .collect::<BTreeSet<_>>();
    let memory_id_refs = memory_ids.iter().map(String::as_str).collect::<Vec<_>>();
    let memories = match connection.get_memories_batch(&memory_id_refs) {
        Ok(memories) => memories,
        Err(error) => {
            push_degradation(
                degraded,
                "context_candidate_memory_batch_unavailable",
                ContextResponseSeverity::Medium,
                format!("Memory tier admission could not batch-load candidate memories: {error}"),
                Some("ee status --json".to_string()),
            );
            return MemoryTierCandidateAdmissionMetrics::default();
        }
    };

    apply_memory_tier_candidate_admission_from_memories(
        candidates,
        &memories,
        MemoryTierPolicyConfig::default_swarm(),
    )
}

fn apply_memory_tier_candidate_admission_from_memories(
    candidates: &mut [PackCandidate],
    memories: &BTreeMap<String, StoredMemory>,
    policy: MemoryTierPolicyConfig,
) -> MemoryTierCandidateAdmissionMetrics {
    if candidates.is_empty() || memories.is_empty() {
        return MemoryTierCandidateAdmissionMetrics::default();
    }

    let inputs = candidates
        .iter()
        .filter_map(|candidate| {
            memories
                .get(&candidate.memory_id.to_string())
                .map(|memory| memory_tier_input_for_candidate(candidate, memory))
        })
        .collect::<Vec<_>>();
    if inputs.is_empty() {
        return MemoryTierCandidateAdmissionMetrics::default();
    }

    let assignments = assign_memory_storage_tiers(inputs, policy)
        .into_iter()
        .map(|assignment| (assignment.memory_id.clone(), assignment))
        .collect::<BTreeMap<_, _>>();
    let mut metrics = MemoryTierCandidateAdmissionMetrics::default();
    for candidate in candidates.iter_mut() {
        let Some(assignment) = assignments.get(&candidate.memory_id.to_string()) else {
            continue;
        };
        apply_memory_tier_assignment_to_candidate(candidate, assignment, &mut metrics);
    }
    metrics
}

fn memory_tier_input_for_candidate(
    candidate: &PackCandidate,
    memory: &StoredMemory,
) -> MemoryTierInput {
    MemoryTierInput::from_normalized_scores(
        memory.id.clone(),
        memory.workspace_id.clone(),
        f64::from(memory.confidence),
        f64::from(memory.utility),
        f64::from(memory.importance),
        1.0,
    )
    .with_trust_class(memory.trust_class.clone())
    .with_explicit_query_match(memory_tier_explicit_query_match(candidate))
    .with_safety_or_failure_evidence(memory_tier_safety_or_failure_evidence(&memory.kind))
}

fn memory_tier_explicit_query_match(candidate: &PackCandidate) -> bool {
    candidate.why.starts_with("matched '")
}

fn memory_tier_safety_or_failure_evidence(kind: &str) -> bool {
    let normalized = kind.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "failure" | "risk" | "anti-pattern" | "anti_pattern" | "safety" | "security"
    )
}

fn apply_memory_tier_assignment_to_candidate(
    candidate: &mut PackCandidate,
    assignment: &MemoryTierAssignment,
    metrics: &mut MemoryTierCandidateAdmissionMetrics,
) {
    if assignment.tier == MemoryStorageTier::Cold {
        metrics.cold_candidates = metrics.cold_candidates.saturating_add(1);
        if assignment.required_evidence_preserved {
            metrics.required_cold_candidates = metrics.required_cold_candidates.saturating_add(1);
        }
    }

    let boost = match assignment.tier {
        MemoryStorageTier::Hot => CONTEXT_MEMORY_TIER_HOT_BOOST,
        MemoryStorageTier::Warm => CONTEXT_MEMORY_TIER_WARM_BOOST,
        MemoryStorageTier::Cold => 0.0,
    };
    let base = candidate.relevance.into_inner();
    let adjusted = unit_score(base + boost).unwrap_or(candidate.relevance);
    if adjusted.into_inner() > base {
        candidate.relevance = adjusted;
        metrics.boosted_candidates = metrics.boosted_candidates.saturating_add(1);
    }

    candidate.why = format!(
        "{} tierAdmission tier={} tierScore={} boost={:.4} requiredEvidencePreserved={} noFilter=true policy={} advisoryOnly=true.",
        candidate.why,
        assignment.tier.as_str(),
        assignment.tier_score,
        adjusted.into_inner() - base,
        assignment.required_evidence_preserved,
        assignment.policy_version,
    );
}

fn apply_personalized_pagerank_rerank(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &SearchReport,
    candidates: &mut [PackCandidate],
    ppr_weight: f32,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> PersonalizedPageRankRerankMetrics {
    let ppr_weight = if ppr_weight.is_nan() {
        0.0
    } else {
        ppr_weight.clamp(0.0, 1.0)
    };
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
    let current_generation = match current_memory_links_snapshot_generation(connection) {
        Ok(generation) => generation,
        Err(message) => {
            push_degradation(
                degraded,
                "context_graph_snapshot_unavailable",
                ContextResponseSeverity::Low,
                format!(
                    "PPR rerank skipped because graph source generation could not be checked: {message}"
                ),
                Some("ee graph centrality-refresh".to_string()),
            );
            return PersonalizedPageRankRerankMetrics::default();
        }
    };
    if current_generation != snapshot.source_generation {
        push_degradation(
            degraded,
            GRAPH_PPR_SNAPSHOT_STALE_CODE,
            ContextResponseSeverity::Medium,
            format!(
                "PPR rerank skipped because graph snapshot {} is generation {} but memory_links is generation {}.",
                snapshot.id, snapshot.source_generation, current_generation
            ),
            Some("ee graph snapshot refresh --workspace .".to_string()),
        );
        return PersonalizedPageRankRerankMetrics::default();
    }

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
        let raw_ppr = scores.get(&candidate.memory_id).copied().unwrap_or(0.0);
        let ppr_score = if raw_ppr.is_nan() {
            0.0
        } else {
            raw_ppr.clamp(0.0, 1.0) as f32
        };
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

fn context_memory_tier_admission_enabled(workspace_path: &Path) -> Result<bool, String> {
    let config = context_workspace_config(workspace_path, "Memory tier candidate admission")?;
    Ok(config
        .and_then(|config| config.pack.memory_tier_admission)
        .unwrap_or(false))
}

/// Read the `[pack]` telescoping-LOD tier ratios (bd-1n0np.5.2). Returns an
/// override only when all three basis points are configured AND fit `u16`;
/// otherwise `None`, so the caller keeps the in-code 70/20/10 default.
fn context_lod_budget_shares(
    workspace_path: &Path,
) -> Result<Option<crate::pack::PackLodBudgetShares>, String> {
    let Some(config) = context_workspace_config(workspace_path, "Pack LOD tier ratios")? else {
        return Ok(None);
    };
    let (Some(full), Some(preview), Some(link)) = (
        config.pack.lod_full_basis_points,
        config.pack.lod_truncated_preview_basis_points,
        config.pack.lod_link_only_basis_points,
    ) else {
        return Ok(None);
    };
    // Basis points are bounded to u16; out-of-range config falls back to the
    // in-code default rather than silently truncating.
    match (
        u16::try_from(full),
        u16::try_from(preview),
        u16::try_from(link),
    ) {
        (Ok(full), Ok(preview), Ok(link)) => Ok(Some(crate::pack::PackLodBudgetShares::new(
            full, preview, link,
        ))),
        _ => Ok(None),
    }
}

fn adaptive_budget_decision_for_context(
    workspace_path: &Path,
    explicit_max_tokens: Option<u32>,
    request: &ContextRequest,
    search_report: &SearchReport,
    runtime_profile: &RuntimeProfileReport,
) -> Result<Option<AdaptiveBudgetDecision>, String> {
    if explicit_max_tokens.is_some() {
        return Ok(None);
    }
    let Some(config) = context_workspace_config(workspace_path, "Adaptive pack budget")? else {
        return Ok(None);
    };
    if !config.pack.adaptive_budget.unwrap_or(false) {
        return Ok(None);
    }
    let configured_max_tokens = config
        .pack
        .default_max_tokens
        .and_then(|tokens| u32::try_from(tokens).ok())
        .unwrap_or_else(|| request.budget.max_tokens());
    let (effective_max_tokens, _) = runtime_profile.cap_pack_max_tokens(configured_max_tokens);
    let retrieval_scores = search_report
        .results
        .iter()
        .map(|hit| hit.score)
        .collect::<Vec<_>>();
    Ok(Some(classify_adaptive_budget(
        AdaptiveBudgetInput::new(&request.query, &retrieval_scores, 0.0)
            .with_max_tokens(effective_max_tokens),
    )))
}

/// Upper bound on `.ee/config.toml` reads from the `ee pack` / `ee context`
/// hot path. Real configs are kilobytes to low tens of KiB; 4 MiB is a very
/// generous ceiling that matches the parallel cap `WORKSPACE_CONFIG_MAX_BYTES`
/// in `core::memory` (e1499deb), the operating-profile apply cap (31be37fd),
/// the `ee config get/set` surface cap (47d6b07c), the structural-decay
/// feature-check cap (0fe4a339), and the `load_team_members` cap (696d0324).
/// Without the cap, a peer-planted multi-GB `.ee/config.toml` (accidental
/// — `cat /dev/urandom > .ee/config.toml` — or hostile in a shared
/// multi-agent checkout) would pin a matching allocation on every
/// `ee pack` / `ee context` invocation through eight distinct sub-paths
/// (Pack DNA, L2 pack cache, PPR rerank, memory-tier admission, adaptive
/// pack budget, read-pool snapshot pin, proximity-to-seed scoring, PPR
/// weight). The blast radius is amplified by `ee pack` being the canonical
/// agent surface — one bad config silently OOMs every other agent's
/// pack/context calls for the workspace.
const CONTEXT_WORKSPACE_CONFIG_MAX_BYTES: u64 = 4 * 1024 * 1024;

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
    // Three layers of defense against an oversized `.ee/config.toml`,
    // matching the `read_workspace_config_if_present` shape landed in
    // e1499deb for the parallel `ee remember` hot path:
    //  1. `symlink_metadata().len()` pre-check at stat time, before any
    //     allocation. Refuses with a structured error naming the path
    //     and the ceiling.
    //  2. No-follow open plus opened-metadata checks close the leaf-symlink
    //     and race-grown-file windows between stat and read.
    //  3. `file.take(LIMIT + 1).read_to_end(...)` for the actual read,
    //     bounding allocation if the opened file grows while being read.
    //     Post-read length re-check
    //     converts the bounded read to a TOCTOU-specific error.
    if let Ok(metadata) = fs::symlink_metadata(&config_path) {
        if metadata.len() > CONTEXT_WORKSPACE_CONFIG_MAX_BYTES {
            return Err(format!(
                "{surface} skipped because workspace config {} is {} bytes, exceeding the {CONTEXT_WORKSPACE_CONFIG_MAX_BYTES}-byte ceiling.",
                config_path.display(),
                metadata.len()
            ));
        }
    }
    let mut file = match open_context_file_for_read_no_follow(&config_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "{surface} skipped because workspace config {} could not be read: {error}",
                config_path.display()
            ));
        }
    };
    let opened_metadata = file.metadata().map_err(|error| {
        format!(
            "{surface} skipped because workspace config {} could not be inspected after open: {error}",
            config_path.display()
        )
    })?;
    if !opened_metadata.file_type().is_file() {
        return Err(format!(
            "{surface} skipped because workspace config {} is not a regular file after open.",
            config_path.display()
        ));
    }
    if opened_metadata.len() > CONTEXT_WORKSPACE_CONFIG_MAX_BYTES {
        return Err(format!(
            "{surface} skipped because workspace config {} grew past the {CONTEXT_WORKSPACE_CONFIG_MAX_BYTES}-byte cap after open.",
            config_path.display()
        ));
    }
    let mut bytes = Vec::new();
    if let Err(error) = (&mut file)
        .take(CONTEXT_WORKSPACE_CONFIG_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        return Err(format!(
            "{surface} skipped because workspace config {} could not be read: {error}",
            config_path.display()
        ));
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > CONTEXT_WORKSPACE_CONFIG_MAX_BYTES {
        return Err(format!(
            "{surface} skipped because workspace config {} grew past the {CONTEXT_WORKSPACE_CONFIG_MAX_BYTES}-byte cap after the metadata check (TOCTOU).",
            config_path.display()
        ));
    }
    let contents = match String::from_utf8(bytes) {
        Ok(contents) => contents,
        Err(error) => {
            return Err(format!(
                "{surface} skipped because workspace config {} contents are not valid UTF-8: {error}",
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
    let acquire_timeout_ms = env
        .acquire_timeout_ms
        .or(read_pool.acquire_timeout_ms)
        .unwrap_or(5000);
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

fn context_snapshot_pin_metadata(request: &ContextRequest) -> SnapshotPinMetadata {
    SnapshotPinMetadata {
        workflow_id: Some("context".to_owned()),
        request_id: Some(crate::obs::audit_events::query_hash(&request.query)),
    }
}

fn read_env_u64(var: EnvVar) -> Option<u64> {
    read_env_var(var).and_then(|raw| raw.parse::<u64>().ok())
}

fn read_env_bool(var: EnvVar) -> Option<bool> {
    read_env_var(var).and_then(|raw| parse_env_bool_flag(&raw))
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

fn push_context_read_pool_degradations(
    degraded: &mut Vec<ContextResponseDegradation>,
    stats: &PoolStats,
    request_ad_hoc_bypass_count: u64,
) {
    if request_ad_hoc_bypass_count > 0
        && !degraded
            .iter()
            .any(|entry| entry.code == READ_POOL_ACQUIRE_TIMEOUT_CODE)
    {
        push_degradation(
            degraded,
            READ_POOL_ACQUIRE_TIMEOUT_CODE,
            ContextResponseSeverity::Medium,
            format!(
                "Read pool acquire timeout opened {} ad-hoc read connection{} for this request.",
                request_ad_hoc_bypass_count,
                plural_suffix(request_ad_hoc_bypass_count as usize)
            ),
            Some("increase storage.read_pool.size".to_string()),
        );
    }

    if read_pool_stats_indicate_undersized(stats)
        && !degraded
            .iter()
            .any(|entry| entry.code == READ_POOL_UNDERSIZED_CODE)
    {
        push_degradation(
            degraded,
            READ_POOL_UNDERSIZED_CODE,
            ContextResponseSeverity::Low,
            format!(
                "Read pool appears undersized: acquire wait p99={}ns over {} samples.",
                stats.acquire_wait.p99_ns, stats.acquire_wait.samples
            ),
            Some("increase storage.read_pool.size".to_string()),
        );
    }
}

fn read_pool_stats_indicate_undersized(stats: &PoolStats) -> bool {
    stats.acquire_wait.samples >= READ_POOL_UNDERSIZED_SAMPLE_FLOOR
        && stats.acquire_wait.p99_ns >= READ_POOL_UNDERSIZED_P99_THRESHOLD.as_nanos()
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

    let tree = match context_proximity_tree(connection) {
        Ok(tree) => tree,
        Err(ContextProximityTreeError::Graph(error)) => {
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
        Err(ContextProximityTreeError::GomoryHu(error)) => {
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
                crate::graph::gomory_hu::query_min_cut(tree.as_ref(), &candidate_id, &seed_id)
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ChangedSymbolBoostMetrics {
    boosted_candidates: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SymbolSourceText {
    relative_path: String,
    contents: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CandidateSymbolEvidence {
    memory_id: MemoryId,
    provenance_uri: String,
    target_path: String,
    start_line: u32,
    end_line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChangedSymbolMatch {
    canonical_name: String,
    reason: String,
}

fn apply_changed_symbol_context_boost(
    workspace_path: &Path,
    explicit_symbols: &[String],
    derive_from_git: bool,
    candidates: &mut [PackCandidate],
    degraded: &mut Vec<ContextResponseDegradation>,
) -> ChangedSymbolBoostMetrics {
    let mut selectors = explicit_symbols
        .iter()
        .filter_map(|symbol| normalize_symbol_selector(symbol))
        .collect::<BTreeSet<_>>();
    let mut changed_paths = BTreeSet::new();
    if derive_from_git {
        match changed_rust_paths_from_git(workspace_path) {
            Ok(paths) => changed_paths = paths,
            Err(message) => push_symbol_index_stale_degradation(degraded, message),
        }
    }
    if selectors.is_empty() && changed_paths.is_empty() {
        return ChangedSymbolBoostMetrics::default();
    }

    let evidence = candidate_symbol_evidence(candidates, workspace_path);
    if evidence.is_empty() {
        push_symbol_index_stale_degradation(
            degraded,
            "Symbol index is stale: no file-span provenance was available for changed-symbol context boosting.",
        );
        return ChangedSymbolBoostMetrics::default();
    }

    let source_paths = evidence
        .iter()
        .map(|item| item.target_path.as_str())
        .chain(changed_paths.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let sources = symbol_sources_for_paths(workspace_path, &source_paths, degraded);
    if sources.is_empty() {
        push_symbol_index_stale_degradation(
            degraded,
            "Symbol index is stale: no readable Rust sources were available for changed-symbol context boosting.",
        );
        return ChangedSymbolBoostMetrics::default();
    }
    let source_inputs = sources
        .iter()
        .map(|source| {
            crate::core::symbol_graph::RustSourceInput::new(
                source.relative_path.as_str(),
                source.contents.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let snapshot =
        crate::core::symbol_graph::extract_rust_symbol_snapshot_from_sources(&source_inputs);
    if !snapshot.degraded.is_empty() {
        push_symbol_index_stale_degradation(
            degraded,
            "Symbol index is stale: source degradations were reported while extracting the changed-symbol snapshot.",
        );
    }

    for symbol in &snapshot.symbols {
        if changed_paths.contains(&symbol.path) {
            selectors.insert(normalize_symbol_key(&symbol.canonical_name));
            selectors.insert(normalize_symbol_key(&symbol.id));
            if let Some(short_name) = symbol.canonical_name.rsplit("::").next() {
                selectors.insert(normalize_symbol_key(short_name));
            }
        }
    }
    if selectors.is_empty() {
        return ChangedSymbolBoostMetrics::default();
    }

    let evidence_inputs = evidence
        .iter()
        .map(|item| {
            crate::core::symbol_graph::SymbolEvidenceInput::new(
                crate::models::SymbolEvidenceSourceKind::Memory,
                item.memory_id.to_string(),
                item.provenance_uri.as_str(),
                item.target_path.as_str(),
                item.start_line,
                item.end_line,
                1.0,
            )
        })
        .collect::<Vec<_>>();
    let link_set = crate::core::symbol_graph::link_symbol_evidence(&snapshot, &evidence_inputs);
    if !link_set.degraded.is_empty() {
        push_symbol_index_stale_degradation(
            degraded,
            "Symbol index is stale: some memory evidence links could not be resolved against the changed-symbol snapshot.",
        );
    }

    let symbols_by_id = snapshot
        .symbols
        .iter()
        .map(|symbol| (symbol.id.as_str(), symbol))
        .collect::<BTreeMap<_, _>>();
    let selected_symbols = selected_changed_symbols(&snapshot.symbols, &selectors);
    let mut matches = BTreeMap::<MemoryId, ChangedSymbolMatch>::new();
    for link in &link_set.links {
        let Some(symbol_id) = link.symbol_id.as_deref() else {
            continue;
        };
        let Some(symbol) = symbols_by_id.get(symbol_id) else {
            continue;
        };
        if let Some((anchor, match_kind)) =
            changed_symbol_boost_anchor(symbol, &selectors, &selected_symbols)
        {
            let reason = format!(
                "{}:{}:{}:{}",
                symbol.path,
                symbol.canonical_name,
                link.reason.as_str(),
                changed_symbol_boost_reason(match_kind, anchor)
            );
            if let Ok(memory_id) = MemoryId::from_str(&link.evidence_id) {
                matches.entry(memory_id).or_insert(ChangedSymbolMatch {
                    canonical_name: anchor.canonical_name.clone(),
                    reason,
                });
            }
        }
    }

    let mut boosted_candidates = 0_usize;
    for candidate in candidates {
        let Some(symbol_match) = matches.get(&candidate.memory_id) else {
            continue;
        };
        let base = candidate.relevance.into_inner();
        let boosted = (base + CONTEXT_CHANGED_SYMBOL_BOOST).min(1.0);
        if boosted <= base {
            continue;
        }
        if let Some(score) = unit_score(boosted) {
            candidate.relevance = score;
            candidate.why = format!(
                "{} symbolBoost changedSymbol={} boost={:.4} reason={}.",
                candidate.why,
                symbol_match.canonical_name,
                boosted - base,
                symbol_match.reason
            );
            boosted_candidates = boosted_candidates.saturating_add(1);
        }
    }

    ChangedSymbolBoostMetrics { boosted_candidates }
}

fn candidate_symbol_evidence(
    candidates: &[PackCandidate],
    workspace_path: &Path,
) -> Vec<CandidateSymbolEvidence> {
    let mut evidence = Vec::new();
    for candidate in candidates {
        for provenance in &candidate.provenance {
            let ProvenanceUri::File { path, span } = &provenance.uri else {
                continue;
            };
            let Some(span) = span else {
                continue;
            };
            let Some(target_path) = normalize_symbol_workspace_path(workspace_path, path) else {
                continue;
            };
            if !target_path.ends_with(".rs") {
                continue;
            }
            evidence.push(CandidateSymbolEvidence {
                memory_id: candidate.memory_id,
                provenance_uri: provenance.uri.to_string(),
                target_path,
                start_line: u32_saturating_from_u64(span.start),
                end_line: u32_saturating_from_u64(span.end.unwrap_or(span.start)),
            });
        }
    }
    evidence.sort_by(|left, right| {
        (
            left.memory_id,
            left.target_path.as_str(),
            left.start_line,
            left.end_line,
            left.provenance_uri.as_str(),
        )
            .cmp(&(
                right.memory_id,
                right.target_path.as_str(),
                right.start_line,
                right.end_line,
                right.provenance_uri.as_str(),
            ))
    });
    evidence.dedup();
    evidence
}

fn changed_rust_paths_from_git(workspace_path: &Path) -> Result<BTreeSet<String>, &'static str> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace_path)
        .args(["status", "--porcelain=v1", "--untracked-files=no"])
        .output()
        .map_err(|_| {
            "Symbol index is stale: git status could not be executed for changed-symbol context boosting."
        })?;
    if !output.status.success() {
        return Err(
            "Symbol index is stale: git status failed while deriving changed symbols from the workspace diff.",
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut paths = BTreeSet::new();
    for line in stdout.lines() {
        let Some(raw_path) = line.get(3..) else {
            continue;
        };
        let path = raw_path
            .split(" -> ")
            .last()
            .unwrap_or(raw_path)
            .trim()
            .trim_matches('"');
        if let Some(relative_path) = normalize_symbol_workspace_path(workspace_path, path)
            && relative_path.ends_with(".rs")
        {
            paths.insert(relative_path);
        }
    }
    Ok(paths)
}

fn symbol_sources_for_paths(
    workspace_path: &Path,
    paths: &BTreeSet<&str>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<SymbolSourceText> {
    let mut sources = Vec::new();
    for path in paths {
        let Some(relative_path) = normalize_symbol_workspace_path(workspace_path, path) else {
            continue;
        };
        if !relative_path.ends_with(".rs") {
            continue;
        }
        let absolute_path = workspace_path.join(&relative_path);
        match read_symbol_source_no_follow_bounded(
            &absolute_path,
            crate::core::symbol_graph::DEFAULT_MAX_RUST_SOURCE_BYTES,
        ) {
            Ok(contents) => sources.push(SymbolSourceText {
                relative_path,
                contents,
            }),
            Err(_) => push_symbol_index_stale_degradation(
                degraded,
                "Symbol index is stale: a Rust source referenced by changed-symbol context boosting could not be read.",
            ),
        }
    }
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    sources.dedup_by(|left, right| left.relative_path == right.relative_path);
    sources
}

/// Read a Rust source file via the no-follow opener with a hard cap on the
/// allocation. The unbounded `read_context_file_to_string_no_follow` path
/// pre-sizes the `String` from the file's current length on every read, so a
/// peer-grown `.rs` file matching one of the `changed_symbols` paths would
/// inflate the allocation without bound on every `ee pack` invocation.
/// Wrapping the handle in `file.take(max_bytes + 1)` mirrors the
/// `symbol_graph::extract_paths` fix (27a3cb9b) so the parallel reader in
/// the pack hot path obeys the same `DEFAULT_MAX_RUST_SOURCE_BYTES` ceiling.
/// Over-cap reads land in the existing `symbol_index_stale` degraded code via
/// the caller's `Err` arm — same observable behavior as an unreadable
/// source.
fn read_symbol_source_no_follow_bounded(path: &Path, max_bytes: u64) -> io::Result<String> {
    let file = open_context_file_for_read_no_follow(path)?;
    let mut bytes = Vec::new();
    (&file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Rust source {} exceeds the {max_bytes}-byte changed-symbol context cap",
                path.display()
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn normalize_symbol_workspace_path(workspace_path: &Path, raw_path: &str) -> Option<String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    let relative = if path.is_absolute() {
        path.strip_prefix(workspace_path).ok()?.to_path_buf()
    } else {
        path
    };
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    let normalized = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy()),
            std::path::Component::CurDir => None,
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    (!normalized.is_empty()).then_some(normalized)
}

fn symbol_matches_selectors(
    symbol: &crate::models::SymbolRecord,
    selectors: &BTreeSet<String>,
) -> bool {
    let canonical = normalize_symbol_key(&symbol.canonical_name);
    let id = normalize_symbol_key(&symbol.id);
    if selectors.contains(&canonical) || selectors.contains(&id) {
        return true;
    }
    symbol
        .canonical_name
        .rsplit("::")
        .next()
        .map(normalize_symbol_key)
        .is_some_and(|name| selectors.contains(&name))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChangedSymbolBoostMatchKind {
    Direct,
    Adjacent,
}

fn selected_changed_symbols<'a>(
    symbols: &'a [crate::models::SymbolRecord],
    selectors: &BTreeSet<String>,
) -> Vec<&'a crate::models::SymbolRecord> {
    symbols
        .iter()
        .filter(|symbol| symbol_matches_selectors(symbol, selectors))
        .collect()
}

fn changed_symbol_boost_anchor<'a>(
    symbol: &'a crate::models::SymbolRecord,
    selectors: &BTreeSet<String>,
    selected_symbols: &[&'a crate::models::SymbolRecord],
) -> Option<(&'a crate::models::SymbolRecord, ChangedSymbolBoostMatchKind)> {
    if symbol_matches_selectors(symbol, selectors) {
        return Some((symbol, ChangedSymbolBoostMatchKind::Direct));
    }
    selected_symbols
        .iter()
        .copied()
        .find(|anchor| symbol_is_adjacent_to_changed_symbol(symbol, anchor))
        .map(|anchor| (anchor, ChangedSymbolBoostMatchKind::Adjacent))
}

fn symbol_is_adjacent_to_changed_symbol(
    symbol: &crate::models::SymbolRecord,
    anchor: &crate::models::SymbolRecord,
) -> bool {
    if symbol.id == anchor.id || symbol.path != anchor.path {
        return false;
    }
    symbol_line_gap(symbol, anchor) <= CONTEXT_CHANGED_SYMBOL_ADJACENCY_LINE_WINDOW
}

fn symbol_line_gap(left: &crate::models::SymbolRecord, right: &crate::models::SymbolRecord) -> u32 {
    if left.range.end_line < right.range.start_line {
        right.range.start_line.saturating_sub(left.range.end_line)
    } else if right.range.end_line < left.range.start_line {
        left.range.start_line.saturating_sub(right.range.end_line)
    } else {
        0
    }
}

fn changed_symbol_boost_reason(
    match_kind: ChangedSymbolBoostMatchKind,
    anchor: &crate::models::SymbolRecord,
) -> String {
    match match_kind {
        ChangedSymbolBoostMatchKind::Direct => "direct".to_string(),
        ChangedSymbolBoostMatchKind::Adjacent => {
            format!("adjacent_to={}", anchor.canonical_name)
        }
    }
}

fn normalize_symbol_selector(raw: &str) -> Option<String> {
    let normalized = normalize_symbol_key(raw);
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_symbol_key(raw: &str) -> String {
    raw.trim()
        .trim_end_matches("()")
        .to_ascii_lowercase()
        .replace('\\', "/")
}

fn u32_saturating_from_u64(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX).max(1)
}

fn push_symbol_index_stale_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    message: impl Into<String>,
) {
    let message = message.into();
    if degraded.iter().any(|entry| {
        entry.code == crate::models::symbol::SYMBOL_INDEX_STALE_CODE && entry.message == message
    }) {
        return;
    }
    push_degradation(
        degraded,
        crate::models::symbol::SYMBOL_INDEX_STALE_CODE,
        ContextResponseSeverity::Low,
        message,
        Some("ee symbol snapshot --workspace . --refresh".to_string()),
    );
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum ContextProximityTreeError {
    Graph(String),
    GomoryHu(String),
}

fn context_proximity_tree(
    connection: &DbConnection,
) -> Result<Arc<crate::graph::gomory_hu::GomoryHuTree>, ContextProximityTreeError> {
    let generation = context_proximity_graph_generation(connection).ok();
    if let Some(generation) = generation {
        if let Some(tree) = cached_context_proximity_tree(generation) {
            return Ok(tree);
        }
    }

    let graph = context_proximity_graph(connection).map_err(ContextProximityTreeError::Graph)?;
    let tree = Arc::new(
        crate::graph::gomory_hu::build_gomory_hu_tree(&graph)
            .map_err(|error| ContextProximityTreeError::GomoryHu(error.to_string()))?,
    );

    if let Some(generation) = generation {
        store_context_proximity_tree(generation, Arc::clone(&tree));
    }

    Ok(tree)
}

fn cached_context_proximity_tree(
    generation: u64,
) -> Option<Arc<crate::graph::gomory_hu::GomoryHuTree>> {
    let guard = context_proximity_tree_cache()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_ref()
        .filter(|cached| cached.generation == generation)
        .map(|cached| Arc::clone(&cached.tree))
}

fn store_context_proximity_tree(generation: u64, tree: Arc<crate::graph::gomory_hu::GomoryHuTree>) {
    let mut guard = context_proximity_tree_cache()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(CachedContextProximityTree { generation, tree });
}

fn context_proximity_tree_cache() -> &'static RwLock<Option<CachedContextProximityTree>> {
    CONTEXT_PROXIMITY_TREE_CACHE.get_or_init(|| RwLock::new(None))
}

fn context_proximity_graph_generation(connection: &DbConnection) -> Result<u64, String> {
    context_pack_l2_query_generation(
        connection,
        "SELECT \
            COUNT(*), \
            COALESCE(MAX(created_at), '') \
         FROM memory_links",
    )
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

fn configured_context_ppr_weight(workspace_path: &Path) -> Result<Option<f32>, String> {
    let Some(config) = context_workspace_config(workspace_path, "Personalized PageRank weight")?
    else {
        return Ok(None);
    };
    if !config.graph.feature.ppr_enabled.unwrap_or(false) {
        return Ok(None);
    }
    Ok(Some(
        config
            .graph
            .ppr
            .alpha
            .map(|alpha| alpha as f32)
            .unwrap_or(DEFAULT_CONTEXT_PPR_WEIGHT),
    ))
}

fn effective_context_ppr_weight(value: Option<f32>, configured: Option<f32>) -> f32 {
    match value {
        Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
        Some(_) => DEFAULT_CONTEXT_PPR_WEIGHT,
        None => match configured {
            Some(configured) if configured.is_finite() => configured.clamp(0.0, 1.0),
            _ => 0.0,
        },
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

fn current_memory_links_snapshot_generation(connection: &DbConnection) -> Result<u32, String> {
    let visible_count = connection
        .list_all_memory_links(None)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|link| {
            crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
        })
        .count();
    u32::try_from(visible_count)
        .map_err(|_| format!("visible memory link count {visible_count} does not fit u32"))
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
        if !matches!(
            context_memory_seal_admission(
                connection,
                memory,
                degraded,
                "context_candidate_memory_batch_unavailable",
                ContextResponseSeverity::Medium,
                "Graph candidate admission",
            ),
            ContextMemorySealAdmission::Admit
        ) {
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
        if !matches!(
            context_memory_seal_admission(
                connection,
                seed_memory,
                degraded,
                "context_graph_neighborhood_unavailable",
                ContextResponseSeverity::Low,
                "Graph seed admission",
            ),
            ContextMemorySealAdmission::Admit
        ) {
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
        let frontier_refs: Vec<&str> = frontier.iter().map(String::as_str).collect();
        let frontier_links = match connection.list_memory_links_for_memories(&frontier_refs, None) {
            Ok(links) => links,
            Err(error) => {
                push_degradation(
                    degraded,
                    "context_graph_neighborhood_unavailable",
                    ContextResponseSeverity::Low,
                    format!(
                        "Graph frontier neighborhood at depth {depth} could not be read: {error}"
                    ),
                    Some("ee graph neighborhood <memory-id> --json".to_string()),
                );
                break;
            }
        };
        let mut links_by_frontier = BTreeMap::<String, Vec<&crate::db::StoredMemoryLink>>::new();
        for link in &frontier_links {
            if frontier.contains(&link.src_memory_id) {
                links_by_frontier
                    .entry(link.src_memory_id.clone())
                    .or_default()
                    .push(link);
            }
            if link.dst_memory_id != link.src_memory_id && frontier.contains(&link.dst_memory_id) {
                links_by_frontier
                    .entry(link.dst_memory_id.clone())
                    .or_default()
                    .push(link);
            }
        }
        for memory_id in &frontier {
            let Some(links) = links_by_frontier.get(memory_id) else {
                continue;
            };
            for edge in crate::graph::graph_neighborhood_edges_from_links(
                memory_id,
                direction,
                links.iter().copied(),
            ) {
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
            if !matches!(
                context_memory_seal_admission(
                    connection,
                    neighbor_memory,
                    degraded,
                    "context_graph_neighborhood_unavailable",
                    ContextResponseSeverity::Low,
                    "Graph neighbor admission",
                ),
                ContextMemorySealAdmission::Admit
            ) {
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
    preloaded_memories: Option<&BTreeMap<String, StoredMemory>>,
    global_store_memory_ids: &BTreeSet<String>,
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

    let candidate_memory_ids: BTreeSet<String> = candidates
        .iter()
        .map(|candidate| candidate.memory_id.to_string())
        .collect();
    let candidate_memory_refs: Vec<&str> =
        candidate_memory_ids.iter().map(String::as_str).collect();
    let (mut scope_memories, read_error): (BTreeMap<String, StoredMemory>, Option<String>) =
        match connection.get_memories_batch(&candidate_memory_refs) {
            Ok(memories) => (memories, None),
            Err(error) => (BTreeMap::new(), Some(error.to_string())),
        };
    if let Some(preloaded) = preloaded_memories {
        for memory_id in &candidate_memory_ids {
            if let Some(memory) = preloaded.get(memory_id) {
                scope_memories
                    .entry(memory_id.clone())
                    .or_insert_with(|| memory.clone());
            }
        }
    }
    let (scope_tags, tag_read_error): (BTreeMap<String, Vec<String>>, Option<String>) =
        if matches!(scope_context.scope, MemoryScope::Global) {
            match connection.get_memory_tags_batch(&candidate_memory_refs) {
                Ok(tags) => (tags, None),
                Err(error) => (BTreeMap::new(), Some(error.to_string())),
            }
        } else {
            (BTreeMap::new(), None)
        };

    let mut scoped = Vec::with_capacity(candidates.len());
    let global_scope_tags = [GLOBAL_MEMORY_SCOPE_TAG.to_owned()];
    for candidate in std::mem::take(candidates) {
        let memory_id = candidate.memory_id.to_string();
        match scope_memories.get(&memory_id) {
            Some(memory) => {
                let tags = if matches!(scope_context.scope, MemoryScope::Global)
                    && global_store_memory_ids.contains(&memory_id)
                {
                    global_scope_tags.as_slice()
                } else {
                    scope_tags.get(&memory_id).map(Vec::as_slice).unwrap_or(&[])
                };
                let in_scope = scope_context.memory_in_scope_with_tags(memory, tags);
                stats.record_candidate_id(in_scope, Some(&memory_id));
                if in_scope {
                    scoped.push(candidate);
                }
            }
            None => {
                stats.record_candidate_id(false, Some(&memory_id));
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
    if let Some(error) = tag_read_error {
        push_degradation(
            degraded,
            "scope_metadata_unavailable",
            ContextResponseSeverity::Medium,
            format!("Context could not verify global memory scope tags: {error}"),
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

fn global_store_search_memory_ids(
    search_report: &crate::core::search::SearchReport,
) -> BTreeSet<String> {
    search_report
        .results
        .iter()
        .filter(|hit| {
            hit.metadata
                .as_ref()
                .and_then(|metadata| metadata.get("storeLane"))
                .and_then(serde_json::Value::as_str)
                == Some(crate::core::global_store::GLOBAL_PROVENANCE_LANE)
        })
        .filter_map(|hit| MemoryId::from_str(&hit.doc_id).ok())
        .map(|memory_id| memory_id.to_string())
        .collect()
}

fn filter_candidates_by_required_fresh_sentinels(
    connection: &DbConnection,
    candidates: &mut Vec<PackCandidate>,
    reference_time: DateTime<Utc>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Result<Vec<PackOmission>, ContextPackError> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut retained = Vec::with_capacity(candidates.len());
    let mut omitted = Vec::new();
    let mut failure_examples = Vec::new();
    for candidate in std::mem::take(candidates) {
        let memory_id = candidate.memory_id.to_string();
        match sentinel_candidate_freshness(connection, &memory_id, reference_time)? {
            SentinelCandidateFreshness::NoSentinels | SentinelCandidateFreshness::Fresh => {
                retained.push(candidate);
            }
            SentinelCandidateFreshness::Blocked(reason) => {
                if failure_examples.len() < 3 {
                    failure_examples.push(format!("{memory_id}:{reason}"));
                }
                omitted.push(PackOmission {
                    memory_id: candidate.memory_id,
                    estimated_tokens: candidate.estimated_tokens,
                    relevance: candidate.relevance,
                    utility: candidate.utility,
                    attempt_family_multiplicity: candidate.attempt_family_multiplicity.clone(),
                    reason: PackOmissionReason::ExcludedByPolicy,
                    rejected_at: PackRejectionStage::Selection,
                    feasible: false,
                    could_fit_with_budget: None,
                });
            }
        }
    }
    let omitted_count = omitted.len();
    *candidates = retained;
    if omitted_count > 0 {
        push_degradation(
            degraded,
            "context_filtered_results",
            ContextResponseSeverity::Medium,
            format!(
                "{omitted_count} sentinel-backed candidate memor{} excluded by --require-fresh-sentinels{}.",
                if omitted_count == 1 {
                    "y was"
                } else {
                    "ies were"
                },
                if failure_examples.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", failure_examples.join(", "))
                }
            ),
            Some(
                "Run `ee sentinel check --workspace . --json` before requiring fresh sentinels."
                    .to_string(),
            ),
        );
    }
    Ok(omitted)
}

enum SentinelCandidateFreshness {
    NoSentinels,
    Fresh,
    Blocked(&'static str),
}

fn sentinel_candidate_freshness(
    connection: &DbConnection,
    memory_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<SentinelCandidateFreshness, ContextPackError> {
    let specs: Vec<_> = connection
        .list_memory_sentinel_specs(memory_id)
        .map_err(|error| {
            ContextPackError::Storage(format!("Failed to load sentinel specs: {error}"))
        })?
        .into_iter()
        // Revive-polarity sentinels watch for a retired memory's blocker to
        // clear: their predicate is EXPECTED to fail while the memory stays
        // down, so they must never gate serving. Only gate-polarity specs
        // participate in freshness (bd-wake-on-condition-inverse-sentinel-65uci).
        .filter(|spec| spec.polarity == crate::models::MemorySentinelPolarity::Gate)
        .collect();
    if specs.is_empty() {
        return Ok(SentinelCandidateFreshness::NoSentinels);
    }
    for spec in specs {
        let latest = connection
            .latest_memory_sentinel_result(&spec.spec_hash)
            .map_err(|error| {
                ContextPackError::Storage(format!("Failed to load sentinel result: {error}"))
            })?;
        let Some(latest) = latest else {
            return Ok(SentinelCandidateFreshness::Blocked("missing_result"));
        };
        match latest.status {
            MemorySentinelResultStatus::Pass => {}
            MemorySentinelResultStatus::Fail => {
                return Ok(SentinelCandidateFreshness::Blocked("fail"));
            }
            MemorySentinelResultStatus::Unknown => {
                return Ok(SentinelCandidateFreshness::Blocked("unknown"));
            }
            MemorySentinelResultStatus::Degraded => {
                return Ok(SentinelCandidateFreshness::Blocked("degraded"));
            }
        }
        if sentinel_result_stale(
            &latest.checked_at,
            latest
                .stale_threshold_seconds
                .or(spec.stale_threshold_seconds),
            reference_time,
        ) {
            return Ok(SentinelCandidateFreshness::Blocked("stale"));
        }
    }
    Ok(SentinelCandidateFreshness::Fresh)
}

fn sentinel_result_stale(
    checked_at: &str,
    stale_threshold_seconds: Option<u64>,
    reference_time: DateTime<Utc>,
) -> bool {
    let Some(threshold) = stale_threshold_seconds else {
        return false;
    };
    let Ok(checked_at) =
        DateTime::parse_from_rfc3339(checked_at).map(|timestamp| timestamp.with_timezone(&Utc))
    else {
        return true;
    };
    reference_time
        .signed_duration_since(checked_at)
        .num_seconds()
        > threshold as i64
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
    let (memories, tags_map, _) =
        load_candidate_batch_maps_with_preloaded(connection, memory_ids, None, degraded);
    (memories.into_owned(), tags_map)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContextMemorySealAdmission {
    Admit,
    Sealed,
    LookupUnavailable,
}

/// Resolve placeholder-shaped content against durable seal-sidecar truth.
///
/// Ordinary candidates avoid the DB lookup entirely. Exact-placeholder
/// candidates are admitted only when the sidecar query succeeds and proves
/// that no seal row exists; lookup failure excludes fail closed and emits an
/// existing context degradation selected by the caller.
fn context_memory_seal_admission(
    connection: &DbConnection,
    memory: &StoredMemory,
    degraded: &mut Vec<ContextResponseDegradation>,
    lookup_degradation_code: &str,
    lookup_degradation_severity: ContextResponseSeverity,
    lookup_surface: &str,
) -> ContextMemorySealAdmission {
    if memory.content != crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT {
        return ContextMemorySealAdmission::Admit;
    }
    match connection.get_memory_seal(&memory.id) {
        Ok(Some(_)) => ContextMemorySealAdmission::Sealed,
        Ok(None) => ContextMemorySealAdmission::Admit,
        Err(error) => {
            push_degradation(
                degraded,
                lookup_degradation_code,
                lookup_degradation_severity,
                format!(
                    "{lookup_surface} could not verify seal sidecar state for memory {}; the candidate was excluded fail closed: {error}",
                    memory.id
                ),
                Some("ee status --json".to_string()),
            );
            ContextMemorySealAdmission::LookupUnavailable
        }
    }
}

enum CandidateMemoryBatch<'a> {
    Owned(BTreeMap<String, StoredMemory>),
    Borrowed(&'a BTreeMap<String, StoredMemory>),
}

impl CandidateMemoryBatch<'_> {
    fn get(&self, memory_id: &str) -> Option<&StoredMemory> {
        match self {
            Self::Owned(memories) => memories.get(memory_id),
            Self::Borrowed(memories) => memories.get(memory_id),
        }
    }

    fn into_owned(self) -> BTreeMap<String, StoredMemory> {
        match self {
            Self::Owned(memories) => memories,
            Self::Borrowed(memories) => memories.clone(),
        }
    }
}

fn load_candidate_batch_maps_with_preloaded<'a>(
    connection: &DbConnection,
    memory_ids: &[&str],
    preloaded_memories: Option<&'a BTreeMap<String, StoredMemory>>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> (
    CandidateMemoryBatch<'a>,
    BTreeMap<String, Vec<String>>,
    bool,
) {
    if memory_ids.is_empty() {
        return (
            CandidateMemoryBatch::Owned(BTreeMap::new()),
            BTreeMap::new(),
            true,
        );
    }

    let unique_memory_ids = memory_ids.iter().copied().collect::<BTreeSet<&str>>();
    let preloaded_covers_all = preloaded_memories
        .map(|preloaded| {
            unique_memory_ids
                .iter()
                .all(|memory_id| preloaded.contains_key(*memory_id))
        })
        .unwrap_or(false);

    let memories = if let Some(preloaded) = preloaded_memories.filter(|_| preloaded_covers_all) {
        CandidateMemoryBatch::Borrowed(preloaded)
    } else {
        CandidateMemoryBatch::Owned(match connection.get_memories_batch(memory_ids) {
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
        })
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

    (memories, tags_map, preloaded_covers_all)
}

struct PreloadedCandidateSource<'a> {
    memories: &'a CandidateMemoryBatch<'a>,
    tags_map: &'a BTreeMap<String, Vec<String>>,
    workspace_path: &'a Path,
    query: &'a str,
    validity_reference_time: Option<DateTime<Utc>>,
    include_tombstoned: bool,
    freshness_file_cache: &'a mut crate::core::memory::EvidenceFreshnessFileCache,
}

struct FocusCandidateSource<'a> {
    connection: &'a DbConnection,
    focus_state: &'a crate::models::FocusState,
    workspace_path: &'a Path,
    workspace_ids: &'a BTreeSet<String>,
    focus_hash: &'a str,
    storage_path: &'a str,
    include_tombstoned: bool,
    include_expired: bool,
    include_future: bool,
    validity_reference_time: DateTime<Utc>,
}

fn candidate_from_hit_preloaded(
    source: PreloadedCandidateSource<'_>,
    hit: &crate::core::search::SearchHit,
    memory_key: &str,
    memory_id: MemoryId,
    artifact_id: Option<String>,
    degraded: &mut Vec<ContextResponseDegradation>,
    subspans: &mut CandidateResolutionSubspans,
) -> Option<PackCandidate> {
    let memory = match source.memories.get(memory_key) {
        Some(memory) if memory.tombstoned_at.is_none() => memory,
        Some(memory) if source.include_tombstoned => memory,
        _ => return None,
    };
    let tags = source.tags_map.get(&memory.id).cloned().unwrap_or_default();
    let provenance_start = Instant::now();
    let provenance = provenance_for_memory_cached(
        memory,
        memory_id,
        source.workspace_path,
        degraded,
        source.freshness_file_cache,
    );
    subspans.freshness_provenance += provenance_start.elapsed();
    let provenance = provenance?;
    let construction_start = Instant::now();
    let Some(relevance) = pack_candidate_relevance_from_search_hit(hit) else {
        subspans.candidate_construction += construction_start.elapsed();
        return None;
    };
    let Some(utility) = unit_score(memory.utility) else {
        subspans.candidate_construction += construction_start.elapsed();
        return None;
    };
    let content = memory.content.clone();
    let why = candidate_selection_why(
        source.query,
        hit.source.as_str(),
        relevance.into_inner(),
        memory.utility,
        artifact_id.as_deref(),
    );
    let candidate = match PackCandidate::new(PackCandidateInput {
        memory_id,
        section: section_for_memory(memory),
        content,
        estimated_tokens: estimate_tokens_default(&memory.content),
        relevance,
        utility,
        provenance: vec![provenance],
        why,
    }) {
        Ok(candidate) => candidate,
        Err(_) => {
            subspans.candidate_construction += construction_start.elapsed();
            return None;
        }
    };

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
    subspans.candidate_construction += construction_start.elapsed();
    Some(candidate)
}

fn focus_candidates_from_state(
    connection: &DbConnection,
    workspace_path: &Path,
    focus_state: &crate::models::FocusState,
    include_tombstoned: bool,
    include_expired: bool,
    include_future: bool,
    validity_reference_time: DateTime<Utc>,
    workspace_ids: &BTreeSet<String>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<PackCandidate> {
    let mut candidates = Vec::new();
    let focus_hash = focus_state_hash(focus_state);
    let storage_path = focus_state_path(workspace_path).display().to_string();
    let source = FocusCandidateSource {
        connection,
        focus_state,
        workspace_path,
        workspace_ids,
        focus_hash: &focus_hash,
        storage_path: &storage_path,
        include_tombstoned,
        include_expired,
        include_future,
        validity_reference_time,
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
    if !source.workspace_ids.contains(&memory.workspace_id) {
        push_degradation(
            degraded,
            "context_focus_workspace_filtered",
            ContextResponseSeverity::Low,
            format!(
                "Focused memory {} belongs to a different workspace and was excluded from context.",
                item.memory_id
            ),
            Some(format!("ee focus remove {} --json", item.memory_id)),
        );
        return None;
    }
    if !matches!(
        fallback_memory_validity_visibility(
            &memory,
            source.validity_reference_time,
            source.include_expired,
            source.include_future,
            false,
        ),
        FallbackMemoryVisibility::Visible
    ) {
        push_degradation(
            degraded,
            "context_focus_temporal_filtered",
            ContextResponseSeverity::Low,
            format!(
                "Focused memory {} is outside the requested validity window and was excluded from context.",
                item.memory_id
            ),
            Some(format!("ee focus remove {} --json", item.memory_id)),
        );
        return None;
    }
    match context_memory_seal_admission(
        source.connection,
        &memory,
        degraded,
        "context_focus_memory_lookup_unavailable",
        ContextResponseSeverity::Low,
        "Focus candidate admission",
    ) {
        ContextMemorySealAdmission::Admit => {}
        ContextMemorySealAdmission::Sealed => {
            push_degradation(
                degraded,
                "context_focus_sealed_memory",
                ContextResponseSeverity::Info,
                format!(
                    "Focused memory {} is sealed and was excluded from context until reveal.",
                    item.memory_id
                ),
                Some(format!(
                    "ee memory reveal {} --content-file <path> --json",
                    item.memory_id
                )),
            );
            return None;
        }
        ContextMemorySealAdmission::LookupUnavailable => return None,
    }
    if !crate::policy::redact_secret_like_content(&memory.content)
        .redacted_reasons
        .is_empty()
    {
        push_degradation(
            degraded,
            "context_focus_secret_filtered",
            ContextResponseSeverity::Low,
            format!(
                "Focused memory {} contains secret-like content and was excluded from context.",
                item.memory_id
            ),
            Some(format!("ee focus remove {} --json", item.memory_id)),
        );
        return None;
    }
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
    // The linked-document slot carries a registered artifact id, an applied
    // procedural rule id (bd-3h6bz), or an imported evidence span id
    // (bd-16imy); label the attribution honestly.
    if let Some(linked_id) = artifact_id {
        if linked_id.starts_with("rule_") {
            format!("{base}; via applied procedural rule {linked_id}")
        } else if linked_id.starts_with("ev_") {
            format!("{base}; via imported evidence {linked_id}")
        } else {
            format!("{base}; via registered artifact {linked_id}")
        }
    } else {
        base
    }
}

fn artifact_linked_memory_id(
    connection: &DbConnection,
    hit: &crate::core::search::SearchHit,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<(MemoryId, Option<String>)> {
    let claims_artifact = hit
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("source"))
        .and_then(serde_json::Value::as_str)
        == Some("artifact")
        || hit.doc_id.starts_with("art_");
    if !claims_artifact {
        return None;
    }
    if !is_registry_artifact_id(&hit.doc_id) {
        push_degradation(
            degraded,
            "context_artifact_lookup_unavailable",
            ContextResponseSeverity::Low,
            "A malformed artifact identifier from the derived index was excluded.".to_owned(),
            Some("ee index rebuild --json".to_owned()),
        );
        return None;
    }
    let artifact_id = hit.doc_id.clone();
    match connection.get_artifact(&artifact_id) {
        Ok(Some(_)) => {}
        Ok(None) => return None,
        Err(error) => {
            push_degradation(
                degraded,
                "context_artifact_lookup_unavailable",
                ContextResponseSeverity::Low,
                format!("Artifact {artifact_id} could not be loaded: {error}"),
                Some(format!("ee artifact inspect {artifact_id} --json")),
            );
            return None;
        }
    }

    let links = match connection.list_artifact_links(&artifact_id) {
        Ok(links) => links,
        Err(error) => {
            push_degradation(
                degraded,
                "context_artifact_links_unavailable",
                ContextResponseSeverity::Low,
                format!("Artifact links for {artifact_id} could not be loaded: {error}"),
                Some(format!("ee artifact inspect {artifact_id} --json")),
            );
            return None;
        }
    };

    for link in links {
        if link.target_type != "memory" {
            continue;
        }
        match MemoryId::from_str(&link.target_id) {
            Ok(memory_id) => return Some((memory_id, Some(artifact_id.clone()))),
            Err(_) => push_degradation(
                degraded,
                "context_artifact_memory_link_invalid",
                ContextResponseSeverity::Low,
                format!("Artifact {artifact_id} links to a malformed memory identifier."),
                Some(format!("ee artifact inspect {artifact_id} --json")),
            ),
        }
    }

    push_degradation(
        degraded,
        "context_artifact_unlinked",
        ContextResponseSeverity::Low,
        format!(
            "Artifact {} matched search but has no valid memory link for context packing.",
            artifact_id
        ),
        Some("ee artifact register <path> --link-memory <memory-id> --json".to_string()),
    );
    None
}

/// Resolve a procedural-rule search hit to one of its source memories so the
/// hit can hydrate into the pack's `procedural_rules` section (bd-3h6bz).
///
/// Rules are indexed as first-class `source=rule` documents, but the pack
/// candidate model is memory-centric, so a rule hit hydrates through its
/// `rule_source_memories` linkage — mirroring how artifact hits hydrate
/// through their memory links. The source-memory pick is deterministic
/// (lexicographically smallest id). The rule id rides along as the linked
/// document so the candidate's `why` names the applied rule. A matched rule
/// with no hydratable source memory degrades honestly instead of being
/// silently dropped: the rule stays retrievable via `ee search`.
fn rule_linked_memory_id(
    connection: &DbConnection,
    workspace_path: &Path,
    hit: &crate::core::search::SearchHit,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<(MemoryId, Option<String>)> {
    let claims_rule = hit
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("source"))
        .and_then(serde_json::Value::as_str)
        == Some("rule")
        || hit.doc_id.starts_with("rule_");
    if !claims_rule {
        return None;
    }
    let rule_id = match RuleId::from_str(&hit.doc_id) {
        Ok(id) => id.to_string(),
        Err(_) => {
            push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                "A malformed rule identifier from the derived index was excluded.".to_owned(),
                Some("ee index rebuild --json".to_owned()),
            );
            return None;
        }
    };

    let rule = match connection.get_procedural_rule(&rule_id) {
        Ok(Some(rule)) => rule,
        Ok(None) => {
            push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!("Rule {rule_id} matched a stale index but no live source row exists."),
                Some("ee index rebuild --json".to_owned()),
            );
            return None;
        }
        Err(error) => {
            push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!(
                    "Rule {} matched search but could not be loaded for context packing: {error}",
                    rule_id
                ),
                Some(format!("ee rule show {rule_id} --json")),
            );
            return None;
        }
    };

    let workspace = match connection.get_workspace(&rule.workspace_id) {
        Ok(Some(workspace)) => workspace,
        Ok(None) => {
            push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!("Rule {rule_id} has no live workspace for context admission."),
                Some("ee doctor --json".to_owned()),
            );
            return None;
        }
        Err(error) => {
            push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!("Rule {rule_id} workspace admission failed: {error}"),
                Some("ee doctor --json".to_owned()),
            );
            return None;
        }
    };
    let stored_workspace_path = Path::new(&workspace.path);
    let same_workspace = match (
        std::fs::canonicalize(stored_workspace_path),
        std::fs::canonicalize(workspace_path),
    ) {
        (Ok(stored), Ok(requested)) => stored == requested,
        _ => stored_workspace_path == workspace_path,
    };
    if !same_workspace {
        push_degradation(
            degraded,
            "context_rule_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!("Rule {rule_id} belongs to a different workspace and was excluded."),
            Some("ee index rebuild --json".to_owned()),
        );
        return None;
    }

    let tags = match connection.get_rule_tags(&rule_id) {
        Ok(tags) => tags,
        Err(error) => {
            push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!("Tags for rule {rule_id} could not be loaded: {error}"),
                Some(format!("ee rule show {rule_id} --json")),
            );
            return None;
        }
    };
    let source_memory_ids = match connection.get_rule_source_memory_ids(&rule_id) {
        Ok(ids) => ids,
        Err(error) => {
            push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!(
                    "Source memories for rule {} could not be loaded for context packing: {error}",
                    rule_id
                ),
                Some(format!("ee rule show {rule_id} --json")),
            );
            return None;
        }
    };
    let projection = RuleIndexProjection::new(rule, stored_workspace_path, tags, source_memory_ids);
    let indexed_revision = hit
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("entity_revision"))
        .and_then(serde_json::Value::as_str);
    if !projection.is_pack_admissible() || indexed_revision != Some(projection.entity_revision()) {
        push_degradation(
            degraded,
            "context_rule_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!(
                "Rule {rule_id} is no longer pack-admissible or its derived index revision is stale."
            ),
            Some("ee index rebuild --json".to_owned()),
        );
        return None;
    }

    for source_memory_id in projection.source_memory_ids() {
        match MemoryId::from_str(source_memory_id) {
            Ok(memory_id) => return Some((memory_id, Some(rule_id.clone()))),
            Err(_) => push_degradation(
                degraded,
                "context_rule_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!("Rule {rule_id} references a malformed source memory identifier."),
                Some(format!("ee rule show {rule_id} --json")),
            ),
        }
    }

    push_degradation(
        degraded,
        "context_rule_hit_unhydrated",
        ContextResponseSeverity::Low,
        format!(
            "Rule {} matched search but has no source memories to hydrate into the pack; the rule remains retrievable via ee search.",
            rule_id
        ),
        Some(format!(
            "ee rule update {} --source-memory <memory-id> --json",
            rule_id
        )),
    );
    None
}

fn append_direct_evidence_pack_items(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &crate::core::search::SearchReport,
    request: &ContextRequest,
    draft: &mut PackDraft,
    degraded: &mut Vec<ContextResponseDegradation>,
) {
    if !request.sections.is_empty() && !request.sections.contains(&PackSection::Evidence) {
        return;
    }

    let workspace_ids = context_workspace_ids(connection, workspace_path, degraded);
    let selected_memory_ids = draft
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut rejected_live_admission = 0_usize;

    for hit in &search_report.results {
        if !hit.doc_id.starts_with("ev_") || !seen.insert(hit.doc_id.clone()) {
            continue;
        }
        let Ok(evidence_id) = EvidenceId::from_str(&hit.doc_id) else {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        };
        let Ok(Some(span)) = connection.get_evidence_span(&evidence_id.to_string()) else {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        };
        if !workspace_ids.iter().any(|id| id == &span.workspace_id) {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        }
        let Ok(Some(session)) = connection.get_session(&span.session_id) else {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        };
        if !span.is_direct_pack_admitted_for_session(&span.workspace_id, &session) {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        }
        if span
            .memory_id
            .as_ref()
            .is_some_and(|memory_id| selected_memory_ids.contains(memory_id))
        {
            continue;
        }

        let estimated_tokens = estimate_tokens_default(&span.excerpt).max(1);
        let Some(next_used_tokens) = draft.used_tokens.checked_add(estimated_tokens) else {
            continue;
        };
        if next_used_tokens > draft.budget.max_tokens() {
            continue;
        }
        let Ok(provenance_uri) = ProvenanceUri::from_str(&span.canonical_provenance_uri()) else {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        };
        let Ok(provenance) = PackProvenance::new(
            provenance_uri,
            format!(
                "Imported CASS transcript span {} lines {}-{}",
                span.id, span.start_line, span.end_line
            ),
        ) else {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        };
        let relevance =
            UnitScore::parse(hit.relevance_score()).unwrap_or_else(|_| UnitScore::zero());
        let utility = UnitScore::neutral();
        let rank = u32::try_from(
            draft
                .items
                .len()
                .saturating_add(draft.evidence_items.len())
                .saturating_add(1),
        )
        .unwrap_or(u32::MAX);
        let mut revision_hasher = blake3::Hasher::new();
        revision_hasher.update(b"evidence_span");
        revision_hasher.update(span.id.as_bytes());
        revision_hasher.update(span.content_hash.as_bytes());
        revision_hasher.update(&span.canonical_provenance_revision.to_le_bytes());
        revision_hasher.update(&span.security_policy_epoch.to_le_bytes());
        let entity_revision = format!("blake3:{}", revision_hasher.finalize().to_hex());
        let why = format!(
            "matched '{}' via {} (relevance {:.4}, utility 0.5000); selected live-admitted imported evidence {}",
            request.query,
            hit.source.as_str(),
            hit.relevance_score(),
            span.id
        );
        draft.evidence_items.push(PackEvidenceItem {
            rank,
            evidence_id: span.id,
            entity_revision,
            session_id: span.session_id,
            start_line: span.start_line,
            end_line: span.end_line,
            section: PackSection::Evidence,
            content: span.excerpt,
            estimated_tokens,
            relevance,
            utility,
            provenance: vec![provenance],
            why,
            trust: PackTrustSignal::new(
                TrustClass::CassEvidence,
                Some("imported_transcript_excerpt".to_owned()),
            ),
        });
        draft.used_tokens = next_used_tokens;
    }

    if !draft.evidence_items.is_empty() {
        draft.selection_audit.candidate_count = draft
            .selection_audit
            .candidate_count
            .saturating_add(draft.evidence_items.len());
        draft.selection_audit.selected_count =
            draft.items.len().saturating_add(draft.evidence_items.len());
        draft.selection_audit.budget_used = draft.used_tokens;
        draft.hash = None;
    }
    if rejected_live_admission > 0 {
        push_degradation(
            degraded,
            "context_evidence_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!(
                "Excluded {rejected_live_admission} imported-evidence search hit(s) because live pack admission could not be proved."
            ),
            Some("ee index rebuild --json".to_owned()),
        );
    }
}

/// Resolve an imported-evidence search hit to the memory its span was
/// distilled into so the hit can hydrate into the pack (bd-16imy).
///
/// Search-hit metadata is a derived, staleable asset and therefore never
/// authorizes pack hydration. Reload the span, session, and linked memory,
/// verify that the evidence belongs to the requested workspace, and re-run
/// the current positive-admission policy before returning a memory id.
fn evidence_linked_memory_id(
    connection: &DbConnection,
    workspace_path: &Path,
    hit: &crate::core::search::SearchHit,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<(MemoryId, Option<String>)> {
    if !hit.doc_id.starts_with("ev_") {
        return None;
    }
    let evidence_id = match EvidenceId::from_str(&hit.doc_id) {
        Ok(id) => id.to_string(),
        Err(_) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                "A malformed evidence identifier from the derived index was excluded.".to_owned(),
                Some("ee index rebuild --json".to_owned()),
            );
            return None;
        }
    };

    let span = match connection.get_evidence_span(&evidence_id) {
        Ok(Some(span)) => span,
        Ok(None) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!(
                    "Evidence {} matched a stale search index but no live source row exists.",
                    evidence_id
                ),
                Some("ee index rebuild --json".to_owned()),
            );
            return None;
        }
        Err(error) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!("Evidence {evidence_id} could not be revalidated: {error}"),
                Some("ee doctor --json".to_owned()),
            );
            return None;
        }
    };
    let workspace_ids = context_workspace_ids(connection, workspace_path, degraded);
    if !workspace_ids.iter().any(|id| id == &span.workspace_id) {
        push_degradation(
            degraded,
            "context_evidence_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!(
                "Evidence {} was excluded because its live workspace is outside this pack request.",
                evidence_id
            ),
            None,
        );
        return None;
    }
    let session = match connection.get_session(&span.session_id) {
        Ok(Some(session)) => session,
        Ok(None) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!(
                    "Evidence {} was excluded because its live session provenance is missing.",
                    evidence_id
                ),
                Some("ee index rebuild --json".to_owned()),
            );
            return None;
        }
        Err(error) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!(
                    "Evidence {} session provenance could not be revalidated: {error}",
                    evidence_id
                ),
                Some("ee doctor --json".to_owned()),
            );
            return None;
        }
    };
    if !span.is_search_admitted_for_session(&span.workspace_id, &session) {
        push_degradation(
            degraded,
            "context_evidence_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!(
                "Evidence {} was excluded because its live security posture is not admitted.",
                evidence_id
            ),
            Some("ee index rebuild --json".to_owned()),
        );
        return None;
    }
    let Some(linked_memory_id) = span.memory_id.as_deref() else {
        // Fresh imported evidence is hydrated by the typed direct-evidence
        // boundary after memory-only selection. It is not a degradation and
        // must never receive a synthetic MemoryId (bd-16imy).
        return None;
    };
    let memory_id = match MemoryId::from_str(linked_memory_id) {
        Ok(memory_id) => memory_id,
        Err(_) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!("Evidence {evidence_id} links to a malformed memory identifier."),
                None,
            );
            return None;
        }
    };
    let memory = match connection.get_memory(linked_memory_id) {
        Ok(Some(memory)) => memory,
        Ok(None) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!(
                    "Evidence {} links to a memory that is no longer present.",
                    evidence_id
                ),
                None,
            );
            return None;
        }
        Err(error) => {
            push_degradation(
                degraded,
                "context_evidence_hit_unhydrated",
                ContextResponseSeverity::Low,
                format!(
                    "Evidence {} linked memory could not be revalidated: {error}",
                    evidence_id
                ),
                Some("ee doctor --json".to_owned()),
            );
            return None;
        }
    };
    if span.is_pack_admitted(&span.workspace_id, &session, &memory) {
        return Some((memory_id, Some(evidence_id)));
    }

    push_degradation(
        degraded,
        "context_evidence_hit_unhydrated",
        ContextResponseSeverity::Low,
        format!(
            "Evidence {} was excluded because its live pack admission proof is incomplete.",
            evidence_id
        ),
        Some("ee index rebuild --json".to_owned()),
    );
    None
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
    let mut freshness_file_cache = crate::core::memory::EvidenceFreshnessFileCache::default();
    provenance_for_memory_cached(
        memory,
        memory_id,
        workspace_path,
        degraded,
        &mut freshness_file_cache,
    )
}

fn provenance_for_memory_cached(
    memory: &StoredMemory,
    memory_id: MemoryId,
    workspace_path: &Path,
    degraded: &mut Vec<ContextResponseDegradation>,
    freshness_file_cache: &mut crate::core::memory::EvidenceFreshnessFileCache,
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
    let freshness = crate::core::memory::assess_memory_evidence_freshness_with_cache(
        memory,
        Some(workspace_path),
        freshness_file_cache,
    );
    if freshness.status.should_report() {
        push_evidence_freshness_degradation(memory, &freshness, degraded);
    }
    let active_workspace_id = stable_context_workspace_id(workspace_path);
    let note = if memory.workspace_id == active_workspace_id {
        format!(
            "Memory {} selected for context pack; evidenceFreshness={}",
            memory.id,
            freshness.status.as_str()
        )
    } else {
        format!(
            "Memory {} selected by cross_shard_read; origin_workspace_id={}; pack_workspace_id={}; evidenceFreshness={}",
            memory.id,
            memory.workspace_id,
            active_workspace_id,
            freshness.status.as_str()
        )
    };

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
    let detail = redact_pack_provenance_text(&freshness.detail);
    let repair = freshness.repair.as_deref().map(redact_pack_provenance_text);
    push_degradation(
        degraded,
        code,
        ContextResponseSeverity::Low,
        format!(
            "Memory {} evidence freshness is {}: {}",
            memory.id,
            freshness.status.as_str(),
            detail
        ),
        repair,
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

fn pack_candidate_relevance_from_search_hit(hit: &SearchHit) -> Option<UnitScore> {
    unit_score(hit.relevance_score())
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

fn apply_context_pack_contradiction_guard(connection: &DbConnection, draft: &mut PackDraft) {
    if draft.items.len() < 2 {
        return;
    }
    let gathered = crate::core::contradiction_detect::gather_explicit_conflict_edges(connection);
    if let Some(read_error) = gathered.read_error.as_deref() {
        tracing::warn!(
            target: "ee::pack::contradiction_guard",
            error = read_error,
            "skipping context pack contradiction guard because memory links could not be read"
        );
        return;
    }
    let detected = gathered
        .edges
        .iter()
        .map(|edge| (edge.memory_a.clone(), edge.memory_b.clone()))
        .collect::<Vec<_>>();
    let unresolved =
        crate::core::contradiction_guard::unresolved_contradiction_pairs(&detected, &[]);
    let suppressed = draft.apply_contradiction_guard(&unresolved, false);
    if suppressed > 0 {
        tracing::debug!(
            target: "ee::pack::contradiction_guard",
            suppressed,
            "suppressed unresolved contradiction sides from context pack"
        );
    }
}

fn push_pack_budget_too_small_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    candidate_pool: usize,
    item_count: usize,
    used_tokens: u32,
    max_tokens: u32,
    candidate_token_costs_min: Option<u32>,
) {
    if candidate_pool == 0
        || item_count > 0
        || degraded
            .iter()
            .any(|entry| entry.code == "no_relevant_results")
    {
        return;
    }

    tracing::warn!(
        target: "ee::pack::budget_exhausted",
        pool_size = candidate_pool,
        max_tokens,
        candidate_token_costs_min = candidate_token_costs_min.unwrap_or(0),
        "pack budget too small"
    );
    push_degradation(
        degraded,
        crate::pack::PACK_BUDGET_TOO_SMALL_CODE,
        ContextResponseSeverity::Warning,
        format!(
            "Pack budget could not fit any candidate. Items=0, pool={candidate_pool}, used_tokens={used_tokens}/{max_tokens}."
        ),
        None,
    );
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
    type TestResult = Result<(), String>;

    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use proptest::prelude::*;
    use proptest::test_runner::Config as ProptestConfig;

    use asupersync::{CancelReason, Cx};

    use super::{
        AccessLevel, CandidateResolutionMetrics, CapabilitySet, CommandCancellation,
        CommandContext, ContextPagination, ContextPerformanceTrace, PackPersistenceSubspans,
        PackSlotAcquisition, PerformanceTiming, ReadSnapshotTrace, apply_pagination,
        candidate_selection_why, context_performance_json, focus_candidate_why, focus_relevance,
        open_pack_slot_lock_file, pack_assembly_slo_for_run, push_evidence_freshness_degradation,
        push_pack_budget_too_small_degradation, push_search_degradations, try_acquire_pack_slot,
        unit_score,
    };
    use crate::config::{ReadPoolConfig, WorkspaceLocation};
    use crate::core::budget::{BudgetDimension, RequestBudget};
    use crate::core::memory::{ReviseMemoryOptions, ReviseReason, revise_memory};
    use crate::core::memory_drift::{MemoryDriftSelectionHint, MemoryDriftStatus};
    use crate::core::profile::{OperatingProfile, RuntimeProfileReport};
    use crate::core::search::{
        PERFORMANCE_EXPLAIN_SCHEMA_V1, RERANK_MODEL_UNAVAILABLE_ADVISORY, ScoreSource,
        SearchDegradation, SearchHit, SearchReport, SearchStatus,
    };
    use crate::db::read_pool::{
        AcquireWaitStats, PoolConfig, PoolStats, READ_POOL_UNDERSIZED_P99_THRESHOLD,
        READ_POOL_UNDERSIZED_SAMPLE_FLOOR, ReadConnectionPool,
    };
    use crate::db::{
        CreateMemoryInput, CreateWorkspaceInput, DatabaseConfig, DbConnection,
        StoredAgentContextProfileForPack, StoredMemory, UpsertAgentContextProfileInput,
    };
    use crate::models::{
        AgentContextProfileCounts, EmbedBackend, FocusItem, FocusState, LineSpan, MemoryId,
        MemoryScope, MemoryScopeStats, ProvenanceUri, QueryTemporalFilters, QueryTemporalValidity,
        QueryTemporalValidityPosture, TrustClass, UnitScore, WorkspaceId,
    };
    use crate::pack::{
        ContextPackProfile, ContextRequest, ContextRequestInput, ContextResponseDegradation,
        ContextResponseSeverity, PackAssemblyOptions, PackCandidate, PackCandidateInput,
        PackProvenance, PackResourceProfile, PackScoreBreakdown, PackSection, TokenBudget,
        assemble_draft_with_profile, assemble_draft_with_profile_and_options,
    };

    #[test]
    fn orient_fast_snippet_source_counts_ellipsis_inside_the_character_cap() {
        for (length, truncated) in [(479, false), (480, false), (481, true)] {
            let source = "λ".repeat(length);
            let snippet = super::orient_fast_snippet_source(&source);
            assert!(snippet.chars().count() <= 480);
            assert_eq!(snippet.ends_with('…'), truncated);
            if !truncated {
                assert_eq!(snippet, source);
            }
        }
    }

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

    fn ctx_with_budget(budget: RequestBudget) -> CommandContext {
        CommandContext::new(
            workspace_at("/tmp/ee-test-workspace"),
            budget,
            CapabilitySet::read_only(),
        )
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> Result<(), String>
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn test_runtime_profile() -> RuntimeProfileReport {
        RuntimeProfileReport::for_profile(OperatingProfile::Workstation, "test_fixture")
    }

    fn pagination_candidate(seed: u128) -> Result<PackCandidate, String> {
        let provenance = PackProvenance::new(
            ProvenanceUri::from_str("manual://pagination")
                .map_err(|error| format!("provenance uri: {error:?}"))?,
            "pagination fixture",
        )
        .map_err(|error| format!("provenance: {error:?}"))?;
        PackCandidate::new(PackCandidateInput {
            memory_id: MemoryId::from_uuid(uuid::Uuid::from_u128(seed)),
            section: PackSection::ProceduralRules,
            content: format!("Pagination candidate {seed}."),
            estimated_tokens: 4,
            relevance: UnitScore::parse(0.8).map_err(|error| format!("relevance: {error:?}"))?,
            utility: UnitScore::parse(0.7).map_err(|error| format!("utility: {error:?}"))?,
            provenance: vec![provenance],
            why: "pagination helper fixture".to_owned(),
        })
        .map_err(|error| format!("candidate: {error:?}"))
    }

    #[test]
    fn apply_pagination_preserves_next_cursor_metadata_for_response() -> Result<(), String> {
        let mut candidates = vec![
            pagination_candidate(1)?,
            pagination_candidate(2)?,
            pagination_candidate(3)?,
        ];
        let mut degraded = Vec::new();
        let info = apply_pagination(
            &mut candidates,
            &Some(ContextPagination {
                limit: 1,
                offset: 1,
                query_hash: "query-shape".to_owned(),
            }),
            &mut degraded,
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(info.offset, 1);
        assert_eq!(info.limit, 1);
        assert_eq!(info.page_size, 1);
        assert_eq!(info.total, 3);
        assert!(info.has_more);
        let cursor = info
            .next_cursor
            .clone()
            .ok_or_else(|| "expected next cursor".to_owned())?;
        let decoded =
            crate::models::PaginationCursor::decode(&cursor).map_err(|error| error.to_string())?;
        assert_eq!(decoded.offset, 2);
        assert_eq!(decoded.query_hash, "query-shape");
        let response = info.into_response();
        assert_eq!(response.next_cursor.as_deref(), Some(cursor.as_str()));
        assert_eq!(response.total, 3);
        assert_eq!(response.page_size, 1);
        assert!(
            degraded
                .iter()
                .any(|entry| entry.code == "context_pagination_applied"),
            "pagination should remain visible as a degradation"
        );
        Ok(())
    }

    #[test]
    fn check_cancellation_accepts_live_cx_and_unexceeded_budget() -> Result<(), String> {
        let cx = Cx::for_testing();
        ctx(CapabilitySet::read_only())
            .check_cancellation(&cx)
            .map_err(|error| format!("live Cx should pass cancellation check: {error}"))
    }

    #[test]
    fn check_cancellation_preserves_asupersync_cancel_reason() -> Result<(), String> {
        let cx = Cx::for_testing();
        cx.set_cancel_reason(CancelReason::user("context cancellation test"));
        let error = ctx(CapabilitySet::read_only())
            .check_cancellation(&cx)
            .expect_err("cancelled Cx must fail check_cancellation");
        let CommandCancellation::Cancelled(reason) = error else {
            return Err("cancelled Cx must retain a typed cancellation reason".to_owned());
        };
        ensure_equal(
            &reason.kind,
            &asupersync::CancelKind::User,
            "cancelled Cx reason kind",
        )?;
        ensure_equal(
            &reason.message.as_deref(),
            &Some("context cancellation test"),
            "cancelled Cx reason message",
        )
    }

    #[test]
    fn check_cancellation_preserves_budget_error_before_cx_error() -> Result<(), String> {
        let cx = Cx::for_testing();
        cx.set_cancel_reason(CancelReason::user("context cancellation test"));
        let mut budget = RequestBudget::unbounded().with_tokens(0);
        budget.record_tokens(1);
        let error = ctx_with_budget(budget)
            .check_cancellation(&cx)
            .expect_err("exceeded budget must fail check_cancellation");
        let CommandCancellation::BudgetExceeded(error) = error else {
            return Err("request budget breach must win an already-cancelled Cx".to_owned());
        };
        ensure_equal(
            &error.dimension,
            &BudgetDimension::Tokens,
            "budget-first cancellation dimension",
        )?;
        ensure_equal(&error.limit, &0, "budget-first limit")?;
        ensure_equal(&error.used, &1, "budget-first used")
    }

    #[test]
    fn evidence_freshness_degradation_redacts_provenance_detail_and_repair() -> Result<(), String> {
        let memory = tier_memory(
            MemoryId::from_uuid(uuid::Uuid::from_u128(7210)),
            0.9,
            0.8,
            0.7,
            "rule",
        );
        let secret = "AbCDefGhIjKlMnOpQrStUvWxYz0123456789abCDefGhIj";
        let freshness = crate::core::memory::EvidenceFreshness {
            status: crate::core::memory::EvidenceFreshnessStatus::MissingSource,
            provenance_uri: Some(
                "file:/Users/jemanuel/projects/eidetic_engine_cli/CLOSE_THE_GAP_PLAN.md#L1186"
                    .to_string(),
            ),
            detail: format!(
                "Referenced provenance file /Users/jemanuel/projects/eidetic_engine_cli/CLOSE_THE_GAP_PLAN.md is missing; token={secret}."
            ),
            repair: Some(format!(
                "Restore /Users/jemanuel/projects/eidetic_engine_cli/CLOSE_THE_GAP_PLAN.md with token={secret}."
            )),
        };
        let mut degraded = Vec::new();

        push_evidence_freshness_degradation(&memory, &freshness, &mut degraded);

        ensure_equal(&degraded.len(), &1, "freshness degradation count")?;
        let entry = &degraded[0];
        ensure_equal(
            &entry.code,
            &"context_evidence_freshness_missing_source".to_string(),
            "freshness degradation code",
        )?;
        ensure_equal(
            &entry.message.contains("[REDACTED_PATH]"),
            &true,
            "message path placeholder",
        )?;
        ensure_equal(
            &entry.message.contains("[REDACTED:token]"),
            &true,
            "message token placeholder",
        )?;
        ensure_equal(
            &entry.message.contains("/Users/jemanuel"),
            &false,
            "message raw path leak",
        )?;
        ensure_equal(
            &entry.message.contains(secret),
            &false,
            "message raw token leak",
        )?;
        let repair = entry
            .repair
            .as_deref()
            .ok_or("expected redacted repair command")?;
        ensure_equal(
            &repair.contains("[REDACTED_PATH]"),
            &true,
            "repair path placeholder",
        )?;
        ensure_equal(
            &repair.contains("[REDACTED:token]"),
            &true,
            "repair token placeholder",
        )?;
        ensure_equal(
            &repair.contains("/Users/jemanuel"),
            &false,
            "repair raw path leak",
        )
    }

    #[test]
    fn pack_budget_too_small_degradation_emits_for_empty_selection_with_candidates()
    -> Result<(), String> {
        let mut degraded = Vec::new();

        push_pack_budget_too_small_degradation(&mut degraded, 3, 0, 0, 2, Some(8));

        ensure_equal(&degraded.len(), &1, "emitted degradation count")?;
        let entry = &degraded[0];
        ensure_equal(
            &entry.code,
            &"pack_budget_too_small".to_string(),
            "degraded code",
        )?;
        ensure_equal(
            &entry.severity,
            &ContextResponseSeverity::Warning,
            "degraded severity",
        )?;
        ensure_equal(
            &entry.message,
            &"Pack budget could not fit any candidate. Items=0, pool=3, used_tokens=0/2."
                .to_string(),
            "degraded message",
        )
    }

    #[test]
    fn pack_budget_too_small_degradation_skips_empty_pool_selected_items_and_no_results()
    -> Result<(), String> {
        let mut degraded = Vec::new();

        push_pack_budget_too_small_degradation(&mut degraded, 0, 0, 0, 2, None);
        ensure_equal(&degraded.len(), &0, "empty pool emits nothing")?;

        push_pack_budget_too_small_degradation(&mut degraded, 3, 1, 1, 2, Some(1));
        ensure_equal(&degraded.len(), &0, "selected item emits nothing")?;

        degraded.push(
            ContextResponseDegradation::new(
                "no_relevant_results",
                ContextResponseSeverity::Medium,
                "No relevant results.",
                None,
            )
            .map_err(|error| format!("failed to build no-results degradation: {error:?}"))?,
        );
        push_pack_budget_too_small_degradation(&mut degraded, 3, 0, 0, 2, Some(8));
        ensure_equal(
            &degraded.len(),
            &1,
            "no_relevant_results suppresses budget degradation",
        )
    }

    #[test]
    fn imported_degradation_severities_preserve_critical() -> TestResult {
        ensure_equal(
            &super::context_severity_from_pack_dna("critical"),
            &ContextResponseSeverity::Critical,
            "pack DNA critical severity",
        )?;

        let mut degraded = Vec::new();
        push_search_degradations(
            &mut degraded,
            &[SearchDegradation {
                code: "mesh_cursor_repair_required".to_owned(),
                severity: "critical".to_owned(),
                message: "Mesh cursor repair is required before continuing.".to_owned(),
                repair: Some("ee mesh repair-cursor --json".to_owned()),
            }],
        );

        ensure_equal(&degraded.len(), &1, "search degradation count")?;
        ensure_equal(
            &degraded[0].severity,
            &ContextResponseSeverity::Critical,
            "search critical severity",
        )?;

        let mut hint = MemoryDriftSelectionHint::new(
            "mem_critical",
            MemoryDriftStatus::MissingSource,
            "source_missing",
            1,
        );
        hint.severity = "critical".to_owned();
        ensure_equal(
            &super::context_severity_for_memory_drift_hint(&hint),
            &ContextResponseSeverity::Critical,
            "memory drift critical severity",
        )
    }

    #[test]
    fn permanent_search_capability_posture_does_not_repeat_in_pack_degraded() -> TestResult {
        let mut degraded = Vec::new();
        push_search_degradations(
            &mut degraded,
            &[
                SearchDegradation {
                    code: "rerank_model_unavailable".to_owned(),
                    severity: "low".to_owned(),
                    message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_owned(),
                    repair: None,
                },
                SearchDegradation {
                    code: "search_index_stale".to_owned(),
                    severity: "medium".to_owned(),
                    message: "Search index is stale.".to_owned(),
                    repair: Some("ee index rebuild --workspace .".to_owned()),
                },
            ],
        );

        ensure_equal(&degraded.len(), &1, "only transient degradation imported")?;
        ensure_equal(
            &degraded[0].code,
            &"search_index_stale".to_owned(),
            "transient code remains visible",
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn pack_budget_too_small_and_no_relevant_results_never_both_emit(
            candidate_pool in 0_usize..64,
            item_count in 0_usize..64,
            used_tokens in any::<u16>(),
            max_tokens in 0_u16..8192,
            no_relevant_results_present in proptest::bool::ANY,
        ) {
            let mut degraded = Vec::new();

            if no_relevant_results_present {
                degraded.push(
                    ContextResponseDegradation::new(
                        "no_relevant_results",
                        ContextResponseSeverity::Medium,
                        "No relevant results.",
                        None,
                    )
                    .expect("fixture degradation is valid"),
                );
            }

            push_pack_budget_too_small_degradation(
                &mut degraded,
                candidate_pool,
                item_count,
                u32::from(used_tokens),
                u32::from(max_tokens),
                None,
            );

            let has_pack_budget_too_small = degraded
                .iter()
                .any(|entry| entry.code == crate::pack::PACK_BUDGET_TOO_SMALL_CODE);
            let has_no_relevant_results = degraded
                .iter()
                .any(|entry| entry.code == "no_relevant_results");

            prop_assert!(
                !(has_pack_budget_too_small && has_no_relevant_results),
                "pack_budget_too_small and no_relevant_results must be mutually exclusive"
            );
            prop_assert_eq!(
                has_pack_budget_too_small,
                candidate_pool > 0 && item_count == 0 && !no_relevant_results_present
            );
        }
    }

    #[test]
    fn pack_slot_guard_enforces_lean_profile_limit() -> Result<(), String> {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;

        let first = match try_acquire_pack_slot(workspace.path(), PackResourceProfile::Lean) {
            PackSlotAcquisition::Acquired {
                guard,
                queue_depth,
                concurrent_pack_max,
            } => {
                assert_eq!(queue_depth, 0);
                assert_eq!(concurrent_pack_max, 1);
                guard
            }
            other => {
                return Err(format!(
                    "first lean pack slot should be acquired: {other:?}"
                ));
            }
        };

        match try_acquire_pack_slot(workspace.path(), PackResourceProfile::Lean) {
            PackSlotAcquisition::LimitReached {
                retry_after_ms,
                queue_depth,
                concurrent_pack_max,
            } => {
                assert_eq!(retry_after_ms, super::PACK_SLOT_RETRY_AFTER_MS);
                assert_eq!(queue_depth, 1);
                assert_eq!(concurrent_pack_max, 1);
            }
            other => {
                return Err(format!(
                    "second lean pack slot should be limited: {other:?}"
                ));
            }
        }

        drop(first);

        match try_acquire_pack_slot(workspace.path(), PackResourceProfile::Lean) {
            PackSlotAcquisition::Acquired {
                guard: _guard,
                queue_depth,
                concurrent_pack_max,
            } => {
                assert_eq!(queue_depth, 0);
                assert_eq!(concurrent_pack_max, 1);
                Ok(())
            }
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

    #[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
    #[test]
    fn open_pack_slot_lock_file_rejects_symlinked_final_path() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_lock = tempdir.path().join("outside.lock");
        std::fs::write(&outside_lock, "outside").map_err(|error| error.to_string())?;
        let slot_path = tempdir.path().join("pack-slot.lock");
        std::os::unix::fs::symlink(&outside_lock, &slot_path).map_err(|error| error.to_string())?;

        match open_pack_slot_lock_file(&slot_path) {
            Ok(_) => Err("symlinked pack slot lock final open unexpectedly succeeded".to_owned()),
            Err(error) => {
                let outside =
                    std::fs::read_to_string(&outside_lock).map_err(|error| error.to_string())?;
                assert_eq!(
                    outside, "outside",
                    "pack slot lock final open must not mutate the symlink target"
                );
                assert!(
                    error.raw_os_error().is_some() || error.kind() == std::io::ErrorKind::Other,
                    "expected OS no-follow error for final open, got: {error}"
                );
                Ok(())
            }
        }
    }

    #[test]
    fn pack_slot_guard_rejects_non_regular_lock_file() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let slots_dir = workspace.join(".ee").join("pack-slots");
        std::fs::create_dir_all(&slots_dir).map_err(|error| error.to_string())?;
        let slot_path = slots_dir.join(format!("{}-00.lock", PackResourceProfile::Lean.as_str()));
        std::fs::create_dir(&slot_path).map_err(|error| error.to_string())?;

        match try_acquire_pack_slot(&workspace, PackResourceProfile::Lean) {
            PackSlotAcquisition::Unavailable { message, .. } => {
                assert!(
                    message.contains("not a regular file"),
                    "expected non-regular lock rejection, got: {message}"
                );
                assert!(
                    slot_path.is_dir(),
                    "pack slot lock open must leave the non-regular path untouched"
                );
                Ok(())
            }
            other => Err(format!(
                "non-regular pack slot lock should be unavailable: {other:?}"
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
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: Some(path),
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
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

    #[cfg(unix)]
    #[test]
    fn context_file_final_read_open_rejects_symlinked_path() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_file = tempdir.path().join("outside-context-file.toml");
        std::fs::write(&outside_file, "[graph.feature]\nppr_enabled = true\n")
            .map_err(|error| error.to_string())?;
        let linked_file = tempdir.path().join("context-file.toml");
        std::os::unix::fs::symlink(&outside_file, &linked_file)
            .map_err(|error| error.to_string())?;

        let error = super::open_context_file_for_read_no_follow(&linked_file)
            .expect_err("final context file read open must reject symlinks");

        assert_ne!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "final symlink read should fail because the path is a symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&outside_file).map_err(|error| error.to_string())?,
            "[graph.feature]\nppr_enabled = true\n",
            "context file read helper must not follow the symlink target"
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

    fn symbol_candidate(
        memory_id: MemoryId,
        relevance: f32,
        workspace_path: &Path,
        relative_path: &str,
        line: u64,
    ) -> Result<PackCandidate, String> {
        let provenance = PackProvenance::new(
            ProvenanceUri::File {
                path: workspace_path.join(relative_path).display().to_string(),
                span: Some(LineSpan::single(line).map_err(|error| error.to_string())?),
            },
            "context changed-symbol fixture",
        )
        .map_err(|error| error.to_string())?;
        PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::Failures,
            content: format!("candidate {memory_id}"),
            estimated_tokens: 8,
            relevance: UnitScore::parse(relevance).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            provenance: vec![provenance],
            why: "selected by fixture".to_string(),
        })
        .map_err(|error| error.to_string())
    }

    fn tier_candidate(
        memory_id: MemoryId,
        relevance: f32,
        why: &str,
    ) -> Result<PackCandidate, String> {
        let provenance =
            PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "tier admission fixture")
                .map_err(|error| error.to_string())?;
        PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: format!("tier candidate {memory_id}"),
            estimated_tokens: 8,
            relevance: UnitScore::parse(relevance).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            provenance: vec![provenance],
            why: why.to_string(),
        })
        .map_err(|error| error.to_string())
    }

    fn global_policy_candidate(
        seed: u128,
        content: &str,
        estimated_tokens: u32,
    ) -> Result<PackCandidate, String> {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(seed));
        let provenance =
            PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "global policy fixture")
                .map_err(|error| error.to_string())?;
        PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: content.to_string(),
            estimated_tokens,
            relevance: UnitScore::parse(0.8).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.7).map_err(|error| error.to_string())?,
            provenance: vec![provenance],
            why: "selected by fixture".to_string(),
        })
        .map_err(|error| error.to_string())
    }

    fn tier_memory(
        memory_id: MemoryId,
        confidence: f32,
        utility: f32,
        importance: f32,
        kind: &str,
    ) -> StoredMemory {
        StoredMemory {
            id: memory_id.to_string(),
            workspace_id: WorkspaceId::from_uuid(uuid::Uuid::from_u128(930)).to_string(),
            level: "procedural".to_owned(),
            kind: kind.to_owned(),
            content: format!("tier memory {memory_id}"),
            workflow_id: None,
            confidence,
            utility,
            importance,
            provenance_uri: None,
            trust_class: TrustClass::AgentValidated.as_str().to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "1".to_owned(),
            provenance_verification_status: "pending".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-22T00:00:00Z".to_owned(),
            updated_at: "2026-05-22T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    fn tier_memory_map(memories: Vec<StoredMemory>) -> BTreeMap<String, StoredMemory> {
        memories
            .into_iter()
            .map(|memory| (memory.id.clone(), memory))
            .collect()
    }

    #[test]
    fn seal_sidecar_admission_is_lazy_truthful_and_fail_closed() -> TestResult {
        let unavailable_connection =
            DbConnection::open_memory().map_err(|error| error.to_string())?;
        let ordinary_id = MemoryId::from_uuid(uuid::Uuid::from_u128(931));
        let ordinary = tier_memory(ordinary_id, 0.9, 0.8, 0.7, "fact");
        let mut degraded = Vec::new();
        let ordinary_admission = super::context_memory_seal_admission(
            &unavailable_connection,
            &ordinary,
            &mut degraded,
            "context_candidate_memory_batch_unavailable",
            ContextResponseSeverity::Medium,
            "Test candidate admission",
        );
        assert_eq!(
            ordinary_admission,
            super::ContextMemorySealAdmission::Admit,
            "non-placeholder content must not query the unavailable sidecar"
        );
        assert!(degraded.is_empty());

        let mut unresolved_placeholder = ordinary.clone();
        unresolved_placeholder.content = crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT.to_owned();
        let unresolved_admission = super::context_memory_seal_admission(
            &unavailable_connection,
            &unresolved_placeholder,
            &mut degraded,
            "context_candidate_memory_batch_unavailable",
            ContextResponseSeverity::Medium,
            "Test candidate admission",
        );
        assert_eq!(
            unresolved_admission,
            super::ContextMemorySealAdmission::LookupUnavailable,
            "sidecar lookup failure must not admit placeholder-shaped content"
        );
        assert!(degraded.iter().any(|entry| {
            entry.code == "context_candidate_memory_batch_unavailable"
                && entry.message.contains("excluded fail closed")
        }));

        let available_connection =
            DbConnection::open_memory().map_err(|error| error.to_string())?;
        available_connection
            .migrate()
            .map_err(|error| error.to_string())?;
        let mut available_degraded = Vec::new();
        let unsealed_admission = super::context_memory_seal_admission(
            &available_connection,
            &unresolved_placeholder,
            &mut available_degraded,
            "context_candidate_memory_batch_unavailable",
            ContextResponseSeverity::Medium,
            "Test candidate admission",
        );
        assert_eq!(
            unsealed_admission,
            super::ContextMemorySealAdmission::Admit,
            "exact placeholder content without a seal sidecar is ordinary content"
        );
        assert!(available_degraded.is_empty());
        Ok(())
    }

    #[test]
    fn hybrid_search_hit_relevance_is_normalized_for_pack_candidates() -> Result<(), String> {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(939));
        let memory = tier_memory(memory_id, 0.9, 0.8, 0.7, "rule");
        let memory_key = memory.id.clone();
        let memory_batch = super::CandidateMemoryBatch::Owned(tier_memory_map(vec![memory]));
        let tags_map = BTreeMap::new();
        let mut freshness_file_cache = crate::core::memory::EvidenceFreshnessFileCache::default();
        let source = super::PreloadedCandidateSource {
            memories: &memory_batch,
            tags_map: &tags_map,
            workspace_path: Path::new("/tmp/ee-hybrid-pack-relevance-test"),
            query: "hybrid recall",
            validity_reference_time: None,
            include_tombstoned: false,
            freshness_file_cache: &mut freshness_file_cache,
        };
        let hit = SearchHit {
            doc_id: memory_key.clone(),
            score: crate::core::search::RRF_HYBRID_TYPICAL_MAX,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.91),
            quality_score: None,
            lexical_score: Some(0.83),
            rerank_score: None,
            metadata: None,
            explanation: None,
        };
        let mut degraded = Vec::new();
        let mut subspans = super::CandidateResolutionSubspans::default();

        let candidate = super::candidate_from_hit_preloaded(
            source,
            &hit,
            &memory_key,
            memory_id,
            None,
            &mut degraded,
            &mut subspans,
        )
        .ok_or_else(|| "hybrid hit should convert into a pack candidate".to_string())?;

        assert!(
            (candidate.relevance.into_inner() - 1.0).abs() < 1e-6,
            "top hybrid RRF hit must be normalized to pack relevance 1.0, got {}",
            candidate.relevance.into_inner()
        );
        assert!(
            candidate.why.contains("relevance 1.0000"),
            "why text must report normalized relevance, got: {}",
            candidate.why
        );
        Ok(())
    }

    #[test]
    fn memory_tier_admission_boosts_hot_and_warm_candidates() -> Result<(), String> {
        let hot_id = MemoryId::from_uuid(uuid::Uuid::from_u128(931));
        let warm_id = MemoryId::from_uuid(uuid::Uuid::from_u128(932));
        let mut candidates = vec![
            tier_candidate(hot_id, 0.50, "selected by fixture")?,
            tier_candidate(warm_id, 0.51, "selected by fixture")?,
        ];
        let memories = tier_memory_map(vec![
            tier_memory(hot_id, 1.0, 1.0, 1.0, "rule"),
            tier_memory(warm_id, 0.5, 0.5, 0.5, "rule"),
        ]);

        let metrics = super::apply_memory_tier_candidate_admission_from_memories(
            &mut candidates,
            &memories,
            crate::cache::hotset::MemoryTierPolicyConfig::new(1, 1, 700),
        );
        super::sort_context_candidates(&mut candidates);

        assert_eq!(metrics.boosted_candidates, 2);
        assert_eq!(metrics.cold_candidates, 0);
        assert_eq!(candidates[0].memory_id, hot_id);
        assert!(candidates[0].why.contains("tierAdmission tier=hot"));
        assert!(candidates[1].why.contains("tierAdmission tier=warm"));
        Ok(())
    }

    #[test]
    fn memory_tier_admission_preserves_required_cold_evidence() -> Result<(), String> {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(933));
        let mut candidates = vec![tier_candidate(
            memory_id,
            0.73,
            "matched 'release failure' via lexical (relevance 0.7300, utility 0.8000)",
        )?];
        let memories = tier_memory_map(vec![tier_memory(memory_id, 0.2, 0.2, 0.2, "failure")]);

        let metrics = super::apply_memory_tier_candidate_admission_from_memories(
            &mut candidates,
            &memories,
            crate::cache::hotset::MemoryTierPolicyConfig::new(0, 0, 1000),
        );

        assert_eq!(metrics.boosted_candidates, 0);
        assert_eq!(metrics.cold_candidates, 1);
        assert_eq!(metrics.required_cold_candidates, 1);
        assert!((candidates[0].relevance.into_inner() - 0.73).abs() < 0.0001);
        assert!(candidates[0].why.contains("tierAdmission tier=cold"));
        assert!(candidates[0].why.contains("requiredEvidencePreserved=true"));
        assert!(candidates[0].why.contains("noFilter=true"));
        Ok(())
    }

    #[test]
    fn memory_tier_admission_does_not_need_full_pool_to_mark_required_cold() -> Result<(), String> {
        let hot_id = MemoryId::from_uuid(uuid::Uuid::from_u128(936));
        let warm_id = MemoryId::from_uuid(uuid::Uuid::from_u128(937));
        let required_id = MemoryId::from_uuid(uuid::Uuid::from_u128(938));
        let mut candidates = vec![
            tier_candidate(hot_id, 0.91, "matched 'release failure' via lexical")?,
            tier_candidate(warm_id, 0.89, "selected by fixture")?,
            tier_candidate(required_id, 0.87, "matched 'release failure' via lexical")?,
        ];
        let memories = tier_memory_map(vec![
            tier_memory(hot_id, 0.95, 0.95, 0.95, "rule"),
            tier_memory(warm_id, 0.60, 0.60, 0.60, "rule"),
            tier_memory(required_id, 0.05, 0.05, 0.05, "failure"),
        ]);

        let metrics = super::apply_memory_tier_candidate_admission_from_memories(
            &mut candidates,
            &memories,
            crate::cache::hotset::MemoryTierPolicyConfig::new(1, 8, 700),
        );

        assert_eq!(metrics.cold_candidates, 1);
        assert_eq!(metrics.required_cold_candidates, 1);
        let required = candidates
            .iter()
            .find(|candidate| candidate.memory_id == required_id)
            .expect("required cold candidate");
        assert!(required.why.contains("tierAdmission tier=cold"));
        assert!(required.why.contains("requiredEvidencePreserved=true"));
        Ok(())
    }

    #[test]
    fn memory_tier_admission_is_deterministic_for_tied_inputs() -> Result<(), String> {
        let lower_id = MemoryId::from_uuid(uuid::Uuid::from_u128(934));
        let higher_id = MemoryId::from_uuid(uuid::Uuid::from_u128(935));
        let memories = tier_memory_map(vec![
            tier_memory(higher_id, 0.8, 0.8, 0.8, "rule"),
            tier_memory(lower_id, 0.8, 0.8, 0.8, "rule"),
        ]);
        let policy = crate::cache::hotset::MemoryTierPolicyConfig::new(1, 1, 700);
        let mut left = vec![
            tier_candidate(lower_id, 0.60, "selected by fixture")?,
            tier_candidate(higher_id, 0.60, "selected by fixture")?,
        ];
        let mut right = vec![
            tier_candidate(higher_id, 0.60, "selected by fixture")?,
            tier_candidate(lower_id, 0.60, "selected by fixture")?,
        ];

        super::apply_memory_tier_candidate_admission_from_memories(&mut left, &memories, policy);
        super::apply_memory_tier_candidate_admission_from_memories(&mut right, &memories, policy);
        super::sort_context_candidates(&mut left);
        super::sort_context_candidates(&mut right);
        let left_summary = left
            .iter()
            .map(|candidate| {
                (
                    candidate.memory_id,
                    candidate.relevance.into_inner(),
                    candidate.why.clone(),
                )
            })
            .collect::<Vec<_>>();
        let right_summary = right
            .iter()
            .map(|candidate| {
                (
                    candidate.memory_id,
                    candidate.relevance.into_inner(),
                    candidate.why.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(left_summary, right_summary);
        assert_eq!(left[0].memory_id, lower_id);
        assert!(left[0].why.contains("tierAdmission tier=hot"));
        Ok(())
    }

    #[test]
    fn global_store_pack_policy_bounds_non_conflict_global_candidates() -> Result<(), String> {
        let workspace = global_policy_candidate(100, "workspace release checklist", 10)?;
        let global_a = global_policy_candidate(101, "global cargo format convention", 100)?;
        let global_b = global_policy_candidate(102, "global rustfmt setup convention", 100)?;
        let global_c = global_policy_candidate(103, "global docs update convention", 40)?;
        let global_b_id = global_b.memory_id.to_string();
        let mut candidates = vec![workspace, global_a, global_b, global_c];
        let global_store_memory_ids = candidates
            .iter()
            .skip(1)
            .map(|candidate| candidate.memory_id.to_string())
            .collect::<BTreeSet<_>>();
        let mut degraded = Vec::new();

        let removed = super::apply_global_store_pack_policy(
            &mut candidates,
            &global_store_memory_ids,
            1_000,
            &mut degraded,
        );

        assert_eq!(removed, 1);
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.memory_id.to_string() == global_b_id),
            "second 100-token global should overflow the 150-token quota"
        );
        assert!(
            degraded
                .iter()
                .any(|entry| entry.code == "global_lane_fan_in_limited")
        );
        Ok(())
    }

    #[test]
    fn global_store_pack_policy_keeps_and_marks_conflicting_global_candidate() -> Result<(), String>
    {
        let workspace = global_policy_candidate(110, "Never rebase in shared checkouts.", 10)?;
        let global = global_policy_candidate(111, "Always rebase before pushing this repo.", 500)?;
        let global_id = global.memory_id.to_string();
        let mut candidates = vec![workspace, global];
        let global_store_memory_ids = [global_id.clone()].into_iter().collect::<BTreeSet<_>>();
        let mut degraded = Vec::new();

        let removed = super::apply_global_store_pack_policy(
            &mut candidates,
            &global_store_memory_ids,
            100,
            &mut degraded,
        );

        assert_eq!(
            removed, 0,
            "conflicting global rows are protected from fan-in trimming"
        );
        let global_candidate = candidates
            .iter()
            .find(|candidate| candidate.memory_id.to_string() == global_id)
            .ok_or_else(|| "conflicting global candidate should remain visible".to_string())?;
        assert!(
            global_candidate.why.contains("globalLane=")
                && global_candidate.why.contains("kind=contradiction"),
            "conflicting global candidate should carry a marker: {}",
            global_candidate.why
        );
        assert!(
            degraded
                .iter()
                .any(|entry| entry.code == "global_lane_conflict_deferred")
        );
        Ok(())
    }

    #[test]
    fn changed_symbol_context_boost_marks_reason_and_changes_rank() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let src_dir = tempdir.path().join("src");
        std::fs::create_dir_all(&src_dir).map_err(|error| error.to_string())?;
        let padding = "\n".repeat(25);
        std::fs::write(
            src_dir.join("lib.rs"),
            format!(
                "pub fn changed_symbol() -> u64 {{ 1 }}\n{padding}pub fn other_symbol() -> u64 {{ 2 }}\n"
            ),
        )
        .map_err(|error| error.to_string())?;

        let changed = MemoryId::from_uuid(uuid::Uuid::from_u128(1201));
        let other = MemoryId::from_uuid(uuid::Uuid::from_u128(1202));
        let mut candidates = vec![
            symbol_candidate(changed, 0.50, tempdir.path(), "src/lib.rs", 1)?,
            symbol_candidate(other, 0.53, tempdir.path(), "src/lib.rs", 27)?,
        ];
        let mut degraded = Vec::new();

        let metrics = super::apply_changed_symbol_context_boost(
            tempdir.path(),
            &["changed_symbol".to_owned()],
            false,
            &mut candidates,
            &mut degraded,
        );
        super::sort_context_candidates(&mut candidates);

        assert_eq!(metrics.boosted_candidates, 1);
        assert_eq!(candidates[0].memory_id, changed);
        assert!(
            candidates[0].why.contains("symbolBoost changedSymbol="),
            "boost should annotate candidate why: {}",
            candidates[0].why
        );
        assert!(
            degraded.is_empty(),
            "fresh symbol extraction should not degrade: {degraded:?}"
        );
        Ok(())
    }

    #[test]
    fn changed_symbol_context_boost_ties_sort_by_memory_id() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let src_dir = tempdir.path().join("src");
        std::fs::create_dir_all(&src_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            src_dir.join("lib.rs"),
            "pub fn changed_symbol() -> u64 { 1 }\n",
        )
        .map_err(|error| error.to_string())?;

        let high_id = MemoryId::from_uuid(uuid::Uuid::from_u128(1302));
        let low_id = MemoryId::from_uuid(uuid::Uuid::from_u128(1301));
        let mut candidates = vec![
            symbol_candidate(high_id, 0.50, tempdir.path(), "src/lib.rs", 1)?,
            symbol_candidate(low_id, 0.50, tempdir.path(), "src/lib.rs", 1)?,
        ];
        let mut degraded = Vec::new();

        let metrics = super::apply_changed_symbol_context_boost(
            tempdir.path(),
            &["changed_symbol".to_owned()],
            false,
            &mut candidates,
            &mut degraded,
        );
        super::sort_context_candidates(&mut candidates);

        assert_eq!(metrics.boosted_candidates, 2);
        assert_eq!(candidates[0].relevance, candidates[1].relevance);
        assert_eq!(candidates[0].memory_id, low_id);
        assert!(
            degraded.is_empty(),
            "fresh symbol extraction should not degrade: {degraded:?}"
        );
        Ok(())
    }

    #[test]
    fn changed_symbol_context_boost_includes_adjacent_symbol_evidence() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let src_dir = tempdir.path().join("src");
        std::fs::create_dir_all(&src_dir).map_err(|error| error.to_string())?;
        let far_padding = "\n".repeat(25);
        std::fs::write(
            src_dir.join("lib.rs"),
            format!(
                "\
pub fn changed_symbol() -> u64 {{
    1
}}

pub fn adjacent_symbol() -> u64 {{
    2
}}
{far_padding}
pub fn far_symbol() -> u64 {{
    3
}}
"
            ),
        )
        .map_err(|error| error.to_string())?;

        let changed = MemoryId::from_uuid(uuid::Uuid::from_u128(1401));
        let adjacent = MemoryId::from_uuid(uuid::Uuid::from_u128(1402));
        let far = MemoryId::from_uuid(uuid::Uuid::from_u128(1403));
        let mut candidates = vec![
            symbol_candidate(changed, 0.40, tempdir.path(), "src/lib.rs", 1)?,
            symbol_candidate(adjacent, 0.47, tempdir.path(), "src/lib.rs", 5)?,
            symbol_candidate(far, 0.60, tempdir.path(), "src/lib.rs", 34)?,
        ];
        let mut degraded = Vec::new();

        let metrics = super::apply_changed_symbol_context_boost(
            tempdir.path(),
            &["changed_symbol".to_owned()],
            false,
            &mut candidates,
            &mut degraded,
        );
        super::sort_context_candidates(&mut candidates);

        assert_eq!(metrics.boosted_candidates, 2);
        assert_eq!(candidates[0].memory_id, far);
        let adjacent_candidate = candidates
            .iter()
            .find(|candidate| candidate.memory_id == adjacent)
            .ok_or("adjacent candidate should be present")?;
        assert!(
            adjacent_candidate
                .why
                .contains("adjacent_to=changed_symbol"),
            "adjacent symbol boost should explain the anchor: {}",
            adjacent_candidate.why
        );
        assert!(
            degraded.is_empty(),
            "fresh adjacent-symbol extraction should not degrade: {degraded:?}"
        );
        Ok(())
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

    #[test]
    fn changed_symbol_boost_promotes_linked_memory_and_explains_reason() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_dir = tempdir.path().join("src");
        std::fs::create_dir_all(&source_dir).map_err(|error| error.to_string())?;
        let relative_path = "src/symbol_context_boost.rs";
        let padding = "\n".repeat(25);
        std::fs::write(
            tempdir.path().join(relative_path),
            format!(
                "\
pub fn render_context_boost() -> u64 {{
    42
}}
{padding}
pub fn unrelated_context() -> u64 {{
    7
}}
"
            ),
        )
        .map_err(|error| error.to_string())?;

        let boosted_id = MemoryId::from_uuid(uuid::Uuid::from_u128(924));
        let neutral_id = MemoryId::from_uuid(uuid::Uuid::from_u128(925));
        let boosted_provenance = PackProvenance::new(
            ProvenanceUri::File {
                path: relative_path.to_string(),
                span: Some(
                    crate::models::LineSpan::range(1, 3).map_err(|error| error.to_string())?,
                ),
            },
            "changed symbol fixture",
        )
        .map_err(|error| error.to_string())?;
        let neutral_provenance = PackProvenance::new(
            ProvenanceUri::File {
                path: relative_path.to_string(),
                span: Some(
                    crate::models::LineSpan::range(29, 31).map_err(|error| error.to_string())?,
                ),
            },
            "neutral symbol fixture",
        )
        .map_err(|error| error.to_string())?;
        let mut candidates = vec![
            PackCandidate::new(PackCandidateInput {
                memory_id: boosted_id,
                section: PackSection::Failures,
                content: "Failure evidence for render_context_boost".to_string(),
                estimated_tokens: 8,
                relevance: UnitScore::parse(0.46).map_err(|error| error.to_string())?,
                utility: UnitScore::parse(0.80).map_err(|error| error.to_string())?,
                provenance: vec![boosted_provenance],
                why: "selected by fixture".to_string(),
            })
            .map_err(|error| error.to_string())?,
            PackCandidate::new(PackCandidateInput {
                memory_id: neutral_id,
                section: PackSection::Failures,
                content: "Unrelated evidence".to_string(),
                estimated_tokens: 8,
                relevance: UnitScore::parse(0.49).map_err(|error| error.to_string())?,
                utility: UnitScore::parse(0.80).map_err(|error| error.to_string())?,
                provenance: vec![neutral_provenance],
                why: "selected by fixture".to_string(),
            })
            .map_err(|error| error.to_string())?,
        ];
        let mut degraded = Vec::new();

        let metrics = super::apply_changed_symbol_context_boost(
            tempdir.path(),
            &["render_context_boost".to_string()],
            false,
            &mut candidates,
            &mut degraded,
        );

        assert_eq!(metrics.boosted_candidates, 1);
        assert!(degraded.is_empty(), "{degraded:?}");
        super::sort_context_candidates(&mut candidates);
        assert_eq!(candidates[0].memory_id, boosted_id);
        assert!(candidates[0].why.contains("symbolBoost"));
        assert!(candidates[0].why.contains("render_context_boost"));
        Ok(())
    }

    #[test]
    fn changed_symbol_boost_reports_stale_index_without_file_provenance() -> Result<(), String> {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(926));
        let provenance = PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "memory-only")
            .map_err(|error| error.to_string())?;
        let mut candidates = vec![
            PackCandidate::new(PackCandidateInput {
                memory_id,
                section: PackSection::Evidence,
                content: "memory-only evidence".to_string(),
                estimated_tokens: 5,
                relevance: UnitScore::parse(0.60).map_err(|error| error.to_string())?,
                utility: UnitScore::parse(0.70).map_err(|error| error.to_string())?,
                provenance: vec![provenance],
                why: "selected by fixture".to_string(),
            })
            .map_err(|error| error.to_string())?,
        ];
        let mut degraded = Vec::new();

        let metrics = super::apply_changed_symbol_context_boost(
            Path::new("/tmp/ee-context-symbol-missing"),
            &["render_context_boost".to_string()],
            false,
            &mut candidates,
            &mut degraded,
        );

        assert_eq!(metrics.boosted_candidates, 0);
        assert!(degraded.iter().any(|entry| {
            entry.code == crate::models::symbol::SYMBOL_INDEX_STALE_CODE
                && entry.repair.as_deref() == Some("ee symbol snapshot --workspace . --refresh")
        }));
        Ok(())
    }

    fn ppr_search_report(hits: Vec<SearchHit>) -> SearchReport {
        SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "release graph".to_string(),
            requested_limit: hits.len() as u32,
            results: hits,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: crate::core::search::SearchSourceMode::SemanticOnly,
            source_mode_applied: crate::core::search::SearchSourceMode::LexicalOnly,
            source_mode_fallback: true,
            strict_source_mode: false,
            memory_scope: MemoryScope::Workspace,
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

    fn write_context_graph_config(workspace_path: &Path, body: &str) -> Result<(), String> {
        let config_dir = workspace_path.join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        std::fs::write(config_dir.join("config.toml"), body).map_err(|error| error.to_string())
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
    fn pack_dna_projection_seed_ids_prioritize_trust_query_then_pack() {
        let trust_a = MemoryId::from_uuid(uuid::Uuid::from_u128(910));
        let trust_b = MemoryId::from_uuid(uuid::Uuid::from_u128(911));
        let query = MemoryId::from_uuid(uuid::Uuid::from_u128(912));
        let pack_only = MemoryId::from_uuid(uuid::Uuid::from_u128(913));
        let input = crate::graph::pack_dna::PackDnaInput {
            pack_memory_ids: vec![pack_only, trust_a],
            query_seed_weights: BTreeMap::from([(query, 0.9)]),
            trust_anchor_memory_ids: vec![trust_b, trust_a],
            ego_radius: crate::graph::pack_dna::DEFAULT_PACK_DNA_EGO_RADIUS,
            ppr_neighbor_limit: crate::graph::pack_dna::DEFAULT_PACK_DNA_PPR_NEIGHBOR_LIMIT,
        };

        assert_eq!(
            super::pack_dna_projection_seed_ids(&input, 3),
            vec![trust_a.to_string(), trust_b.to_string(), query.to_string()]
        );
        assert!(super::pack_dna_projection_seed_ids(&input, 0).is_empty());
    }

    #[test]
    fn context_ppr_weight_defaults_to_disabled() {
        assert_eq!(super::effective_context_ppr_weight(None, None), 0.0);
        assert_eq!(super::effective_context_ppr_weight(None, Some(0.50)), 0.50);
        assert_eq!(super::effective_context_ppr_weight(Some(0.75), None), 0.75);
        assert_eq!(
            super::effective_context_ppr_weight(Some(0.25), Some(0.80)),
            0.25
        );
        assert_eq!(super::effective_context_ppr_weight(Some(2.0), None), 1.0);
        assert_eq!(
            super::effective_context_ppr_weight(Some(f32::NAN), Some(0.25)),
            super::DEFAULT_CONTEXT_PPR_WEIGHT
        );
    }

    #[test]
    fn context_ppr_configured_weight_requires_feature_enabled() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        write_context_graph_config(&fixture.workspace_path, "[graph.ppr]\nalpha = 0.50\n")?;
        assert_eq!(
            super::configured_context_ppr_weight(&fixture.workspace_path)?,
            None
        );

        write_context_graph_config(
            &fixture.workspace_path,
            "[graph.ppr]\nalpha = 0.50\n[graph.feature.ppr]\nenabled = true\n",
        )?;
        assert_eq!(
            super::configured_context_ppr_weight(&fixture.workspace_path)?,
            Some(0.50)
        );

        write_context_graph_config(
            &fixture.workspace_path,
            "[graph.feature.ppr]\nenabled = true\n",
        )?;
        assert_eq!(
            super::configured_context_ppr_weight(&fixture.workspace_path)?,
            Some(super::DEFAULT_CONTEXT_PPR_WEIGHT)
        );
        Ok(())
    }

    #[test]
    fn context_ppr_seed_map_uses_best_positive_candidate_score() -> Result<(), String> {
        let seed = MemoryId::from_uuid(uuid::Uuid::from_u128(904));
        let lexical_only = MemoryId::from_uuid(uuid::Uuid::from_u128(905));
        let excluded = MemoryId::from_uuid(uuid::Uuid::from_u128(906));
        let zero = MemoryId::from_uuid(uuid::Uuid::from_u128(907));
        let candidates = vec![
            ppr_candidate(seed, 0.80)?,
            ppr_candidate(lexical_only, 0.30)?,
            ppr_candidate(zero, 0.10)?,
        ];
        let invalid_hit = SearchHit {
            doc_id: "not-a-memory-id".to_string(),
            ..ppr_hit(seed, 1.0, Some(1.0))
        };
        let search_report = ppr_search_report(vec![
            ppr_hit(seed, 0.10, Some(0.40)),
            ppr_hit(seed, 0.75, Some(0.20)),
            ppr_hit(lexical_only, -0.25, Some(0.30)),
            ppr_hit(zero, 0.0, Some(0.0)),
            ppr_hit(excluded, 0.95, Some(0.95)),
            invalid_hit,
        ]);

        let seed_map = super::personalized_pagerank_seed_map(&search_report, &candidates);

        assert_eq!(seed_map.len(), 2);
        let seed_weight = seed_map
            .get(&seed)
            .copied()
            .ok_or_else(|| "seed should be retained".to_string())?;
        assert!(
            (seed_weight - 0.75).abs() < 1.0e-6,
            "duplicate seed hits should keep the best positive vector/lexical weight: {seed_map:?}"
        );
        let lexical_weight = seed_map
            .get(&lexical_only)
            .copied()
            .ok_or_else(|| "lexical-only hit should be retained".to_string())?;
        assert!(
            (lexical_weight - 0.30).abs() < 1.0e-6,
            "positive lexical score should seed PPR when vector score is invalid: {seed_map:?}"
        );
        assert!(
            !seed_map.contains_key(&excluded),
            "off-candidate hits must not seed PPR: {seed_map:?}"
        );
        assert!(
            !seed_map.contains_key(&zero),
            "non-positive scores must not seed PPR: {seed_map:?}"
        );
        Ok(())
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
    fn context_ppr_omitted_weight_uses_enabled_graph_config_alpha() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        write_context_graph_config(
            &fixture.workspace_path,
            "[graph.ppr]\nalpha = 0.50\n[graph.feature.ppr]\nenabled = true\n",
        )?;
        let mut candidates = vec![
            ppr_candidate(fixture.seed, 0.80)?,
            ppr_candidate(fixture.neighbor, 0.20)?,
            ppr_candidate(fixture.orphan, 0.60)?,
        ];
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut degraded = Vec::new();
        let configured = super::configured_context_ppr_weight(&fixture.workspace_path)?;
        let effective = super::effective_context_ppr_weight(None, configured);

        let metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &search_report,
            &mut candidates,
            effective,
            &mut degraded,
        );

        assert_eq!(configured, Some(0.50));
        assert_eq!(effective, 0.50);
        assert_eq!(metrics.reranked_candidates, 3);
        assert!(
            degraded.is_empty(),
            "enabled graph.ppr.alpha rerank should not degrade: {degraded:?}"
        );
        let neighbor = candidates
            .iter()
            .find(|candidate| candidate.memory_id == fixture.neighbor)
            .ok_or_else(|| "neighbor candidate should remain present".to_string())?;
        let breakdown = neighbor
            .score_breakdown
            .ok_or_else(|| "configured PPR rerank should add score breakdown".to_string())?;
        assert_eq!(breakdown.text_score, 0.20);
        assert_eq!(breakdown.combined_score, neighbor.relevance.into_inner());
        assert!(
            breakdown.combined_score > breakdown.text_score,
            "configured PPR weight should boost graph-linked neighbor: {breakdown:?}"
        );
        Ok(())
    }

    #[test]
    fn context_ppr_cache_separates_same_count_seed_sets() -> Result<(), String> {
        use crate::db::{CreateMemoryLinkInput, MemoryLinkRelation, MemoryLinkSource};

        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        enable_context_ppr_feature(&fixture.workspace_path)?;
        fixture
            .connection
            .insert_memory_link(
                "link_00000000000000000000000912",
                &CreateMemoryLinkInput {
                    src_memory_id: fixture.orphan.to_string(),
                    dst_memory_id: fixture.seed.to_string(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("context-ppr-cache-test".to_string()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        fixture
            .connection
            .insert_graph_snapshot(
                "gsnap_0000000000000000000000902",
                &crate::db::CreateGraphSnapshotInput {
                    workspace_id: WorkspaceId::from_uuid(uuid::Uuid::from_u128(900)).to_string(),
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot.v1".to_string(),
                    graph_type: crate::db::GraphSnapshotType::MemoryLinks,
                    node_count: 3,
                    edge_count: 2,
                    metrics_json: "{}".to_string(),
                    content_hash: "blake3:context-ppr-second-seed".to_string(),
                    source_generation: 2,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        let mut first_candidates = vec![
            ppr_candidate(fixture.seed, 0.10)?,
            ppr_candidate(fixture.neighbor, 0.10)?,
            ppr_candidate(fixture.orphan, 0.10)?,
        ];
        let first_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);
        let mut first_degraded = Vec::new();

        let first_metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &first_report,
            &mut first_candidates,
            1.0,
            &mut first_degraded,
        );

        assert_eq!(first_metrics.reranked_candidates, 3);
        assert!(
            first_degraded.is_empty(),
            "first PPR pass should not degrade: {first_degraded:?}"
        );
        let first_orphan_score = first_candidates
            .iter()
            .find(|candidate| candidate.memory_id == fixture.orphan)
            .map(|candidate| candidate.relevance.into_inner())
            .ok_or_else(|| "first pass should retain orphan candidate".to_string())?;
        assert_eq!(
            first_orphan_score, 0.0,
            "orphan should not inherit rank from a seed-only cache entry"
        );

        let mut second_candidates = vec![
            ppr_candidate(fixture.seed, 0.10)?,
            ppr_candidate(fixture.neighbor, 0.10)?,
            ppr_candidate(fixture.orphan, 0.10)?,
        ];
        let second_report = ppr_search_report(vec![ppr_hit(fixture.orphan, 0.90, Some(0.95))]);
        let mut second_degraded = Vec::new();

        let second_metrics = super::apply_personalized_pagerank_rerank(
            &fixture.connection,
            &fixture.workspace_path,
            &second_report,
            &mut second_candidates,
            1.0,
            &mut second_degraded,
        );

        assert_eq!(second_metrics.reranked_candidates, 3);
        assert!(
            second_degraded.is_empty(),
            "second PPR pass should not degrade: {second_degraded:?}"
        );
        let second_orphan_score = second_candidates
            .iter()
            .find(|candidate| candidate.memory_id == fixture.orphan)
            .map(|candidate| candidate.relevance.into_inner())
            .ok_or_else(|| "second pass should retain orphan candidate".to_string())?;
        assert!(
            second_orphan_score > first_orphan_score,
            "same-count seed sets must not reuse the first PPR cache result"
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
    fn context_ppr_rerank_skips_valid_status_snapshot_when_generation_lags() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        enable_context_ppr_feature(&fixture.workspace_path)?;
        fixture
            .connection
            .insert_memory_link(
                "link_00000000000000000000000903",
                &crate::db::CreateMemoryLinkInput {
                    src_memory_id: fixture.seed.to_string(),
                    dst_memory_id: fixture.orphan.to_string(),
                    relation: crate::db::MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: crate::db::MemoryLinkSource::Agent,
                    created_by: Some("context-ppr-generation-test".to_string()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
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

        assert_eq!(metrics.reranked_candidates, 0);
        assert_eq!(candidates[1].relevance.into_inner(), 0.20);
        assert_eq!(candidates[2].relevance.into_inner(), 0.60);
        assert!(
            degraded.iter().any(
                |entry| entry.code == crate::models::degradation::GRAPH_PPR_SNAPSHOT_STALE_CODE
            ),
            "generation-stale snapshot should emit graph snapshot degradation: {degraded:?}"
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
    fn context_pack_dna_timeout_emits_cataloged_degradation() -> Result<(), String> {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace_path = tempdir.path();
        let database_path = workspace_path.join(".ee").join("ee.db");
        write_context_graph_config(workspace_path, "[graph.feature.pack_dna]\nenabled = true\n")?;

        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(905)).to_string();
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(906));
        let connection =
            DbConnection::open(DatabaseConfig::file(&database_path)).map_err(|e| e.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("context pack dna timeout fixture".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                &memory_id.to_string(),
                &CreateMemoryInput {
                    workspace_id,
                    level: "semantic".to_string(),
                    kind: "fact".to_string(),
                    content: "Graph-rich Pack DNA timeout fixture.".to_string(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: None,
                    trust_class: TrustClass::HumanExplicit.as_str().to_string(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let mut response = context_response_with_pack_item(memory_id)?;
        super::set_context_pack_dna_compute_error(Some(
            crate::graph::GraphError::AlgorithmTimeout {
                algorithm: "pack_dna".to_string(),
                timeout_ms: 125,
            },
        ));
        super::attach_pack_dna_to_context_response(&database_path, &mut response);
        super::set_context_pack_dna_compute_error(None);

        let timeout = response
            .data
            .degraded
            .iter()
            .find(|entry| entry.code == crate::models::degradation::GRAPH_PACK_DNA_TIMEOUT_CODE)
            .ok_or_else(|| "expected graph_pack_dna_timeout degradation".to_string())?;
        assert_eq!(timeout.severity, ContextResponseSeverity::Low);
        assert!(
            timeout
                .message
                .contains("Pack DNA graph explanation timed out")
        );
        assert!(
            timeout
                .message
                .contains("ordinary context pack items remain usable")
        );
        assert_eq!(
            timeout.repair.as_deref(),
            Some(
                "Retry the context request with `--no-pack-dna`; ordinary pack items remain usable without Pack DNA."
            )
        );
        assert!(
            response
                .data
                .degraded
                .iter()
                .all(|entry| entry.code != "context_graph_snapshot_unavailable"),
            "timeout must not be collapsed to generic graph snapshot unavailable"
        );

        let pack_dna =
            response.data.pack_dna.as_ref().ok_or_else(|| {
                "timeout should still expose Pack DNA degraded payload".to_string()
            })?;
        assert_eq!(
            pack_dna["degraded"][0]["code"],
            serde_json::json!(crate::models::degradation::GRAPH_PACK_DNA_TIMEOUT_CODE)
        );
        assert_eq!(
            pack_dna["degraded"][0]["sources"],
            serde_json::json!(["pack_dna"])
        );
        Ok(())
    }

    #[test]
    fn pack_l2_personalization_generation_uses_existing_profile_columns() -> Result<(), String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let empty_generation = super::context_pack_l2_personalization_generation(&connection)
            .map_err(|error| format!("empty personalization generation failed: {error}"))?;
        let empty_generation =
            empty_generation.ok_or_else(|| "empty generation should be hashable".to_string())?;
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(907)).to_string();
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(908)).to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-pack-l2-profile-generation".to_owned(),
                    name: Some("pack l2 profile generation".to_owned()),
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
                    content: "Use existing profile columns for personalization generation."
                        .to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: None,
                    trust_class: TrustClass::AgentAssertion.as_str().to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        connection
            .upsert_agent_context_profile_event(&UpsertAgentContextProfileInput {
                workspace_id,
                agent_name: "ProudWillow".to_owned(),
                memory_id,
                counts_delta: AgentContextProfileCounts::new(1, 0, 0),
                last_seen_at: Some("2026-05-16T01:12:00Z".to_owned()),
                weight_cached: 0.04,
            })
            .map_err(|error| error.to_string())?;

        let generation = super::context_pack_l2_personalization_generation(&connection)
            .map_err(|error| format!("profile personalization generation failed: {error}"))?;
        let generation =
            generation.ok_or_else(|| "profile generation should be hashable".to_string())?;
        assert_ne!(generation, empty_generation);
        connection.close().map_err(|error| error.to_string())
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

    #[test]
    fn context_ppr_weight_half_reranks_more_than_default_weight() -> Result<(), String> {
        let fixture = ppr_context_fixture(crate::db::GraphSnapshotStatus::Valid)?;
        enable_context_ppr_feature(&fixture.workspace_path)?;
        let search_report = ppr_search_report(vec![ppr_hit(fixture.seed, 0.90, Some(0.95))]);

        let run_with_weight = |ppr_weight: f32| -> Result<(f32, PackScoreBreakdown), String> {
            let mut candidates = vec![
                ppr_candidate(fixture.seed, 0.80)?,
                ppr_candidate(fixture.neighbor, 0.20)?,
                ppr_candidate(fixture.orphan, 0.60)?,
            ];
            let mut degraded = Vec::new();

            let metrics = super::apply_personalized_pagerank_rerank(
                &fixture.connection,
                &fixture.workspace_path,
                &search_report,
                &mut candidates,
                ppr_weight,
                &mut degraded,
            );

            assert_eq!(metrics.reranked_candidates, 3);
            assert!(
                degraded.is_empty(),
                "valid snapshot should not degrade at weight {ppr_weight}: {degraded:?}"
            );
            let neighbor = candidates
                .iter()
                .find(|candidate| candidate.memory_id == fixture.neighbor)
                .ok_or_else(|| "neighbor candidate should remain present".to_string())?;
            let breakdown = neighbor
                .score_breakdown
                .ok_or_else(|| "reranked neighbor should carry score breakdown".to_string())?;
            Ok((neighbor.relevance.into_inner(), breakdown))
        };

        let (default_score, default_breakdown) =
            run_with_weight(super::DEFAULT_CONTEXT_PPR_WEIGHT)?;
        let (half_score, half_breakdown) = run_with_weight(0.50)?;

        assert_eq!(default_breakdown.text_score, 0.20);
        assert_eq!(half_breakdown.text_score, default_breakdown.text_score);
        assert_eq!(half_breakdown.ppr_score, default_breakdown.ppr_score);
        assert!(
            default_breakdown.ppr_score > default_breakdown.text_score,
            "fixture neighbor must receive a graph boost: {default_breakdown:?}"
        );
        assert!(
            half_score > default_score,
            "higher PPR weight should move the linked neighbor farther toward its PPR score"
        );
        assert_eq!(default_breakdown.combined_score, default_score);
        assert_eq!(half_breakdown.combined_score, half_score);
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
    fn lexical_fallback_score_ties_use_radix_memory_id_order() {
        let lower_id = MemoryId::from_uuid(uuid::Uuid::from_u128(7010)).to_string();
        let higher_id = MemoryId::from_uuid(uuid::Uuid::from_u128(7020)).to_string();
        let top_score_id = MemoryId::from_uuid(uuid::Uuid::from_u128(7030)).to_string();

        let mut lower =
            stored_memory_with_time("2026-05-01T12:00:00Z", "2026-05-01T12:00:00Z", None, None);
        lower.id = lower_id.clone();
        let mut higher = lower.clone();
        higher.id = higher_id.clone();
        let mut top_score = lower.clone();
        top_score.id = top_score_id.clone();

        let mut scored = vec![(higher, 0.7), (top_score, 0.9), (lower, 0.7)];
        super::sort_scored_memories_by_score_then_memory_id(&mut scored);

        let ids = scored
            .into_iter()
            .map(|(memory, _)| memory.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![top_score_id, lower_id, higher_id]);
    }

    #[test]
    fn lexical_fallback_score_ties_include_workspace_tiebreaker() {
        let local_id = MemoryId::from_uuid(uuid::Uuid::from_u128(7040)).to_string();
        let peer_id = MemoryId::from_uuid(uuid::Uuid::from_u128(7030)).to_string();
        let mut local =
            stored_memory_with_time("2026-05-01T12:00:00Z", "2026-05-01T12:00:00Z", None, None);
        local.id = local_id.clone();
        local.workspace_id = "wsp_b".to_owned();
        let mut peer = local.clone();
        peer.id = peer_id.clone();
        peer.workspace_id = "wsp_a".to_owned();

        let mut scored = vec![(local, 0.7), (peer, 0.7)];
        super::sort_scored_memories_by_score_then_memory_id(&mut scored);

        let ordered = scored
            .into_iter()
            .map(|(memory, _)| (memory.workspace_id, memory.id))
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                ("wsp_a".to_owned(), peer_id),
                ("wsp_b".to_owned(), local_id)
            ]
        );
    }

    #[test]
    fn provenance_marks_intentional_cross_shard_context_memory() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut memory =
            stored_memory_with_time("2026-05-01T12:00:00Z", "2026-05-01T12:00:00Z", None, None);
        memory.workspace_id = "wsp_peer".to_owned();
        let memory_id = memory
            .id
            .parse::<MemoryId>()
            .map_err(|error| error.to_string())?;
        let mut degraded = Vec::new();

        let provenance =
            super::provenance_for_memory(&memory, memory_id, temp.path(), &mut degraded)
                .ok_or_else(|| "cross-shard provenance should render".to_owned())?;

        assert!(provenance.note.contains("cross_shard_read"));
        assert!(provenance.note.contains("origin_workspace_id=wsp_peer"));
        assert!(provenance.note.contains("pack_workspace_id="));
        Ok(())
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: crate::core::search::SearchSourceMode::SemanticOnly,
            source_mode_applied: crate::core::search::SearchSourceMode::LexicalOnly,
            source_mode_fallback: true,
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
            None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            None,
        );
        let mut second_degraded = Vec::new();
        let (second_candidates, second_metrics) = super::candidates_from_search_with_metrics(
            &connection,
            workspace_path,
            &search_report,
            &crate::models::QueryFilters::default(),
            false,
            &mut second_degraded,
            None,
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
        let draft = assemble_draft_with_profile_and_options(
            request.profile,
            request.query.clone(),
            TokenBudget::new(60).map_err(|error| error.to_string())?,
            [candidate_a, candidate_b],
            PackAssemblyOptions {
                lod_budget_shares: None,
                ..PackAssemblyOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
        let search_report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: crate::core::search::SearchSourceMode::SemanticOnly,
            source_mode_applied: crate::core::search::SearchSourceMode::LexicalOnly,
            source_mode_fallback: true,
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
            source_mode: crate::core::search::SearchSourceMode::SemanticOnly,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
        };
        let trace = ContextPerformanceTrace {
            db_open_count: 1,
            index_status_checks: 1,
            pack_record_writes: 1,
            read_snapshot: Some(ReadSnapshotTrace {
                pinned: true,
                slot_id: Some(7),
                snapshot_generation: Some(42),
                lease_held_ms: 12,
                expired: false,
                poisoned: false,
            }),
            candidate_resolution: CandidateResolutionMetrics {
                search_hits: 2,
                resolved_memory_ids: 2,
                unique_memory_ids: 2,
                memory_batch_reads: 1,
                tag_batch_reads: 1,
                converted_candidates: 2,
                ..CandidateResolutionMetrics::default()
            },
            pack_persistence: PackPersistenceSubspans {
                attempted: true,
                succeeded: true,
                item_count: 2,
                omission_count: 2,
                item_write_batches: 1,
                omission_write_batches: 1,
                ledger_serialization: Duration::from_millis(4),
                record_write: Duration::from_millis(5),
                item_writes: Duration::from_millis(6),
                omission_writes: Duration::from_millis(7),
                transaction: Duration::from_millis(21),
                audit: Duration::from_millis(2),
                ..PackPersistenceSubspans::default()
            },
            timings: vec![
                PerformanceTiming {
                    name: "pprRerank",
                    elapsed: Duration::from_millis(2),
                },
                PerformanceTiming {
                    name: "packAssembly",
                    elapsed: Duration::from_millis(3),
                },
            ],
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
        assert_eq!(
            json["data"]["queryPlan"]["sourceModeRequested"],
            "semantic_only"
        );
        assert_eq!(
            json["data"]["queryPlan"]["sourceModeApplied"],
            "lexical_only"
        );
        assert_eq!(json["data"]["queryPlan"]["strictSourceMode"], false);
        assert_eq!(json["data"]["queryPlan"]["fallbackApplied"], true);
        assert_eq!(json["data"]["dbReads"]["memoryBatchReads"], 1);
        assert_eq!(
            json["data"]["dbReads"]["readSnapshot"]["surface"],
            "read_snapshot"
        );
        assert_eq!(json["data"]["dbReads"]["readSnapshot"]["pinned"], true);
        assert_eq!(json["data"]["dbReads"]["readSnapshot"]["slotId"], 7);
        assert_eq!(
            json["data"]["dbReads"]["readSnapshot"]["snapshotGeneration"],
            42
        );
        assert_eq!(json["data"]["dbReads"]["readSnapshot"]["leaseHeldMs"], 12);
        assert_eq!(json["data"]["candidates"]["convertedCandidates"], 2);
        assert_eq!(json["data"]["pack"]["pruning"]["tokenBudgetExceeded"], 2);
        assert_eq!(json["data"]["pack"]["persistence"]["attempted"], true);
        assert_eq!(
            json["data"]["pack"]["persistence"]["subspans"]["ledgerSerialization"]["elapsedMs"],
            4.0
        );
        assert_eq!(
            json["data"]["pack"]["persistence"]["subspans"]["itemWrites"]["elapsedMs"],
            6.0
        );
        assert_eq!(
            json["data"]["pack"]["persistence"]["subspans"]["transactionOverhead"]["elapsedMs"],
            3.0
        );
        assert_eq!(json["data"]["cache"]["status"], "fallback");
        assert!(
            json["data"]["timings"]
                .as_array()
                .is_some_and(|timings| timings.iter().any(|timing| timing["name"] == "pprRerank")),
            "performance output should expose PPR rerank timing: {json:#?}"
        );
        assert_eq!(json["data"]["redaction"]["memoryContentIncluded"], false);
        assert!(!rendered.contains("sk_live_do_not_emit"));
        assert!(!rendered.contains("SECRET_VALUE_ONE"));
        assert!(!rendered.contains("SECRET_VALUE_TWO"));
        assert!(!rendered.contains(&memory_a.to_string()));
        Ok(())
    }

    #[test]
    fn l2_hit_performance_query_plan_reports_source_mode_policy() -> Result<(), String> {
        let request = ContextRequest::new(ContextRequestInput {
            query: "prepare release".to_string(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(600),
            candidate_pool: Some(12),
            max_results: Some(4),
            sections: Vec::new(),
        })
        .map_err(|error| error.to_string())?;
        let options = super::ContextPackOptions {
            workspace_path: PathBuf::from("/tmp/ee-l2-hit-performance"),
            database_path: None,
            index_dir: None,
            query: request.query.clone(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
            strict_source_mode: true,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(600),
            candidate_pool: Some(12),
            max_results: Some(4),
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::SelfOnly,
            strict_scope: true,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
        };
        let trace = ContextPerformanceTrace::default();
        let source_mode_metadata = super::ContextPackL2SourceModeMetadata::from_options(&options);

        let json = super::context_pack_l2_hit_performance_json(
            "pack",
            &options,
            &request,
            &trace,
            super::ContextPackL2HitCacheMetadata {
                key: "blake3:l2-test-key",
                byte_len: 123,
                compression: None,
                source_mode: source_mode_metadata,
            },
        );

        assert_eq!(json["schema"], PERFORMANCE_EXPLAIN_SCHEMA_V1);
        assert_eq!(json["data"]["cache"]["status"], "hit");
        assert_eq!(
            json["data"]["queryPlan"]["sourceModeRequested"],
            "lexical_only"
        );
        assert_eq!(
            json["data"]["queryPlan"]["sourceModeApplied"],
            "lexical_only"
        );
        assert_eq!(json["data"]["queryPlan"]["strictSourceMode"], true);
        assert_eq!(json["data"]["queryPlan"]["fallbackApplied"], false);
        assert_eq!(json["data"]["queryPlan"]["memoryScope"], "self");
        assert_eq!(json["data"]["queryPlan"]["strictScope"], true);
        Ok(())
    }

    #[test]
    fn l2_source_mode_metadata_uses_authoritative_search_report_policy() {
        let stale_options_requested = crate::core::search::SearchSourceMode::Hybrid;
        let mut report =
            super::missing_index_search_report("source metadata", 10, test_runtime_profile());
        report.source_mode_requested = crate::core::search::SearchSourceMode::LexicalOnly;
        report.source_mode_applied = crate::core::search::SearchSourceMode::LexicalOnly;
        report.strict_source_mode = true;
        report.source_mode_fallback = false;

        let metadata = super::ContextPackL2SourceModeMetadata::from_search_report(&report);

        assert_eq!(
            metadata.requested, report.source_mode_requested,
            "stored L2 source metadata must use SearchReport requested mode, not stale options"
        );
        assert_ne!(metadata.requested, stale_options_requested);
        assert_eq!(metadata.applied, report.source_mode_applied);
        assert_eq!(metadata.strict, report.strict_source_mode);
        assert_eq!(metadata.fallback, report.source_mode_fallback);
    }

    #[test]
    fn l2_advisory_snapshot_refreshes_both_reranker_availability_transitions() {
        let absent = SearchDegradation::rerank_model_absent();
        let transient = SearchDegradation {
            code: "rerank_model_unavailable".to_owned(),
            severity: "low".to_owned(),
            message: "registered reranker failed to load".to_owned(),
            repair: None,
        };
        let unrelated = SearchDegradation::stale_index(Some(7), Some(3));
        let mut cached_absent = super::ContextSearchAdvisorySnapshot {
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            rerank_score_count: 0,
            degraded: vec![absent, unrelated.clone()],
        };
        let available_now = super::ContextSearchAdvisorySnapshot {
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 25,
            rerank_runtime_available: true,
            rerank_score_count: 0,
            degraded: Vec::new(),
        };

        cached_absent.refresh_rerank_posture_from(&available_now);
        assert!(cached_absent.rerank_runtime_available);
        assert_eq!(cached_absent.rerank_configured_top_k, 25);
        assert!(
            cached_absent
                .degraded
                .iter()
                .all(|entry| entry.code != "rerank_model_unavailable")
        );
        assert_eq!(cached_absent.degraded, vec![unrelated.clone()]);

        let mut cached_available = super::ContextSearchAdvisorySnapshot {
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 25,
            rerank_runtime_available: true,
            rerank_score_count: 8,
            degraded: vec![unrelated.clone()],
        };
        let unavailable_now = super::ContextSearchAdvisorySnapshot {
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            rerank_score_count: 0,
            degraded: vec![transient.clone()],
        };

        cached_available.refresh_rerank_posture_from(&unavailable_now);
        assert!(!cached_available.rerank_runtime_available);
        assert_eq!(cached_available.rerank_configured_top_k, 50);
        assert_eq!(
            cached_available.rerank_score_count, 8,
            "cached pack provenance must retain the score count that shaped its selection"
        );
        assert_eq!(cached_available.degraded, vec![unrelated, transient]);
    }

    #[test]
    fn l2_hit_revalidates_stale_reranker_posture_without_losing_the_hit() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let cache = crate::cache::pack_l2::PackL2Cache::new(
            tempdir.path().join("pack-l2"),
            crate::cache::pack_l2::PackL2CacheOptions::default(),
        );
        let request = ContextRequest::from_query("refresh reranker posture")
            .map_err(|error| error.to_string())?;
        let output_options =
            super::ContextPackOutputOptions::default().with_cache_json_response(true);
        let options = super::ContextPackOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database_path.clone()),
            index_dir: None,
            query: request.query.clone(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options,
            persist_pack: false,
            baseline_write: None,
            no_lod: false,
        };
        let search_options = SearchOptions {
            workspace_path: workspace,
            database_path: Some(database_path),
            index_dir: None,
            query: request.query.clone(),
            limit: 10,
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
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };
        let key_input = super::PackL2CacheKeyInput {
            workspace_id: "wsp_l2_reranker_refresh".to_owned(),
            database_generation: 1,
            index_generation: 1,
            graph_generation: None,
            embed_backend: EmbedBackend::HashFallback,
            redaction_level: options.redaction_level,
            request: request.clone(),
            output_options,
            include_legacy_selection_certificate: false,
            memory_scope: options.memory_scope,
            strict_scope: options.strict_scope,
            source_mode: options.source_mode,
            strict_source_mode: options.strict_source_mode,
            context_feature_flags_hash: "blake3:test-features".to_owned(),
            personalization_generation: None,
        };
        let key = super::compute_pack_l2_cache_key(&key_input);
        let l2_context = super::ContextPackL2Context {
            cache: cache.clone(),
            key: key.clone(),
            key_input,
        };
        let stale_available = super::ContextSearchAdvisorySnapshot {
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: true,
            rerank_score_count: 7,
            degraded: Vec::new(),
        };
        let response_json = serde_json::json!({
            "schema": crate::models::RESPONSE_SCHEMA_V2,
            "success": true,
            "data": {
                "command": PACK_COMMAND,
                "embed_backend": "hash_fallback",
                "pack": { "schema": crate::models::PACK_SCHEMA_V2 }
            },
            "degraded": []
        })
        .to_string();
        let payload = serde_json::json!({
            "schema": super::PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3,
            "responseJson": response_json,
            "searchAdvisorySnapshot": stale_available.cache_json(),
            "sourceMode": {
                "requested": "hybrid",
                "applied": "hybrid",
                "strict": false,
                "fallback": false
            }
        });
        cache
            .put_compressed(&key, &payload)
            .map_err(|error| error.to_string())?;

        let mut trace = super::ContextPerformanceTrace::default();
        let mut degraded = Vec::new();
        let cached_run = super::context_pack_l2_try_hit(
            &l2_context,
            PACK_COMMAND,
            &options,
            &search_options,
            &connection,
            &request,
            std::time::Instant::now(),
            &mut trace,
            &mut degraded,
        )
        .ok_or_else(|| "seeded L2 entry should remain a cache hit".to_owned())?;

        assert_eq!(
            cached_run
                .performance
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            Some("hit")
        );
        assert!(cached_run.response.cached_json.is_some());
        assert!(!cached_run.search_advisory_snapshot.rerank_runtime_available);
        assert_eq!(
            cached_run.search_advisory_snapshot.rerank_score_count, 7,
            "the cached ranking provenance must survive runtime-posture refresh"
        );
        assert!(
            cached_run
                .search_advisory_snapshot
                .degraded
                .iter()
                .any(|entry| entry.code == "rerank_model_unavailable")
        );
        assert!(degraded.is_empty());
        Ok(())
    }

    #[test]
    fn l2_cached_response_json_preserves_current_payload_bytes() -> Result<(), String> {
        let response_json = serde_json::json!({
            "schema": crate::models::RESPONSE_SCHEMA_V2,
            "success": true,
            "data": {
                "command": "pack",
                "embed_backend": "hash_fallback",
                "pack": {
                    "schema": crate::models::PACK_SCHEMA_V2,
                    "query": "prepare release"
                }
            },
            "degraded": []
        })
        .to_string();
        let payload = serde_json::json!({
            "schema": super::PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3,
            "responseJson": response_json,
        });

        let replayed = super::context_pack_l2_cached_response_json(
            &payload,
            "pack",
            crate::models::EmbedBackend::HashFallback,
        )?;

        assert_eq!(
            replayed,
            payload
                .get("responseJson")
                .and_then(serde_json::Value::as_str)
                .unwrap(),
            "current cached responses should replay byte-identically"
        );
        Ok(())
    }

    #[test]
    fn l2_cached_response_json_rejects_unattributed_embedding_backend() {
        let payload = serde_json::json!({
            "schema": super::PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3,
            "responseJson": serde_json::json!({
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": {
                    "command": "pack",
                    "pack": {
                        "schema": crate::models::PACK_SCHEMA_V2,
                        "query": "prepare release"
                    }
                },
                "degraded": []
            })
            .to_string(),
        });

        let error = super::context_pack_l2_cached_response_json(
            &payload,
            "pack",
            crate::models::EmbedBackend::HashFallback,
        )
        .expect_err("cache payloads without an embedding backend must be invalidated");
        assert!(
            error.contains("missing a valid data.embed_backend"),
            "unexpected cache rejection: {error}"
        );
    }

    #[test]
    fn l2_cached_response_json_rejects_backend_mismatch_with_cache_key() {
        let payload = serde_json::json!({
            "schema": super::PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3,
            "responseJson": serde_json::json!({
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": {
                    "command": "pack",
                    "embed_backend": "neural_local",
                    "pack": {
                        "schema": crate::models::PACK_SCHEMA_V2,
                        "query": "prepare release"
                    }
                },
                "degraded": []
            })
            .to_string(),
        });

        let error = super::context_pack_l2_cached_response_json(
            &payload,
            "pack",
            crate::models::EmbedBackend::HashFallback,
        )
        .expect_err("a neural response stored under a hash key must be rejected");
        assert!(
            error.contains("neural_local does not match cache key backend hash_fallback"),
            "unexpected cache rejection: {error}"
        );
    }

    #[test]
    fn l2_cached_response_json_rejects_v1_banner_semantics() -> Result<(), String> {
        let payload = serde_json::json!({
            "schema": "ee.pack.l2_context_response.v1",
            "responseJson": serde_json::json!({
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": {
                    "command": "pack",
                    "embed_backend": "hash_fallback",
                    "pack": {
                        "schema": crate::models::PACK_SCHEMA_V2,
                        "query": "prepare release",
                        "advisoryBanner": {
                            "status": "degraded",
                            "degradationCount": 1
                        }
                    },
                    "degraded": [{
                        "code": "index_missing",
                        "severity": "medium",
                        "message": "stale pre-filter cache semantics"
                    }]
                },
                "degraded": [{
                    "code": "index_missing",
                    "severity": "medium",
                    "message": "stale pre-filter cache semantics"
                }]
            })
            .to_string(),
        });

        let error = super::context_pack_l2_cached_response_json(
            &payload,
            "pack",
            crate::models::EmbedBackend::HashFallback,
        )
        .expect_err("v1 cached response semantics must be invalidated");
        assert!(
            error.contains("unexpected schema ee.pack.l2_context_response.v1"),
            "unexpected cache rejection: {error}"
        );
        Ok(())
    }

    #[test]
    fn l2_cached_response_json_backfills_inner_pack_schema() -> Result<(), String> {
        let response_json = serde_json::json!({
            "schema": crate::models::RESPONSE_SCHEMA_V2,
            "success": true,
            "data": {
                "command": "pack",
                "embed_backend": "hash_fallback",
                "pack": {
                    "query": "prepare release"
                }
            },
            "degraded": []
        })
        .to_string();
        let payload = serde_json::json!({
            "schema": super::PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3,
            "responseJson": response_json,
        });

        let replayed = super::context_pack_l2_cached_response_json(
            &payload,
            "pack",
            crate::models::EmbedBackend::HashFallback,
        )?;
        let replayed_json = serde_json::from_str::<serde_json::Value>(&replayed)
            .map_err(|error| error.to_string())?;

        assert_eq!(
            replayed_json.pointer("/data/pack/schema"),
            Some(&serde_json::json!(crate::models::PACK_SCHEMA_V2)),
            "stale cached responses should be normalized to the documented inner pack schema"
        );
        Ok(())
    }

    #[test]
    fn l2_hit_performance_query_plan_uses_cached_source_mode_fallback() -> Result<(), String> {
        let request = ContextRequest::new(ContextRequestInput {
            query: "prepare release".to_string(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(600),
            candidate_pool: Some(12),
            max_results: Some(4),
            sections: Vec::new(),
        })
        .map_err(|error| error.to_string())?;
        let options = super::ContextPackOptions {
            workspace_path: PathBuf::from("/tmp/ee-l2-hit-performance-fallback"),
            database_path: None,
            index_dir: None,
            query: request.query.clone(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::SemanticOnly,
            strict_source_mode: false,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(600),
            candidate_pool: Some(12),
            max_results: Some(4),
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::SelfOnly,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
        };
        let payload = serde_json::json!({
            "schema": super::PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3,
            "responseJson": "{\"schema\":\"ee.response.v2\",\"success\":true,\"data\":{\"command\":\"pack\"},\"degraded\":[]}",
            "sourceMode": {
                "requested": "semantic_only",
                "applied": "lexical_only",
                "strict": false,
                "fallback": true
            }
        });
        let source_mode_metadata =
            super::context_pack_l2_cached_source_mode_metadata(&payload, &options);
        let json = super::context_pack_l2_hit_performance_json(
            "pack",
            &options,
            &request,
            &ContextPerformanceTrace::default(),
            super::ContextPackL2HitCacheMetadata {
                key: "blake3:l2-test-key",
                byte_len: 123,
                compression: None,
                source_mode: source_mode_metadata,
            },
        );

        assert_eq!(
            json["data"]["queryPlan"]["sourceModeRequested"],
            "semantic_only"
        );
        assert_eq!(
            json["data"]["queryPlan"]["sourceModeApplied"],
            "lexical_only"
        );
        assert_eq!(json["data"]["queryPlan"]["strictSourceMode"], false);
        assert_eq!(json["data"]["queryPlan"]["fallbackApplied"], true);

        let mut fallbackless_payload = payload;
        fallbackless_payload
            .pointer_mut("/sourceMode")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "test payload missing sourceMode".to_string())?
            .remove("fallback");
        let fallbackless_metadata =
            super::context_pack_l2_cached_source_mode_metadata(&fallbackless_payload, &options);
        assert!(fallbackless_metadata.fallback);
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
                    workspace_id: workspace_id.clone(),
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
            query: "fmt before release".to_owned(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
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
    fn context_pack_l2_hit_replays_fresh_json_byte_identically() -> Result<(), String> {
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
        let cache_root = tempdir.path().join("pack-l2");
        std::fs::write(
            ee_dir.join("config.toml"),
            format!(
                "[cache.pack_l2]\ndirectory = {:?}\n",
                cache_root.to_string_lossy()
            ),
        )
        .map_err(|error| error.to_string())?;

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
                    workspace_id: workspace_id.clone(),
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

        let options = super::ContextPackOptions {
            workspace_path: workspace,
            database_path: Some(db_path),
            index_dir: Some(empty_index_dir),
            query: "format before release".to_owned(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: super::ContextPackOutputOptions::default()
                .with_cache_json_response(true),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
        };

        let fresh_run = super::run_context_pack_with_performance(&options, PACK_COMMAND)
            .map_err(|error| error.to_string())?;
        let fresh = fresh_run.response;
        assert!(
            fresh.cached_json.is_none(),
            "first run should assemble fresh output"
        );
        let workspace_cache_dir =
            cache_root.join(super::pack_l2_workspace_component(&workspace_id));
        let cache_entry_paths = std::fs::read_dir(&workspace_cache_dir)
            .map_err(|error| error.to_string())?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert!(
            !cache_entry_paths.is_empty(),
            "first run should publish at least one L2 cache entry"
        );
        let cache_entry_json = cache_entry_paths
            .iter()
            .filter_map(|path| {
                std::fs::read(path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            })
            .find(|entry| {
                entry.get("schema").and_then(serde_json::Value::as_str)
                    == Some(crate::cache::pack_l2::PACK_L2_CACHE_ENTRY_SCHEMA_V2)
            })
            .ok_or_else(|| "first run should publish a compressed v2 L2 cache entry".to_owned())?;
        assert_eq!(
            cache_entry_json
                .get("schema")
                .and_then(serde_json::Value::as_str),
            Some(crate::cache::pack_l2::PACK_L2_CACHE_ENTRY_SCHEMA_V2),
            "context L2 store should use the compressed v2 entry schema"
        );
        assert!(
            cache_entry_json
                .pointer("/compression/compressedPayloadBase64")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "compressed v2 entry should carry compressed payload bytes"
        );
        let cached_run = super::run_context_pack_with_performance(&options, PACK_COMMAND)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            cached_run
                .performance
                .pointer("/data/cache/status")
                .and_then(serde_json::Value::as_str),
            Some("hit"),
            "second identical pack request must remain an L2 hit"
        );
        let mut advisory_session = SearchAdvisorySession::default();
        let advisory_workspace_id = super::stable_context_workspace_id(&workspace);
        let mut render_advisory = |snapshot: &super::ContextSearchAdvisorySnapshot| {
            let mut reservation = advisory_session.reserve_delivery(&advisory_workspace_id);
            let data = snapshot.data_json_with_delivery_reservation(
                &mut advisory_session,
                &advisory_workspace_id,
                &mut reservation,
            );
            assert_eq!(
                advisory_session.settle_delivery(
                    reservation.workspace_id(),
                    reservation.token(),
                    true,
                    reservation.large_gap_capacity_busy(),
                ),
                crate::core::search::SearchAdvisorySettlement::Complete
            );
            data
        };
        let fresh_advisory = render_advisory(&fresh_run.search_advisory_snapshot);
        let cached_advisory = render_advisory(&cached_run.search_advisory_snapshot);
        assert_eq!(
            fresh_advisory
                .pointer("/rerank/advisory/code")
                .and_then(serde_json::Value::as_str),
            Some("rerank_model_unavailable"),
            "fresh pack must emit the permanent advisory"
        );
        assert!(
            cached_advisory
                .pointer("/rerank/advisory")
                .is_some_and(serde_json::Value::is_null),
            "L2 hit must share the process advisory ledger and suppress repetition"
        );
        assert_eq!(
            cached_advisory
                .pointer("/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        let cached = cached_run.response;
        assert!(
            cached.cached_json.is_some(),
            "second run should return the L2 cached JSON response"
        );
        let render_options = crate::output::ContextJsonRenderOptions::from(options.output_options);
        let fresh_json =
            crate::output::render_context_response_json_with_options(&fresh, render_options);
        let cached_json =
            crate::output::render_context_response_json_with_options(&cached, render_options);
        for (label, rendered) in [("fresh", &fresh_json), ("cached", &cached_json)] {
            let parsed = serde_json::from_str::<serde_json::Value>(rendered)
                .map_err(|error| format!("parse {label} L2 response JSON: {error}"))?;
            assert_eq!(
                parsed.pointer("/degraded"),
                parsed.pointer("/data/degraded"),
                "{label} L2 response must mirror top-level and data degradations"
            );
            let degraded = parsed
                .pointer("/data/degraded")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| format!("{label} L2 response data.degraded must be an array"))?;
            assert!(
                degraded.iter().all(|entry| {
                    entry.get("code").and_then(serde_json::Value::as_str) != Some("index_missing")
                }),
                "{label} default L2 response must filter non-affecting index_missing"
            );
            assert_eq!(
                parsed
                    .pointer("/data/pack/advisoryBanner/degradationCount")
                    .and_then(serde_json::Value::as_u64),
                Some(degraded.len() as u64),
                "{label} L2 advisory banner must count the filtered degradation set"
            );
        }
        assert_eq!(
            fresh_json, cached_json,
            "L2 hit must replay byte-identical JSON"
        );
        Ok(())
    }

    #[test]
    fn context_pack_l2_does_not_cache_source_mode_fallback_runs() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let cache_root = tempdir.path().join("pack-l2").join("workspace");
        let cache = crate::cache::pack_l2::PackL2Cache::new(
            cache_root,
            crate::cache::pack_l2::PackL2CacheOptions::default(),
        );
        let options = super::ContextPackOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: None,
            index_dir: None,
            query: "lexical fallback".to_owned(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::SemanticOnly,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: super::ContextPackOutputOptions::default()
                .with_cache_json_response(true),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
        };
        let key_input = super::PackL2CacheKeyInput {
            workspace_id: "wsp_l2_source_mode_fallback".to_owned(),
            database_generation: 1,
            index_generation: 1,
            graph_generation: None,
            embed_backend: crate::models::EmbedBackend::HashFallback,
            redaction_level: options.redaction_level,
            request: ContextRequest::from_query("lexical fallback")
                .map_err(|error| error.to_string())?,
            output_options: options.output_options,
            include_legacy_selection_certificate: false,
            memory_scope: options.memory_scope,
            strict_scope: options.strict_scope,
            source_mode: options.source_mode,
            strict_source_mode: options.strict_source_mode,
            context_feature_flags_hash: "blake3:test-features".to_owned(),
            personalization_generation: None,
        };
        let l2_context = super::ContextPackL2Context {
            cache: cache.clone(),
            key: "blake3:l2-source-mode-fallback".to_owned(),
            key_input,
        };
        let mut search_report =
            super::missing_index_search_report("lexical fallback", 10, test_runtime_profile());
        search_report.source_mode_requested = crate::core::search::SearchSourceMode::SemanticOnly;
        search_report.source_mode_applied = crate::core::search::SearchSourceMode::LexicalOnly;
        search_report.source_mode_fallback = true;
        let mut response =
            context_response_with_pack_item(MemoryId::from_uuid(uuid::Uuid::from_u128(44)))?;

        super::context_pack_l2_store(&l2_context, &options, &search_report, &mut response);

        assert!(
            matches!(
                cache
                    .get("blake3:l2-source-mode-fallback")
                    .map_err(|error| error.to_string())?,
                crate::cache::pack_l2::PackL2CacheLookup::Miss(_)
            ),
            "source-mode fallback payload should not be written to L2"
        );
        Ok(())
    }

    #[test]
    fn context_pack_l2_rekeys_storage_to_backend_that_produced_response() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let cache = crate::cache::pack_l2::PackL2Cache::new(
            tempdir.path().join("pack-l2").join("workspace"),
            crate::cache::pack_l2::PackL2CacheOptions::default(),
        );
        let options = super::ContextPackOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: None,
            index_dir: None,
            query: "backend transition".to_owned(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: super::ContextPackOutputOptions::default()
                .with_cache_json_response(true),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
        };
        let key_input = super::PackL2CacheKeyInput {
            workspace_id: "wsp_l2_backend_transition".to_owned(),
            database_generation: 1,
            index_generation: 1,
            graph_generation: None,
            embed_backend: EmbedBackend::HashFallback,
            redaction_level: options.redaction_level,
            request: ContextRequest::from_query("backend transition")
                .map_err(|error| error.to_string())?,
            output_options: options.output_options,
            include_legacy_selection_certificate: false,
            memory_scope: options.memory_scope,
            strict_scope: options.strict_scope,
            source_mode: options.source_mode,
            strict_source_mode: options.strict_source_mode,
            context_feature_flags_hash: "blake3:test-features".to_owned(),
            personalization_generation: None,
        };
        let lookup_key = super::compute_pack_l2_cache_key(&key_input);
        let l2_context = super::ContextPackL2Context {
            cache: cache.clone(),
            key: lookup_key.clone(),
            key_input: key_input.clone(),
        };
        let search_report =
            super::missing_index_search_report("backend transition", 10, test_runtime_profile());
        let mut response =
            context_response_with_pack_item(MemoryId::from_uuid(uuid::Uuid::from_u128(45)))?;
        response.data.embed_backend = EmbedBackend::NeuralLocal;

        super::context_pack_l2_store(&l2_context, &options, &search_report, &mut response);

        let mut neural_key_input = key_input;
        neural_key_input.embed_backend = EmbedBackend::NeuralLocal;
        let neural_key = super::compute_pack_l2_cache_key(&neural_key_input);
        assert_ne!(lookup_key, neural_key);
        assert!(matches!(
            cache.get(&lookup_key).map_err(|error| error.to_string())?,
            crate::cache::pack_l2::PackL2CacheLookup::Miss(_)
        ));
        assert!(matches!(
            cache.get(&neural_key).map_err(|error| error.to_string())?,
            crate::cache::pack_l2::PackL2CacheLookup::Hit(_)
        ));
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
                    source_mode: crate::core::search::SearchSourceMode::Hybrid,
                    strict_source_mode: false,
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
                    relevance_floor: None,
                    redaction_level: crate::models::RedactionLevel::Minimal,
                    memory_scope: MemoryScope::Swarm,
                    strict_scope: false,
                    ppr_weight: None,
                    changed_symbols: Vec::new(),
                    changed_symbols_from_git: false,
                    pagination: None,
                    coordination_snapshot_path: None,
                    coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                    task_lens: None,
                    require_fresh_sentinels: false,
                    output_options: Default::default(),
                    persist_pack: true,
                    baseline_write: None,
                    no_lod: false,
                },
                determinism,
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
    fn lab_runtime_cancellation_after_pack_persistence_is_not_laundered() -> Result<(), String> {
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
                &MemoryId::from_uuid(uuid::Uuid::from_u128(4243)).to_string(),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
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
        connection.close().map_err(|error| error.to_string())?;

        let query = "format before release";
        let options = super::ContextPackOptions {
            workspace_path: workspace,
            database_path: Some(db_path.clone()),
            index_dir: Some(empty_index_dir),
            query: query.to_owned(),
            speed: crate::search::SpeedMode::Default,
            source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
        };

        let expected_message = "caller cancelled immediately after pack persistence";
        let hook_result = Arc::new(Mutex::new(None));
        let hook_result_for_hook = Arc::clone(&hook_result);
        super::install_after_pack_persistence_hook(move |cx, succeeded| {
            if let Ok(mut observed) = hook_result_for_hook.lock() {
                *observed = Some(succeeded);
            }
            cx.set_cancel_reason(CancelReason::user(expected_message));
        });

        let observation: Arc<Mutex<Option<Result<CancelReason, String>>>> =
            Arc::new(Mutex::new(None));
        let task_observation = Arc::clone(&observation);
        let mut lab =
            asupersync::LabRuntime::new(asupersync::LabConfig::new(0xEE_90E).max_steps(256));
        let root = lab.state.create_root_region(asupersync::Budget::INFINITE);
        let (task_id, _handle) = lab
            .state
            .create_task(root, asupersync::Budget::INFINITE, async move {
                let result = if let Some(cx) = Cx::current() {
                    match super::run_context_pack_with_performance_with_cx(&cx, &options, "pack")
                        .await
                    {
                        Err(super::ContextPackError::Cancelled(reason)) => Ok(reason),
                        Err(error) => Err(format!(
                            "post-persistence cancellation must remain typed, got {error:?}"
                        )),
                        Ok(run) => Err(format!(
                            "post-persistence cancellation returned success with degraded={:?}",
                            run.response.data.degraded
                        )),
                    }
                } else {
                    Err("LabRuntime pack task did not install a Cx".to_owned())
                };
                if let Ok(mut slot) = task_observation.lock() {
                    *slot = Some(result);
                }
                asupersync::Outcome::<(), String>::Ok(())
            })
            .map_err(|error| format!("create post-persistence cancellation task: {error}"))?;
        lab.scheduler.lock().schedule(task_id, 0);

        let report = lab.run_until_quiescent_with_report();
        assert!(
            report.quiescent,
            "pack cancellation LabRuntime must quiesce"
        );
        assert!(
            report.invariant_violations.is_empty(),
            "pack cancellation must preserve LabRuntime invariants: {:?}",
            report.invariant_violations
        );
        assert_eq!(
            hook_result
                .lock()
                .map_err(|_| "pack persistence hook observation poisoned".to_owned())?
                .take(),
            Some(true),
            "test hook must observe an atomically committed pack record"
        );
        let reason = observation
            .lock()
            .map_err(|_| "pack cancellation observation poisoned".to_owned())?
            .take()
            .ok_or_else(|| "pack cancellation observation missing".to_owned())??;
        assert_eq!(reason.kind, asupersync::CancelKind::User);
        assert_eq!(reason.message.as_deref(), Some(expected_message));

        let connection = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        let record = connection
            .get_latest_pack_record_for_query(&workspace_id, query)
            .map_err(|error| error.to_string())?;
        assert!(
            record.is_some(),
            "the completed atomic pack transaction must remain durable"
        );
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
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
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
    fn context_snapshot_pin_metadata_hashes_query_without_raw_text() -> Result<(), String> {
        let request =
            ContextRequest::from_query("investigate forbidden dependencies and API tokens")
                .map_err(|error| error.to_string())?;

        let metadata = super::context_snapshot_pin_metadata(&request);
        let request_id = metadata
            .request_id
            .as_deref()
            .ok_or_else(|| "context snapshot metadata missing request id".to_string())?;

        assert_eq!(metadata.workflow_id.as_deref(), Some("context"));
        assert_eq!(
            request_id,
            crate::obs::audit_events::query_hash(&request.query)
        );
        assert!(request_id.starts_with("blake3:"));
        assert!(!request_id.contains("forbidden"));
        assert!(!request_id.contains("tokens"));
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
            "[storage.read_pool]\nsize = 2\nidle_timeout_seconds = 11\nmax_pin_duration_seconds = 7\nacquire_timeout_ms = 250\npin_snapshot = true\n",
        )
        .map_err(|error| error.to_string())?;

        let mut degraded = Vec::new();
        let (config, pin_snapshot) = super::context_read_pool_config(&workspace, &mut degraded);

        assert!(degraded.is_empty());
        assert!(pin_snapshot);
        assert_eq!(config.max_size(), 2);
        assert_eq!(config.idle_timeout(), Duration::from_secs(11));
        assert_eq!(config.max_pin_duration(), Duration::from_secs(7));
        assert_eq!(config.acquire_timeout(), Duration::from_millis(250));
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

    /// Regression guard for the bounded-read defense in
    /// `context_workspace_config`. Pre-fix the helper called
    /// `read_context_file_to_string_no_follow` on `.ee/config.toml` with no
    /// size guard, so a peer-planted multi-MiB config would pin a matching
    /// allocation on every `ee pack` invocation through eight distinct
    /// sub-paths (Pack DNA, L2 pack cache, PPR rerank, memory-tier
    /// admission, adaptive pack budget, read-pool snapshot pin,
    /// proximity-to-seed scoring, PPR weight). Same defect class that
    /// e1499deb closed for the parallel `ee remember` hot path.
    #[test]
    fn context_workspace_config_rejects_oversize_config_file() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let config_path = ee_dir.join("config.toml");
        let cap = usize::try_from(super::CONTEXT_WORKSPACE_CONFIG_MAX_BYTES)
            .map_err(|error| format!("cap fits in usize: {error}"))?;
        let mut payload = String::with_capacity(cap + 1);
        while payload.len() <= cap {
            payload.push('#');
        }
        std::fs::write(&config_path, &payload).map_err(|error| error.to_string())?;

        let error = super::context_workspace_config(&workspace, "test context config")
            .expect_err("oversize context config must be rejected before unbounded allocation");

        assert!(
            error.contains("exceeding the"),
            "rejection message must cite the ceiling; got: {error}"
        );
        assert!(
            error.contains(&super::CONTEXT_WORKSPACE_CONFIG_MAX_BYTES.to_string()),
            "rejection message must name the cap constant; got: {error}"
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
            acquire_timeout_ms: Some(19),
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
    fn context_read_pool_degradations_emit_acquire_timeout() -> Result<(), String> {
        let mut degraded = Vec::new();
        let stats = PoolStats {
            ad_hoc_bypass_count: 2,
            ..PoolStats::default()
        };

        super::push_context_read_pool_degradations(&mut degraded, &stats, 2);

        ensure_equal(&degraded.len(), &1, "degraded count")?;
        ensure_equal(
            &degraded[0].code,
            &"read_pool_acquire_timeout".to_string(),
            "degraded code",
        )?;
        ensure_equal(
            &degraded[0].severity,
            &ContextResponseSeverity::Medium,
            "degraded severity",
        )?;
        Ok(())
    }

    #[test]
    fn context_read_pool_degradations_ignore_prior_ad_hoc_bypasses() -> Result<(), String> {
        let mut degraded = Vec::new();
        let stats = PoolStats {
            ad_hoc_bypass_count: 2,
            ..PoolStats::default()
        };

        super::push_context_read_pool_degradations(&mut degraded, &stats, 0);

        ensure_equal(&degraded.len(), &0, "degraded count")?;
        Ok(())
    }

    #[test]
    fn context_read_pool_degradations_emit_undersized_after_full_window() -> Result<(), String> {
        let mut degraded = Vec::new();
        let stats = PoolStats {
            acquire_wait: AcquireWaitStats {
                samples: READ_POOL_UNDERSIZED_SAMPLE_FLOOR,
                p50_ns: 1,
                p99_ns: READ_POOL_UNDERSIZED_P99_THRESHOLD.as_nanos(),
            },
            ..PoolStats::default()
        };

        super::push_context_read_pool_degradations(&mut degraded, &stats, 0);

        ensure_equal(&degraded.len(), &1, "degraded count")?;
        ensure_equal(
            &degraded[0].code,
            &"read_pool_undersized".to_string(),
            "degraded code",
        )?;
        ensure_equal(
            &degraded[0].severity,
            &ContextResponseSeverity::Low,
            "degraded severity",
        )?;
        Ok(())
    }

    #[test]
    fn context_read_pool_degradations_wait_for_full_sample_window() -> Result<(), String> {
        let mut degraded = Vec::new();
        let stats = PoolStats {
            acquire_wait: AcquireWaitStats {
                samples: READ_POOL_UNDERSIZED_SAMPLE_FLOOR - 1,
                p50_ns: 1,
                p99_ns: READ_POOL_UNDERSIZED_P99_THRESHOLD.as_nanos(),
            },
            ..PoolStats::default()
        };

        super::push_context_read_pool_degradations(&mut degraded, &stats, 0);

        ensure_equal(&degraded.len(), &0, "degraded count")?;
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "snapshot provenance release original".to_owned(),
            requested_limit: 10,
            results: hits,
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: 50,
            rerank_runtime_available: false,
            relevance_floor_applied: Some(0.0),
            candidates_below_floor: 0,
            query_assist: None,
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
            None,
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
    fn lexical_fallback_metadata_redacts_sensitive_provenance_uri() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = super::stable_context_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("fallback metadata redaction".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(46)).to_string();
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Fallback provenance redaction protects local paths.".to_owned(),
                    workflow_id: None,
                    confidence: 0.95,
                    utility: 0.80,
                    importance: 0.70,
                    provenance_uri: Some(
                        "file:///Users/alice/private/repo/notes.md?api_key=sk-FAKEabc123def456ghi789"
                            .to_owned(),
                    ),
                    trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["release".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let mut degraded = Vec::new();
        let hits = super::lexical_memory_fallback_hits(
            &connection,
            &workspace,
            "fallback provenance redaction",
            10,
            false,
            None,
            false,
            false,
            false,
            &mut degraded,
        );
        let hit = hits
            .iter()
            .find(|hit| hit.doc_id == memory_id)
            .ok_or_else(|| format!("expected fallback hit for {memory_id}, got {hits:?}"))?;
        let provenance_uri = hit
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("provenanceUri"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "fallback metadata missing provenanceUri".to_owned())?;

        assert!(
            provenance_uri.contains("[REDACTED_PATH]"),
            "fallback provenance should redact local paths: {provenance_uri}"
        );
        assert!(
            provenance_uri.contains("[REDACTED:"),
            "fallback provenance should redact secret-like query values: {provenance_uri}"
        );
        assert!(!provenance_uri.contains("/Users/alice/private/repo"));
        assert!(!provenance_uri.contains("sk-FAKEabc123def456ghi789"));
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
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
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
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
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
            relevance_floor: None,
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            ppr_weight: None,
            changed_symbols: Vec::new(),
            changed_symbols_from_git: false,
            pagination: None,
            coordination_snapshot_path: None,
            coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
            task_lens: None,
            require_fresh_sentinels: false,
            output_options: Default::default(),
            persist_pack: true,
            baseline_write: None,
            no_lod: false,
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
    fn candidate_selection_why_labels_rule_provenance_bd_3h6bz() {
        // A rule hit hydrates through a source memory; the why line must
        // attribute the applied rule, not mislabel it as an artifact.
        let why = candidate_selection_why(
            "prepare release",
            "hybrid",
            0.912_34,
            0.556_78,
            Some("rule_0123456789abcdef01234567"),
        );
        assert_eq!(
            why,
            "matched 'prepare release' via hybrid (relevance 0.9123, utility 0.5568); via applied procedural rule rule_0123456789abcdef01234567"
        );
    }

    #[test]
    fn candidate_selection_why_labels_evidence_provenance_bd_16imy() {
        // An imported-evidence hit hydrates through its distilled memory;
        // the why line must attribute the evidence span, not an artifact.
        let why = candidate_selection_why(
            "prepare release",
            "hybrid",
            0.912_34,
            0.556_78,
            Some("ev_01234567890123456789012345"),
        );
        assert_eq!(
            why,
            "matched 'prepare release' via hybrid (relevance 0.9123, utility 0.5568); via imported evidence ev_01234567890123456789012345"
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
    fn selected_context_memory_drift_degradation_reports_highest_risk_item() -> Result<(), String> {
        use crate::pack::{
            PackDraft, PackDraftItem, PackSelectionAudit, PackSelectionObjective,
            PackSelectionPhase,
        };

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_01234567890123456789033333";
        let workspace_path = Path::new("/tmp/ee-context-memory-drift");
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.display().to_string(),
                    name: Some("context memory drift".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let changed_id = MemoryId::from_uuid(uuid::Uuid::from_u128(3301));
        let missing_id = MemoryId::from_uuid(uuid::Uuid::from_u128(3302));
        for (memory_id, content) in [
            (
                changed_id.to_string(),
                "Changed provenance should be reported.".to_string(),
            ),
            (
                missing_id.to_string(),
                "Missing provenance should outrank changed provenance.".to_string(),
            ),
        ] {
            connection
                .insert_memory(
                    &memory_id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.to_string(),
                        level: "procedural".to_string(),
                        kind: "rule".to_string(),
                        content,
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        provenance_uri: Some("file://AGENTS.md#L1".to_string()),
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
            .execute_raw(&format!(
                "UPDATE memories SET provenance_verification_status = 'mismatch', provenance_chain_hash = 'blake3:changed' WHERE id = '{}'",
                changed_id
            ))
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!(
                "UPDATE memories SET provenance_verification_status = 'missing', provenance_chain_hash = 'blake3:missing' WHERE id = '{}'",
                missing_id
            ))
            .map_err(|error| error.to_string())?;

        fn draft_item(
            rank: usize,
            memory_id: MemoryId,
            content: &str,
        ) -> Result<PackDraftItem, String> {
            let rank = u32::try_from(rank).map_err(|_| format!("rank {rank} overflows u32"))?;
            Ok(PackDraftItem {
                rank,
                memory_id,
                section: PackSection::ProceduralRules,
                content: content.to_string(),
                estimated_tokens: 8,
                relevance: UnitScore::parse(0.8).map_err(|error| error.to_string())?,
                utility: UnitScore::parse(0.7).map_err(|error| error.to_string())?,
                proximity_to_seed: None,
                score_breakdown: None,
                attempt_family_multiplicity: None,
                provenance: vec![
                    PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "test source")
                        .map_err(|error| error.to_string())?,
                ],
                why: "selected for drift test".to_string(),
                diversity_key: None,
                trust: crate::pack::PackTrustSignal::new(TrustClass::AgentAssertion, None),
                redactions: Vec::new(),
                tombstoned_at: None,
                lifecycle: None,
                freshness_facets: Vec::new(),
                selected_in: PackSelectionPhase::StrictMmr,
            })
        }

        let budget = TokenBudget::default_context();
        let mut draft = PackDraft {
            query: "memory drift".to_string(),
            budget,
            used_tokens: 16,
            items: vec![
                draft_item(1, changed_id, "Changed provenance should be reported.")?,
                draft_item(
                    2,
                    missing_id,
                    "Missing provenance should outrank changed provenance.",
                )?,
            ],
            evidence_items: Vec::new(),
            omitted: Vec::new(),
            selection_audit: PackSelectionAudit {
                profile: ContextPackProfile::Balanced,
                objective: PackSelectionObjective::MmrRedundancy,
                algorithm_id: "test_drift_selection",
                algorithm_description: "Test-only context drift selection audit.",
                candidate_count: 2,
                selected_count: 2,
                omitted_count: 0,
                budget_limit: budget.max_tokens(),
                budget_used: 16,
                total_objective_value: 1.0,
                monotone: false,
                submodular: false,
                selected_items: Vec::new(),
                steps: Vec::new(),
            },
            hash: None,
        };
        let original_order = draft
            .items
            .iter()
            .map(|item| item.memory_id.to_string())
            .collect::<Vec<_>>();

        let mut degraded = Vec::new();
        super::push_selected_context_memory_drift_degradations(
            &connection,
            workspace_path,
            &mut draft,
            &mut degraded,
        );

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "memory_drift_source_missing");
        assert_eq!(degraded[0].severity, ContextResponseSeverity::High);
        let expected_repair = format!("ee memory drift {missing_id} --json");
        assert_eq!(
            degraded[0].repair.as_deref(),
            Some(expected_repair.as_str())
        );
        assert!(
            degraded[0]
                .message
                .contains("highest-risk status=missing_source")
        );
        assert!(
            degraded[0]
                .message
                .contains("reason=provenance_chain_missing")
        );
        assert!(degraded[0].message.contains("evidenceCount=1"));
        assert_eq!(draft.items[0].freshness_facets.len(), 1);
        assert_eq!(draft.items[0].freshness_facets[0].kind, "memory_drift");
        assert_eq!(draft.items[0].freshness_facets[0].freshness, "drifted");
        assert_eq!(draft.items[1].freshness_facets.len(), 1);
        assert_eq!(draft.items[1].freshness_facets[0].kind, "memory_drift");
        assert_eq!(draft.items[1].freshness_facets[0].freshness, "missing");
        assert_eq!(
            draft
                .items
                .iter()
                .map(|item| item.memory_id.to_string())
                .collect::<Vec<_>>(),
            original_order
        );

        connection.close().map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn authoritative_batch_annotation_drives_selected_and_rejected_pack_scores()
    -> Result<(), String> {
        use crate::pack::{
            PackCandidate, PackCandidateInput, PackProvenance, PackScoreBreakdown, PackSection,
        };

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(8_100)).to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-attempt-family-pack".to_owned(),
                    name: Some("attempt family pack".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let selected_id = MemoryId::from_uuid(uuid::Uuid::from_u128(8_101));
        let rejected_id = MemoryId::from_uuid(uuid::Uuid::from_u128(8_102));
        for (memory_id, content) in [
            (selected_id, "selected attempt"),
            (rejected_id, "rejected attempt"),
        ] {
            connection
                .insert_memory(
                    &memory_id.to_string(),
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "working".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.6,
                        importance: 0.5,
                        provenance_uri: None,
                        trust_class: TrustClass::AgentAssertion.as_str().to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        const RAW_FAMILY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
        for (memory_id, attempt_index, disposition) in
            [(selected_id, 1, "selected"), (rejected_id, 2, "rejected")]
        {
            connection
                .set_memory_attempt_family(
                    &memory_id.to_string(),
                    &crate::db::MemoryAttemptFamily {
                        family_id: RAW_FAMILY_ID.to_owned(),
                        declared_size: Some(3),
                        attempt_index: Some(attempt_index),
                        disposition: Some(disposition.to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let candidate = |memory_id: MemoryId| -> Result<PackCandidate, String> {
            PackCandidate::new(PackCandidateInput {
                memory_id,
                section: PackSection::Evidence,
                content: "attempt-family evidence".to_owned(),
                estimated_tokens: 8,
                relevance: UnitScore::parse(0.9).map_err(|error| error.to_string())?,
                utility: UnitScore::parse(0.6).map_err(|error| error.to_string())?,
                provenance: vec![
                    PackProvenance::new(
                        ProvenanceUri::EeMemory(memory_id),
                        "authoritative attempt-family ledger",
                    )
                    .map_err(|error| error.to_string())?,
                ],
                why: "candidate received every upstream boost".to_owned(),
            })
            .map(|candidate| {
                candidate.with_score_breakdown(PackScoreBreakdown::ppr(0.8, 0.7, 0.75))
            })
            .map_err(|error| error.to_string())
        };
        let mut candidates = vec![candidate(selected_id)?, candidate(rejected_id)?];
        connection
            .begin_read_snapshot()
            .map_err(|error| error.to_string())?;
        super::annotate_attempt_family_multiplicity_in_current_snapshot(
            &connection,
            &mut candidates,
        )
        .map_err(|error| error.to_string())?;
        super::apply_attempt_family_multiplicity_discount(&mut candidates)
            .map_err(|error| error.to_string())?;

        let selected_snapshot = candidates[0]
            .attempt_family_multiplicity
            .as_ref()
            .ok_or_else(|| "selected authoritative snapshot missing".to_owned())?;
        let rejected_snapshot = candidates[1]
            .attempt_family_multiplicity
            .as_ref()
            .ok_or_else(|| "rejected authoritative snapshot missing".to_owned())?;
        assert_eq!(selected_snapshot.promotion_posture, "blocked_incomplete");
        assert_eq!(selected_snapshot.effective_discount_factor, 1.0 / 3.0);
        assert_eq!(rejected_snapshot.effective_discount_factor, 1.0);
        assert_eq!(
            selected_snapshot.memberships[0].member_disposition,
            "selected"
        );
        assert_eq!(
            rejected_snapshot.memberships[0].member_disposition,
            "rejected"
        );
        assert!((candidates[0].relevance.into_inner() - 0.3).abs() < 1.0e-7);
        assert!((candidates[0].utility.into_inner() - 0.2).abs() < 1.0e-7);
        assert!(
            (candidates[0]
                .score_breakdown
                .ok_or_else(|| "selected combined score missing".to_owned())?
                .combined_score
                - 0.25)
                .abs()
                < 1.0e-7
        );
        assert_eq!(candidates[1].relevance.into_inner(), 0.9);
        assert_eq!(candidates[1].utility.into_inner(), 0.6);
        assert_eq!(
            candidates[1]
                .score_breakdown
                .ok_or_else(|| "rejected combined score missing".to_owned())?
                .combined_score,
            0.75
        );
        let public_snapshot = super::pack_attempt_family_multiplicity_json(selected_snapshot);
        assert!(!public_snapshot.to_string().contains(RAW_FAMILY_ID));
        assert_eq!(
            public_snapshot["memberships"][0]["familyAlias"],
            serde_json::json!(crate::models::public_attempt_family_alias(RAW_FAMILY_ID))
        );
        connection
            .rollback_read_snapshot()
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn multiplicity_discount_scales_final_selected_scores_but_not_rejected_evidence()
    -> Result<(), String> {
        use crate::models::{MemoryId, ProvenanceUri, UnitScore};
        use crate::pack::{
            PACK_ATTEMPT_FAMILY_MULTIPLICITY_SCHEMA_V1, PackAttemptFamilyMembershipSnapshot,
            PackAttemptFamilyMultiplicitySnapshot, PackCandidate, PackCandidateInput,
            PackProvenance, PackScoreBreakdown, PackSection,
        };

        let candidate = |seed: u128| -> Result<PackCandidate, String> {
            PackCandidate::new(PackCandidateInput {
                memory_id: MemoryId::from_uuid(uuid::Uuid::from_u128(seed)),
                section: PackSection::Evidence,
                content: "attempt-family evidence".to_owned(),
                estimated_tokens: 8,
                relevance: UnitScore::parse(0.9).map_err(|error| error.to_string())?,
                utility: UnitScore::parse(0.6).map_err(|error| error.to_string())?,
                provenance: vec![
                    PackProvenance::new(
                        ProvenanceUri::from_str("manual://attempt-family-ranking")
                            .map_err(|error| error.to_string())?,
                        "ranking fixture",
                    )
                    .map_err(|error| error.to_string())?,
                ],
                why: "candidate received every upstream boost".to_owned(),
            })
            .map(|candidate| {
                candidate.with_score_breakdown(PackScoreBreakdown::ppr(0.8, 0.7, 0.75))
            })
            .map_err(|error| error.to_string())
        };
        let snapshot = |disposition: &str, factor: f32| PackAttemptFamilyMultiplicitySnapshot {
            schema: PACK_ATTEMPT_FAMILY_MULTIPLICITY_SCHEMA_V1,
            effective_discount_factor: factor,
            promotion_posture: "blocked_incomplete".to_owned(),
            promotion_reason: "not every declared attempt slot is recorded".to_owned(),
            memberships: vec![PackAttemptFamilyMembershipSnapshot {
                family_alias: "afm_0123456789abcdef0123456789abcdef".to_owned(),
                member_disposition: disposition.to_owned(),
                member_discount_factor: factor,
                declared_size: Some(3),
                recorded_slots: 2,
                selected_count: 1,
                rejected_count: 1,
                unslotted_count: 0,
                duplicate_slot_count: 0,
                duplicate_member_count: 0,
                out_of_range_slot_count: 0,
                unrecorded_count: 1,
                promotion_posture: "blocked_incomplete".to_owned(),
                promotion_reason: "not every declared attempt slot is recorded".to_owned(),
            }],
        };

        let mut selected = candidate(8001)?;
        selected.attempt_family_multiplicity = Some(snapshot("selected", 1.0 / 3.0));
        let mut rejected = candidate(8002)?;
        rejected.attempt_family_multiplicity = Some(snapshot("rejected", 1.0));
        let mut candidates = vec![selected, rejected];
        super::apply_attempt_family_multiplicity_discount(&mut candidates)
            .map_err(|error| error.to_string())?;

        assert!((candidates[0].relevance.into_inner() - 0.3).abs() < 1.0e-7);
        assert!((candidates[0].utility.into_inner() - 0.2).abs() < 1.0e-7);
        assert!(
            (candidates[0]
                .score_breakdown
                .ok_or_else(|| "selected score breakdown missing".to_owned())?
                .combined_score
                - 0.25)
                .abs()
                < 1.0e-7
        );
        assert!((candidates[1].relevance.into_inner() - 0.9).abs() < f32::EPSILON);
        assert!((candidates[1].utility.into_inner() - 0.6).abs() < f32::EPSILON);
        assert_eq!(
            candidates[1]
                .score_breakdown
                .ok_or_else(|| "rejected score breakdown missing".to_owned())?
                .combined_score,
            0.75
        );
        Ok(())
    }

    #[test]
    fn pack_hash_includes_content_provenance_and_degradation() -> Result<(), String> {
        use super::{
            ContextPackOutputOptions, ContextPackOutputProfile, ContextResponseDegradation,
            ContextResponseSeverity, ContextTaskLens, compute_pack_hash,
            compute_pack_hash_with_output_options,
            compute_pack_hash_with_output_options_coordination_and_snapshot,
            compute_pack_hash_with_output_options_coordination_snapshot_and_lens,
        };
        use crate::models::{ProvenanceUri, TrustClass, UnitScore};
        use crate::pack::{
            ContextRequest, DEFAULT_COORDINATION_STALE_AFTER_MS,
            PACK_ATTEMPT_FAMILY_MULTIPLICITY_SCHEMA_V1, PackAttemptFamilyMembershipSnapshot,
            PackAttemptFamilyMultiplicitySnapshot, PackCoordinationSnapshot, PackDraft,
            PackDraftItem, PackOmission, PackOmissionReason, PackProvenance, PackRejectionStage,
            PackSection, PackSelectionAudit, PackSelectionObjective, PackSelectionPhase,
            PackTrustSignal, TokenBudget,
        };

        let request =
            ContextRequest::from_query("test query").map_err(|error| error.to_string())?;

        let mem_a = MemoryId::from_uuid(uuid::Uuid::from_u128(1));
        let mem_b = MemoryId::from_uuid(uuid::Uuid::from_u128(2));
        let mem_c = MemoryId::from_uuid(uuid::Uuid::from_u128(3));
        let mem_d = MemoryId::from_uuid(uuid::Uuid::from_u128(4));
        let budget = TokenBudget::default_context();
        let multiplicity_snapshot = PackAttemptFamilyMultiplicitySnapshot {
            schema: PACK_ATTEMPT_FAMILY_MULTIPLICITY_SCHEMA_V1,
            effective_discount_factor: 1.0 / 3.0,
            promotion_posture: "blocked_incomplete".to_owned(),
            promotion_reason: "not every declared attempt slot is recorded".to_owned(),
            memberships: vec![PackAttemptFamilyMembershipSnapshot {
                family_alias: "afm_0123456789abcdef0123456789abcdef".to_owned(),
                member_disposition: "selected".to_owned(),
                member_discount_factor: 1.0 / 3.0,
                declared_size: Some(3),
                recorded_slots: 1,
                selected_count: 1,
                rejected_count: 0,
                unslotted_count: 0,
                duplicate_slot_count: 0,
                duplicate_member_count: 0,
                out_of_range_slot_count: 0,
                unrecorded_count: 2,
                promotion_posture: "blocked_incomplete".to_owned(),
                promotion_reason: "not every declared attempt slot is recorded".to_owned(),
            }],
        };

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
            attempt_family_multiplicity: None,
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
            freshness_facets: Vec::new(),
            selected_in: PackSelectionPhase::StrictMmr,
        };

        let base_draft = PackDraft {
            query: "test query".to_string(),
            budget,
            used_tokens: 10,
            items: vec![base_item.clone()],
            evidence_items: Vec::new(),
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
        let hash_snapshot_generation_one =
            compute_pack_hash_with_output_options_coordination_and_snapshot(
                &request,
                &base_draft,
                &base_degraded,
                ContextPackOutputOptions::default(),
                None,
                Some(1),
            );
        let hash_snapshot_generation_two =
            compute_pack_hash_with_output_options_coordination_and_snapshot(
                &request,
                &base_draft,
                &base_degraded,
                ContextPackOutputOptions::default(),
                None,
                Some(2),
            );
        assert_ne!(
            hash_base, hash_snapshot_generation_one,
            "pack hash must include pinned read snapshot generation"
        );
        assert_ne!(
            hash_snapshot_generation_one, hash_snapshot_generation_two,
            "different read snapshot generations must produce different pack hashes"
        );
        assert_eq!(
            hash_snapshot_generation_one,
            compute_pack_hash_with_output_options_coordination_and_snapshot(
                &request,
                &base_draft,
                &base_degraded,
                ContextPackOutputOptions::default(),
                None,
                Some(1),
            ),
            "fixed read snapshot generation must reproduce"
        );
        let coordination_a = PackCoordinationSnapshot::from_json_str(
            r#"{"schema":"ee.coordination_snapshot.v1","capturedAt":"2026-06-01T00:00:00Z","scope":"workspace","sources":[]}"#,
            DEFAULT_COORDINATION_STALE_AFTER_MS,
        )?;
        let coordination_b = PackCoordinationSnapshot::from_json_str(
            r#"{"schema":"ee.coordination_snapshot.v1","capturedAt":"2026-06-02T00:00:00Z","scope":"workspace","sources":[]}"#,
            DEFAULT_COORDINATION_STALE_AFTER_MS,
        )?;
        let hash_coordination_a = compute_pack_hash_with_output_options_coordination_and_snapshot(
            &request,
            &base_draft,
            &base_degraded,
            ContextPackOutputOptions::default(),
            Some(&coordination_a),
            None,
        );
        let hash_coordination_b = compute_pack_hash_with_output_options_coordination_and_snapshot(
            &request,
            &base_draft,
            &base_degraded,
            ContextPackOutputOptions::default(),
            Some(&coordination_b),
            None,
        );
        assert_ne!(
            hash_base, hash_coordination_a,
            "pack hash must include coordination snapshot bytes"
        );
        assert_ne!(
            hash_coordination_a, hash_coordination_b,
            "different coordination snapshots must produce different pack hashes"
        );
        assert_eq!(
            hash_coordination_a,
            compute_pack_hash_with_output_options_coordination_and_snapshot(
                &request,
                &base_draft,
                &base_degraded,
                ContextPackOutputOptions::default(),
                Some(&coordination_a),
                None,
            ),
            "fixed coordination snapshot must reproduce"
        );
        let hash_task_lens_a = compute_pack_hash_with_output_options_coordination_snapshot_and_lens(
            &request,
            &base_draft,
            &base_degraded,
            ContextPackOutputOptions::default(),
            None,
            None,
            Some(&ContextTaskLens {
                id: "bugfix".to_string(),
                version: 1,
                lens_hash: "blake3:task-lens-a".to_string(),
            }),
        );
        let hash_task_lens_b = compute_pack_hash_with_output_options_coordination_snapshot_and_lens(
            &request,
            &base_draft,
            &base_degraded,
            ContextPackOutputOptions::default(),
            None,
            None,
            Some(&ContextTaskLens {
                id: "bugfix".to_string(),
                version: 2,
                lens_hash: "blake3:task-lens-b".to_string(),
            }),
        );
        assert_ne!(
            hash_base, hash_task_lens_a,
            "pack hash must include task lens identity"
        );
        assert_ne!(
            hash_task_lens_a, hash_task_lens_b,
            "pack hash must include task lens version and hash"
        );
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

        let mut draft_selected_multiplicity = base_draft.clone();
        draft_selected_multiplicity.items[0].attempt_family_multiplicity =
            Some(multiplicity_snapshot.clone());
        let hash_selected_multiplicity =
            compute_pack_hash(&request, &draft_selected_multiplicity, &base_degraded);
        assert_ne!(
            hash_base, hash_selected_multiplicity,
            "selected multiplicity snapshot must alter hash"
        );

        // Different omissions produce different hash.
        let mut draft_omission = base_draft.clone();
        draft_omission.omitted = vec![PackOmission {
            memory_id: mem_d,
            estimated_tokens: 50,
            relevance: UnitScore::parse(0.5).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.4).map_err(|error| error.to_string())?,
            attempt_family_multiplicity: None,
            reason: PackOmissionReason::TokenBudgetExceeded,
            rejected_at: PackRejectionStage::Selection,
            feasible: false,
            could_fit_with_budget: Some(60),
        }];
        let hash_omission = compute_pack_hash(&request, &draft_omission, &base_degraded);
        assert_ne!(hash_base, hash_omission, "omission change must alter hash");
        let mut draft_omission_multiplicity = draft_omission.clone();
        draft_omission_multiplicity.omitted[0].attempt_family_multiplicity =
            Some(multiplicity_snapshot);
        let hash_omission_multiplicity =
            compute_pack_hash(&request, &draft_omission_multiplicity, &base_degraded);
        assert_ne!(
            hash_omission, hash_omission_multiplicity,
            "omitted multiplicity snapshot must alter hash"
        );

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
    fn pack_hash_refresh_includes_late_context_degradation() -> Result<(), String> {
        use super::{
            ContextPackOutputOptions, ContextResponseDegradation, ContextResponseSeverity,
            compute_pack_hash_with_output_options_coordination_snapshot_and_lens,
            refresh_context_pack_hash,
        };
        use crate::models::{ProvenanceUri, TrustClass, UnitScore};
        use crate::pack::{
            ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
            PackSection, PackTrustSignal, TokenBudget, assemble_draft,
        };

        let request = ContextRequest::from_query("late degradation hash")
            .map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(35));
        let candidate = PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: "Late degradation must change the pack hash.".to_string(),
            estimated_tokens: 9,
            relevance: UnitScore::parse(0.95).map_err(|error| error.to_string())?,
            utility: UnitScore::parse(0.75).map_err(|error| error.to_string())?,
            provenance: vec![
                PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "hash regression source")
                    .map_err(|error| error.to_string())?,
            ],
            why: "Selected for pack hash regression coverage.".to_string(),
        })
        .map_err(|error| error.to_string())?
        .with_trust_signal(PackTrustSignal::new(TrustClass::AgentValidated, None));
        let mut draft = assemble_draft(
            "late degradation hash",
            TokenBudget::default_context(),
            [candidate],
        )
        .map_err(|error| error.to_string())?;
        let output_options = ContextPackOutputOptions::default();
        let read_snapshot_generation = Some(17);
        let initial_degraded: Vec<ContextResponseDegradation> = Vec::new();

        refresh_context_pack_hash(
            &request,
            &mut draft,
            &initial_degraded,
            output_options,
            None,
            read_snapshot_generation,
            None,
        );
        let stale_hash = draft
            .hash
            .clone()
            .ok_or_else(|| "initial hash should be assigned".to_string())?;

        let late_degraded = vec![ContextResponseDegradation {
            code: "pack_assembly_slo_breached".to_string(),
            severity: ContextResponseSeverity::Low,
            message: "Late SLO degradation changed the rendered context advisory.".to_string(),
            repair: Some(
                "Increase the resource profile or retry after the pack slot clears.".to_string(),
            ),
        }];
        refresh_context_pack_hash(
            &request,
            &mut draft,
            &late_degraded,
            output_options,
            None,
            read_snapshot_generation,
            None,
        );
        let refreshed_hash = draft
            .hash
            .clone()
            .ok_or_else(|| "refreshed hash should be assigned".to_string())?;
        let expected_hash = compute_pack_hash_with_output_options_coordination_snapshot_and_lens(
            &request,
            &draft,
            &late_degraded,
            output_options,
            None,
            read_snapshot_generation,
            None,
        );

        assert_ne!(
            stale_hash, refreshed_hash,
            "a late response degradation must alter the canonical pack hash"
        );
        assert_eq!(
            refreshed_hash, expected_hash,
            "refreshed response hash must be computed from the final degradation list"
        );
        let response = ContextResponse::new(request, draft, late_degraded)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            response.data.pack.hash.as_deref(),
            Some(refreshed_hash.as_str()),
            "final response must carry the hash for the final degraded set"
        );
        Ok(())
    }

    #[test]
    fn pack_l2_cache_key_tracks_canonical_inputs() -> Result<(), String> {
        use super::{ContextPackOutputOptions, PackL2CacheKeyInput, compute_pack_l2_cache_key};
        use crate::models::{EmbedBackend, MemoryScope, RedactionLevel};
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
            embed_backend: EmbedBackend::HashFallback,
            redaction_level: RedactionLevel::Standard,
            request,
            output_options: ContextPackOutputOptions::default()
                .with_resource_profile(PackResourceProfile::SwarmHeavy),
            include_legacy_selection_certificate: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: true,
            source_mode: crate::core::search::SearchSourceMode::Hybrid,
            strict_source_mode: false,
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

        let mut changed_embed_backend = base.clone();
        changed_embed_backend.embed_backend = EmbedBackend::NeuralLocal;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_embed_backend),
            "embedding backend changes must alter the L2 key"
        );

        let mut changed_legacy_selection_certificate = base.clone();
        changed_legacy_selection_certificate.include_legacy_selection_certificate = true;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_legacy_selection_certificate),
            "legacy selection-certificate emission must alter the L2 key"
        );

        let mut changed_source_mode = base.clone();
        changed_source_mode.source_mode = crate::core::search::SearchSourceMode::LexicalOnly;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_source_mode),
            "retrieval source mode changes must alter the L2 key"
        );

        let mut changed_strict_source = base.clone();
        changed_strict_source.strict_source_mode = true;
        assert_ne!(
            key,
            compute_pack_l2_cache_key(&changed_strict_source),
            "strict source-mode fallback policy changes must alter the L2 key"
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
