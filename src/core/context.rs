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
    index_corpus_compatibility_is_current, prepare_read_only_search_embedder_for_workspace,
    prepare_search_embedder_for_workspace,
};
use crate::core::memory_drift::{
    GitDriftProbe, MemoryDriftSelectionHint, memory_drift_selection_hint_for_memory,
};
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
#[cfg(test)]
use crate::db::StoredProceduralRule;
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
    GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE,
};
use crate::models::{
    AGENT_CONTEXT_PROFILE_SCHEMA_V1, AGENT_PROFILE_BIAS_CAP, AGENT_PROFILE_COLD_START_OUTCOMES,
    AgentContextProfileCounts, EmbedBackend, EvidenceId, GLOBAL_MEMORY_SCOPE_TAG, MemoryId,
    MemoryScope, MemoryScopeStats, MemorySentinelResultStatus, PACK_SCHEMA_V2, PackId,
    ProvenanceUri, RedactionLevel, RuleId, TrustClass, UnitScore, posture_for_trust_class,
};
use crate::pack::{
    ConflictKind, ConflictRecommendedAction, ConsensusConflictReport, ContextPackProfile,
    ContextRequest, ContextRequestInput, ContextResponse, ContextResponseDegradation,
    ContextResponsePagination, ContextResponseSeverity, PACK_ATTEMPT_FAMILY_MULTIPLICITY_SCHEMA_V1,
    PACK_COMMAND, PackAdmissionPosture, PackAssemblySlo, PackAssemblySloActuals,
    PackAttemptFamilyMembershipSnapshot, PackAttemptFamilyMultiplicitySnapshot, PackCandidate,
    PackCandidateInput, PackCoordinationSnapshot, PackDraft, PackDraftItem, PackEvidenceItem,
    PackFreshnessAnchorFacet, PackFreshnessFacet, PackItemLifecycle, PackOmission,
    PackOmissionReason, PackProvenance, PackRejectionStage, PackResourceProfile, PackSection,
    PackTrustPosture, PackTrustSignal, TokenBudget, WhyNotSelectedInput, WhyNotSelectedReport,
    assemble_draft_with_profile_and_options_seeded,
    budget_classifier::{AdaptiveBudgetDecision, AdaptiveBudgetInput, classify_adaptive_budget},
    estimate_tokens_default, explain_why_not_selected, pack_item_provenance_json,
    redact_pack_provenance_text,
};
// The production PPR rerank is cfg(test)-only until the pinned FrankenNetworkX
// release exposes deterministic personalized PageRank; the breakdown type is
// only constructed on that path.
#[cfg(test)]
use crate::pack::PackScoreBreakdown;
use crate::runtime::determinism::{Deterministic, Seed};
use crate::search::RuleIndexProjection;
use crate::util::radix_ulid_sort::sort_by_ulid_payload_or_lexical;

static PACK_HASH_LOG_RUN_INDEX: AtomicU64 = AtomicU64::new(0);
static PACK_SLOT_PROCESS_GATES: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
static CONTEXT_PROXIMITY_TREE_CACHE: OnceLock<RwLock<Option<CachedContextProximityTree>>> =
    OnceLock::new();
const PACK_SLOT_RETRY_AFTER_MS: u64 = 250;
#[allow(dead_code, reason = "staged for bd-ndzfg.3 L2 cache wiring")]
/// v7: a cached response carries its snapshot identity, so entries written
/// under pack-hash input v1 must miss rather than replay (ADR 0087 §8).
pub(crate) const PACK_L2_CACHE_KEY_SCHEMA_V7: &str = "ee.pack.l2_cache_key.v7";
const PACK_L2_CONTEXT_RESPONSE_SCHEMA_V3: &str = "ee.pack.l2_context_response.v3";
const CONTEXT_SEARCH_ADVISORY_SNAPSHOT_SCHEMA_V1: &str = "ee.context.search_advisory_snapshot.v1";
pub const DEFAULT_CONTEXT_PPR_WEIGHT: f32 = 0.30;
const GRAPH_PPR_UPSTREAM_UNAVAILABLE_MESSAGE: &str = "Personalized PageRank pack influence is unavailable because the pinned FrankenNetworkX dependency does not expose deterministic personalized PageRank; textual ranking and non-PPR graph explanations remain active.";
const GRAPH_PPR_UPSTREAM_UNAVAILABLE_REPAIR: &str = "Use --ppr-weight 0 and rely on textual ranking until a pinned FrankenNetworkX personalized PageRank API is available.";
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
    /// `--read-only` / `--no-persist` must not create `.ee/pack-slots` locks.
    /// Concurrent LimitReached otherwise empties the candidate set and forks
    /// `pack.hash` (bd-reality-core-convergence-1azkt.2).
    ///
    /// Observe existing slots through `PackAssemblySlo::admission` only.
    /// `degraded[]` is hash-bearing, so reporting contention there or emptying
    /// the candidate set would make a read-only pack depend on machine load.
    /// An unavailable observation stays `None`, not a false admission.
    Bypassed {
        admission: Option<PackAdmissionPosture>,
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

/// Observe admission without creating files, reserving a process slot, or
/// retaining a lock. This is a point-in-time observation, not a reservation.
fn probe_pack_slot_admission(
    workspace_path: &Path,
    profile: PackResourceProfile,
) -> Result<PackAdmissionPosture, String> {
    let concurrent_pack_max = profile.budget_class().concurrent_pack_max;
    let slots_dir = workspace_path.join(".ee").join("pack-slots");
    ensure_pack_slot_path_is_not_symlink(&slots_dir, "pack slot directory")?;

    let mut queue_depth = 0_usize;
    for slot_index in 0..concurrent_pack_max {
        let slot_path = slots_dir.join(format!("{}-{slot_index:02}.lock", profile.as_str()));
        ensure_pack_slot_path_is_not_symlink(&slot_path, "pack slot lock")?;
        ensure_pack_slot_path_is_regular_or_missing(&slot_path, "pack slot lock")?;
        let process_slot_held = pack_slot_process_gates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&slot_path);
        if process_slot_held {
            queue_depth = queue_depth.saturating_add(1);
            continue;
        }

        let mut options = OpenOptions::new();
        options.read(true);
        configure_pack_slot_lock_options(&mut options);
        let file = match options.open(&slot_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PackAdmissionPosture::admitted(
                    queue_depth,
                    concurrent_pack_max,
                ));
            }
            Err(error) => {
                return Err(format!(
                    "Failed to observe pack slot lock '{}': {error}",
                    slot_path.display()
                ));
            }
        };
        let metadata = file.metadata().map_err(|error| {
            format!(
                "Failed to inspect opened pack slot lock '{}': {error}",
                slot_path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "Refusing to observe pack slot lock '{}': path is not a regular file",
                slot_path.display()
            ));
        }

        // Writers hold exclusive locks. Shared probes detect those holders
        // without making simultaneous read-only observers block each other.
        // The read-only descriptor and its transient lock drop before return.
        #[cfg(unix)]
        if let Err(error) = flock(&file, FlockOperation::NonBlockingLockShared) {
            if error == Errno::WOULDBLOCK || error == Errno::AGAIN {
                queue_depth = queue_depth.saturating_add(1);
                continue;
            }
            return Err(format!(
                "Failed to probe pack slot lock '{}': {error}",
                slot_path.display()
            ));
        }
        return Ok(PackAdmissionPosture::admitted(
            queue_depth,
            concurrent_pack_max,
        ));
    }

    Ok(PackAdmissionPosture::backoff(
        queue_depth,
        concurrent_pack_max,
        PACK_SLOT_RETRY_AFTER_MS,
    ))
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

    options
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32);
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

#[path = "context_task_paths.rs"]
mod task_paths;
pub use task_paths::{
    normalize as normalize_context_task_paths, query_hash as task_paths_query_hash,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextTaskLens {
    pub id: String,
    pub version: u32,
    pub lens_hash: String,
}

#[derive(Clone, Debug)]
pub struct ContextPackOptions {
    /// Literal workspace-relative task targets for directory/file-scoped rules.
    pub task_paths: Vec<String>,
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
    /// bd-pack-compact-mode-ibksx: emit `pack.selectionAudit`.
    ///
    /// The single largest block in a pack response — `selectedItems[]` plus
    /// `steps[]` — and the main reason field reports measured pack metadata
    /// outweighing the returned memories roughly 10:1. `false` under
    /// [`ContextPackOutputProfile::Lean`].
    pub include_selection_audit: bool,
    /// bd-pack-compact-mode-ibksx: emit `pack.quality`.
    ///
    /// Walks every section plus the omission set to build its metrics, so it
    /// is both large and non-trivial to produce. `false` under `Lean`.
    pub include_quality_metrics: bool,
    /// bd-pack-compact-mode-ibksx: emit the `pack.budget.adaptiveBudget`
    /// sub-object, including `classifierContributions`.
    ///
    /// Deliberately narrower than "emit `pack.budget`": `maxTokens`,
    /// `usedTokens` and `utilization` are three scalars an agent needs in
    /// order to decide whether to re-pack, so suppressing them would cost
    /// real utility for almost no bytes. The adaptive-budget explanation is
    /// the part that is large and diagnostic. `false` under `Lean`.
    pub include_budget_detail: bool,
    /// bd-pack-compact-mode-ibksx: emit `pack.slo`.
    ///
    /// Safe to suppress only because bd-jikgj (`11f77d25a`) routed elapsed
    /// overruns into `degraded[]` via `PackAssemblySlo::timing_degradations`.
    /// Before that, `pack.slo.elapsedStatus` was the ONLY place a blown
    /// latency budget was reported, and dropping this block would have made
    /// `--compact` hide it. `false` under `Lean`.
    pub include_slo: bool,
    /// bd-r1gbq: emit the `repair` hint on each `degraded[]` entry.
    ///
    /// The field report behind bd-lexical-fallback-hint-suppression-s2c10 was
    /// that a degraded workspace repeats the same `ee index rebuild` hint on
    /// every single pack. That bead asked for per-session suppression, which
    /// cannot work: `ee` is a one-shot CLI, so there is no session to suppress
    /// across, and a persisted marker would break the AGENTS.md determinism
    /// contract (two identical invocations must be byte-identical).
    ///
    /// Suppressing by PROFILE is deterministic instead: the output is a pure
    /// function of (inputs, profile), and `refresh_context_pack_hash` already
    /// takes `ContextPackOutputOptions` as a hash input, so the hash is already
    /// profile-aware.
    ///
    /// The degraded ENTRY is never suppressed — code, severity and message are
    /// always emitted. Only the repair text is elided, and only when the caller
    /// asked for less. Demoting the entry's `DegradedCategory` to achieve this
    /// would drop the whole entry and hide that a pack came from the lexical
    /// fallback at all; see the explicit arm in `crate::pack::category_for_code`.
    /// `false` under `Lean`.
    pub include_degraded_repair_hints: bool,
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
                // bd-pack-compact-mode-ibksx: this is the whole point of the
                // Lean profile. Before this, Lean dropped only coverage_fill,
                // rendered_text and skipped[] while still emitting every heavy
                // diagnostic block, so it barely shrank the response.
                include_selection_audit: false,
                include_quality_metrics: false,
                include_budget_detail: false,
                include_slo: false,
                include_degraded_repair_hints: false,
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
                // Standard is the default profile, so these stay true: nobody
                // who did not ask for Lean sees a field disappear.
                include_selection_audit: true,
                include_quality_metrics: true,
                include_budget_detail: true,
                include_slo: true,
                include_degraded_repair_hints: true,
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
                include_selection_audit: true,
                include_quality_metrics: true,
                include_budget_detail: true,
                include_slo: true,
                include_degraded_repair_hints: true,
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
            // bd-pack-compact-mode-ibksx: carried through from the profile.
            // These have no per-call override flag of their own — `--compact`
            // selects the Lean profile rather than adding four more knobs.
            include_selection_audit: self.include_selection_audit,
            include_quality_metrics: self.include_quality_metrics,
            include_budget_detail: self.include_budget_detail,
            include_slo: self.include_slo,
            include_degraded_repair_hints: self.include_degraded_repair_hints,
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

    /// Test-only span lookup.
    ///
    /// Production deliberately does NOT source elapsed time this way -- see the
    /// note at the `observed_elapsed_ms` computation, which explains that
    /// `"total"` is not recorded yet at that point and that this answers 0 for
    /// an unknown span. Its only caller is
    /// `trace_elapsed_ms_answers_zero_for_an_unrecorded_span`, which pins that
    /// behaviour. Leaving it ungated made it dead code in every non-test build,
    /// which `-D warnings` turns into a hard error; `#[cfg(test)]` states the
    /// fact instead of suppressing the symptom.
    #[cfg(test)]
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

/// Execute the production pack path against an index built with an explicitly
/// supplied embedder. Evaluation must use the same embedder for documents and
/// queries, independently of models installed on the host.
pub(crate) fn run_context_pack_with_embedder(
    options: &ContextPackOptions,
    embedder: Arc<dyn crate::search::Embedder>,
) -> Result<ContextResponse, ContextPackError> {
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        run_context_pack_with_performance_inner(
            options,
            PACK_COMMAND,
            Deterministic::from_seed(0),
            PackRecordPersistence::Seeded,
            ContextPackControl::new(&cx, None, None),
            Some(embedder),
            None,
        )
        .await
        .map(|run| run.response)
    })
    .map_err(|error| ContextPackError::Pack(format!("Failed to start pack runtime: {error}")))?
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
    // Preserve the precise row clock for newly committed memories. The
    // storage query derives a separate canonical bound for lexical validity
    // columns, so this does not relax author expiry or supersession.
    let reference_time_text = crate::core::memory::normalize_row_timestamp(reference_time);
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
        let Some(provenance) = provenance_for_memory(
            &memory,
            memory_id,
            &options.workspace_path,
            Some(memory.workspace_id.as_str()),
            &mut degraded,
        ) else {
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

    // GH49 / bd-jikgj: reuse the request's already-open read snapshot for the
    // team roster instead of opening a second read-only connection and
    // re-querying `team_members` inside `load_team_members`. The helper
    // compares the supplied connection's file path against
    // `<workspace>/.ee/ee.db` itself, so an explicit `--database` or campaign
    // store still falls back to its own roster lookup; passing the connection
    // is therefore safe unconditionally and cannot widen scope.
    let scope_context = MemoryScopeContext::for_workspace_with_connection(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
        Some(&connection),
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
        .filter(|item| item.trust.posture() == PackTrustPosture::Authoritative)
        .map(|item| item.memory_id)
        .collect::<Vec<_>>();

    let pack_dna_ppr_requested = !query_seed_weights.is_empty();
    let input = crate::graph::pack_dna::PackDnaInput {
        pack_memory_ids,
        query_seed_weights,
        trust_anchor_memory_ids,
        ego_radius: crate::graph::pack_dna::DEFAULT_PACK_DNA_EGO_RADIUS,
        // Pack DNA remains useful without its PPR sub-signal. Keep the local
        // ACL implementation off the production pack path until the pinned
        // FrankenNetworkX dependency owns deterministic personalized PageRank.
        ppr_neighbor_limit: 0,
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
    let mut pack_dna = match compute_context_pack_dna(&projection, &input) {
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

    if pack_dna_ppr_requested {
        pack_dna
            .degraded
            .push(graph_ppr_upstream_unavailable_pack_dna_degradation());
    }

    for degradation in &pack_dna.degraded {
        if degradation.code == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE {
            push_graph_ppr_upstream_unavailable_degradation(&mut response.data.degraded);
            continue;
        }
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

/// Pack-DNA-side degradation entry for a requested-but-unavailable pack PPR.
fn graph_ppr_upstream_unavailable_pack_dna_degradation()
-> crate::graph::pack_dna::PackDnaDegradation {
    crate::graph::pack_dna::PackDnaDegradation {
        code: GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE.to_owned(),
        severity: "medium".to_owned(),
        message: GRAPH_PPR_UPSTREAM_UNAVAILABLE_MESSAGE.to_owned(),
        repair: GRAPH_PPR_UPSTREAM_UNAVAILABLE_REPAIR.to_owned(),
    }
}

/// Response-side twin of the pack-DNA entry. Idempotent: converging emit
/// paths (pack pipeline, pack-DNA attach) cannot duplicate the code.
fn push_graph_ppr_upstream_unavailable_degradation(degraded: &mut Vec<ContextResponseDegradation>) {
    if degraded
        .iter()
        .any(|entry| entry.code == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE)
    {
        return;
    }
    push_degradation(
        degraded,
        GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE,
        ContextResponseSeverity::Medium,
        GRAPH_PPR_UPSTREAM_UNAVAILABLE_MESSAGE,
        Some(GRAPH_PPR_UPSTREAM_UNAVAILABLE_REPAIR.to_owned()),
    );
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

/// Optional retrieval adapter; pack policy, budgets, hydration, and all writes
/// remain in the canonical pipeline. An unavailable adapter returns its actual
/// degradation and the pipeline performs ordinary in-process retrieval.
/// The boolean requires an already-loaded local model for a read-only semantic
/// request; a provider must not initialize or download a model when it is true.
pub(crate) type ContextSearchProvider<'a> = dyn Fn(
        &SearchOptions,
        bool,
    )
        -> Result<crate::core::search::PackSearchHandoff, crate::core::search::SearchDegradation>
    + Sync
    + 'a;

pub(crate) fn run_context_pack_with_search_provider(
    options: &ContextPackOptions,
    command: &'static str,
    provider: &ContextSearchProvider<'_>,
) -> Result<ContextPackPerformanceRun, ContextPackError> {
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        run_context_pack_with_performance_inner(
            options,
            command,
            Deterministic::from_seed(0),
            PackRecordPersistence::Ambient,
            ContextPackControl::new(&cx, None, None),
            None,
            Some(provider),
        )
        .await
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
        None,
        None,
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
    request.task_paths = task_paths::normalize(&options.workspace_path, &options.task_paths)?;
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
            None,
            None,
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
            None,
            None,
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

    let fast_embedder_override = if options.source_mode.uses_embeddings() {
        let embedder_database_path = database_path.clone();
        let preparation = crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            prepare_read_only_search_embedder_for_workspace(
                &cx,
                &options.workspace_path,
                &embedder_database_path,
            )
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
    let (candidates, _candidate_metrics) = candidates_from_search_for_task_paths(
        read_connection,
        &options.workspace_path,
        &search_report,
        &effective_filters,
        options.include_tombstoned,
        &mut degraded,
        Some(&search_preloaded_memories),
        &task_paths::normalize(&options.workspace_path, &options.task_paths)?,
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
    let bound_workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
        connection,
        &crate::core::workspace::stable_workspace_id(workspace_path),
        &[workspace_path],
    )
    .ok();
    if let Some(memory_provenance) = provenance_for_memory(
        &memory,
        memory_id,
        workspace_path,
        bound_workspace_id.as_deref(),
        degraded,
    ) {
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
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
    search_provider: Option<&ContextSearchProvider<'_>>,
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

    let index_dir = crate::config::workspace::resolve_store_index_dir(
        &options.workspace_path,
        options.database_path.as_deref(),
        options.index_dir.as_deref(),
    );
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
    // A read-only semantic handoff is allowed only through an explicit
    // already-loaded local-model capability. Readiness alone is not proof:
    // the provider must request it and the returned handoff must attest it.
    let read_only_embeddings = !options.persist_pack && options.source_mode.uses_embeddings();
    let mut remote_search = if let Some(provider) = search_provider {
        let remote_start = Instant::now();
        let result = provider(&search_options, read_only_embeddings);
        trace.record_elapsed("daemonRetrieval", remote_start);
        control.check()?;
        match result {
            Ok(handoff) if !read_only_embeddings || handoff.has_cached_local_embedder() => {
                Some(handoff)
            }
            Ok(_) => {
                push_search_degradations(
                    &mut degraded,
                    &[crate::core::search::SearchDegradation::daemon_fallback(
                        "daemon did not attest an already-loaded local semantic model",
                    )],
                );
                None
            }
            Err(fallback) => {
                push_search_degradations(&mut degraded, &[fallback]);
                None
            }
        }
    } else {
        None
    };
    if remote_search.is_none() && options.persist_pack {
        reconcile_search_index_before_read_with_cx(control.cx, &search_options, true).await;
    }
    let embedder_preparation = if remote_search.is_none()
        && fast_embedder_override.is_none()
        && options.source_mode.uses_embeddings()
        // Snapshot recovery can select a retained generation even when the
        // live directory is absent or incompatible. Always provide a safe
        // concrete model for read-only embedding requests on that path too.
        && (!options.persist_pack
            || (index_dir.exists() && index_corpus_compatibility_is_current(&index_dir)))
    {
        let preparation = if options.persist_pack {
            prepare_search_embedder_for_workspace(
                control.cx,
                &options.workspace_path,
                &database_path,
            )
            .await
        } else {
            prepare_read_only_search_embedder_for_workspace(
                control.cx,
                &options.workspace_path,
                &database_path,
            )
        }
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
    let prepared_embed_backend = if let Some(embedder) = &fast_embedder_override {
        if embedder.is_semantic() {
            EmbedBackend::NeuralLocal
        } else {
            EmbedBackend::HashFallback
        }
    } else {
        embedder_preparation
            .as_ref()
            .map_or_else(crate::core::index::active_embed_backend, |preparation| {
                preparation.backend
            })
    };

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

    if let Some(handoff) = &remote_search
        && !handoff.snapshot_matches(
            &search_options,
            checked_context_read_snapshot(&read_pool, &read_snapshot)?,
        )
    {
        // The source store changed after daemon retrieval. Release the old
        // snapshot and perform the complete canonical path once, including
        // reconciliation, without trying the daemon again.
        drop(read_snapshot);
        let mut run = Box::pin(run_context_pack_with_performance_inner(
            options,
            command,
            determinism,
            pack_record_persistence,
            control,
            fast_embedder_override,
            None,
        ))
        .await?;
        push_search_degradations(
            &mut run.response.data.degraded,
            &[crate::core::search::SearchDegradation::daemon_fallback(
                "workspace changed after daemon retrieval",
            )],
        );
        return Ok(run);
    }

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

    let l2_cache_context = if options.output_options.cache_json_response
        && fast_embedder_override.is_none()
        && search_provider.is_none()
    {
        let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
        let l2_context = context_pack_l2_prepare(
            options,
            read_connection,
            &request,
            &effective_filters,
            &runtime_profile,
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
    let mut search_report = if let Some(mut handoff) = remote_search.take() {
        search_preloaded_memories = handoff.revalidate(&search_options, read_connection);
        if let Some(connection) = &context_write_connection {
            handoff.record_audit(&search_options, connection);
        }
        handoff.report
    } else {
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
            fast_embedder_override.or_else(|| {
                embedder_preparation
                    .as_ref()
                    .map(|preparation| Arc::clone(&preparation.fast_embedder))
            }),
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
        }
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
        // bd-auto-index-rebuild-on-fallback-x35vi: record a durable rebuild
        // request so a later pack or the steward can repair the index, instead of
        // every subsequent pack paying this same degradation until an operator
        // notices. This response keeps its existing read snapshot and latency
        // budget, and it does not soften the degradation
        // below: an agent reading `degraded[]` must still know retrieval was
        // lexical-only for THIS response.
        //
        // The trigger is deliberately limited to the two causes a rebuild can
        // actually fix. A workspace serving lexical-only results because it has
        // no real embedder (deterministic hash fallback) never reaches this
        // branch, and must not: rebuilding cannot conjure a model, so requesting
        // one there would loop the steward over an index that is already fine.
        // `bd-1iupc.2` settled that distinction; `search_lexical_only` is the
        // separate status-side signal for the embedder condition.
        //
        // Gated on `persist_pack` so `--read-only` / `--no-persist` write
        // nothing, and rate-limited inside `record_index_rebuild_request`, so a
        // swarm packing in parallel cannot turn this into a write storm.
        if options.persist_pack {
            let trigger = match search_report.status {
                SearchStatus::IndexNotFound => {
                    crate::core::index::IndexRebuildTrigger::IndexMissing
                }
                _ => crate::core::index::IndexRebuildTrigger::IndexError,
            };
            let outcome = crate::core::index::record_index_rebuild_request(
                &options.workspace_path,
                trigger,
                "context_lexical_fallback",
                &chrono::Utc::now().to_rfc3339(),
                crate::core::index::DEFAULT_INDEX_REBUILD_REQUEST_COOLDOWN_SECS,
            );
            tracing::debug!(
                target: "ee::context::index_rebuild_request",
                outcome = %outcome.data_json(),
                "recorded index rebuild request for lexical fallback"
            );
        }

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

    request.task_paths = task_paths::normalize(&options.workspace_path, &options.task_paths)?;
    let candidate_start = Instant::now();
    let candidate_filter_input_count = search_report.results.len();
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let (mut candidates, mut candidate_metrics) = candidates_from_search_for_task_paths(
        read_connection,
        &options.workspace_path,
        &search_report,
        &effective_filters,
        options.include_tombstoned,
        &mut degraded,
        Some(&search_preloaded_memories),
        &request.task_paths,
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
    let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
    let scope_context = MemoryScopeContext::for_workspace_with_connection(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
        Some(read_connection),
    );
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
    apply_team_lane_pack_policy(&mut candidates, &global_store_memory_ids, &mut degraded);

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

    // Run after every memory fan-in, including graph, focus, and global lanes.
    // Stored trust and redaction opt-outs cannot authorize prompt overrides.
    let instruction_filter_input_count = candidates.len();
    let mut policy_omissions =
        filter_candidates_by_instruction_authority(&mut candidates, &mut degraded);
    if !policy_omissions.is_empty() {
        trace.filter_input_count = trace.filter_input_count.max(instruction_filter_input_count);
    }
    trace.filtered_count = trace.filtered_count.saturating_add(policy_omissions.len());
    if options.require_fresh_sentinels {
        let reference_time =
            context_validity_reference_time(options, &effective_filters).unwrap_or_else(Utc::now);
        let read_connection = checked_context_read_snapshot(&read_pool, &read_snapshot)?;
        policy_omissions.extend(filter_candidates_by_required_fresh_sentinels(
            read_connection,
            &mut candidates,
            reference_time,
            &mut degraded,
        )?);
    }
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

    let mut evidence_candidates = collect_direct_evidence_pack_candidates(
        read_connection,
        &options.workspace_path,
        &search_report,
        &request,
        &effective_filters,
        &mut degraded,
    );
    let pagination_info = apply_pagination(
        &mut candidates,
        &mut evidence_candidates,
        &options.pagination,
        request.max_results,
        &mut degraded,
    );
    candidate_metrics.subspans.scoring_ordering = scoring_ordering_start.elapsed();
    trace.record_candidate_resolution_subspans(&candidate_metrics.subspans);
    trace.candidate_resolution = candidate_metrics;
    control.check()?;

    let pack_slot_acquisition = if options.persist_pack {
        try_acquire_pack_slot(
            &options.workspace_path,
            options.output_options.resource_profile,
        )
    } else {
        let admission = match probe_pack_slot_admission(
            &options.workspace_path,
            options.output_options.resource_profile,
        ) {
            Ok(admission) => Some(admission),
            Err(message) => {
                tracing::debug!(%message, "Read-only pack slot observation unavailable");
                None
            }
        };
        PackSlotAcquisition::Bypassed { admission }
    };
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
            PackSlotAcquisition::Bypassed { admission } => (None, admission, None),
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
    if concurrent_limit_retry_after_ms.is_none() {
        append_direct_evidence_pack_items(evidence_candidates, &request, &mut draft, &mut degraded);
    }
    if !policy_omissions.is_empty() {
        let omitted_count = policy_omissions.len();
        draft.omitted.extend(policy_omissions);
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
    // GH49 / bd-jikgj: classify the SLO against the elapsed time the CALLER
    // actually waited, not against the `packAssembly` phase alone.
    //
    // `packAssembly` is a minority of a pack request — profiling on this bead
    // measured candidateConstruction ~370ms, scopeVisibility ~252ms and
    // queryAssist ~247ms against a packAssembly of 92-250ms — so a pack could
    // blow the published 2s Standard failure threshold end to end and still
    // report `elapsedStatus: within_budget`, which is precisely the defect
    // this bead was filed for.
    //
    // Measured from `total_start` rather than read back via
    // `trace.elapsed_ms("total")`: the `"total"` span is not recorded until
    // after this point, and `elapsed_ms` answers 0 for an unknown span, so the
    // by-name form would silently restore the same false `within_budget`.
    let observed_elapsed_ms = duration_millis_u64(total_start.elapsed());
    let slo = if let Some(retry_after_ms) = concurrent_limit_retry_after_ms {
        let actuals = PackAssemblySloActuals::from_pack_run(
            &draft,
            0,
            trace.candidate_resolution.graph_traversed_edges,
            observed_elapsed_ms,
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
            observed_elapsed_ms,
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
    let mut pack_hash_components = refresh_context_pack_hash(
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
        pack_hash_components = refresh_context_pack_hash(
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

    // GH49 / bd-jikgj: surface an elapsed-budget overrun in `degraded[]`, so a
    // pack that blew its published latency budget says so rather than carrying
    // it only in `slo.elapsedStatus`.
    //
    // Placed here, immediately before the response is built, because
    // `response_degraded` is the hash input for EVERY
    // `refresh_context_pack_hash` call in this function — including the second
    // one inside the persist-failure branch above. Wall-clock time is not
    // reproducible, so letting a timing entry reach any of them would make the
    // same query over the same store hash differently on a loaded machine,
    // breaking the determinism guarantee AGENTS.md calls non-negotiable. This
    // is also why it is sourced from `timing_degradations()` rather than added
    // to `slo.degradations`, which stays resource-only and deterministic.
    //
    // Persistence deliberately does not see it either: the persisted pack
    // record should match the hashed, reproducible content.
    response_degraded.extend(slo.timing_degradations());

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
    response.data.pack_hash_components = Some(pack_hash_components);
    if pagination_info.applied {
        response.data.pagination = Some(pagination_info.into_response());
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
    // A persisted call always performs its ledger and audit work. Only that
    // successful producer may populate L2; read-only calls never write or
    // repair cache entries, even when their lookup misses.
    control.check()?;
    if persist_succeeded
        && let Some(l2_context) = &l2_cache_context
        && let Some(connection) = persist_connection.as_ref()
        && context_pack_l2_database_generation(connection, Some(&l2_context.key_input.workspace_id))
            .ok()
            == Some(l2_context.key_input.database_generation)
    {
        let degraded_count_before_l2_store = response.data.degraded.len();
        context_pack_l2_store(l2_context, options, &search_report, &mut response);
        if response.data.degraded.len() != degraded_count_before_l2_store {
            // `response.data.degraded` already holds the timing entry here;
            // the v2 hash drops it by construction (ADR 0087 §5).
            let pack_hash_components = refresh_context_pack_hash(
                &response.data.request,
                &mut response.data.pack,
                &response.data.degraded,
                options.output_options,
                response.data.coordination.as_ref(),
                read_snapshot_generation,
                options.task_lens.as_ref(),
            );
            response.data.pack_hash_components = Some(pack_hash_components);
            if let Some(profile) = response.data.agent_profile.as_mut() {
                set_agent_profile_base_pack_hash(profile, response.data.pack.hash.as_deref());
            }
        }
    }
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
    let requested = crate::core::curate::stable_workspace_id(&canonical_workspace);
    let workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
        conn,
        &requested,
        &[workspace_path, canonical_workspace.as_path()],
    )
    .unwrap_or(requested);
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

/// Build the pack SLO for one run.
///
/// `observed_elapsed_ms` is the elapsed time the caller waited for the whole
/// request, and is supplied by the caller rather than looked up from `trace`
/// by span name: `ContextPerformanceTrace::elapsed_ms` answers 0 for a span it
/// does not hold, so a by-name lookup here would report a comfortable
/// `within_budget` for a request that had in fact blown its budget
/// (GH49 / bd-jikgj).
fn pack_assembly_slo_for_run(
    profile: PackResourceProfile,
    draft: &crate::pack::PackDraft,
    search_report: &SearchReport,
    trace: &ContextPerformanceTrace,
    observed_elapsed_ms: u64,
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
        observed_elapsed_ms,
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
        "resourceStatus": slo.resource_status.as_str(),
        "elapsedStatus": slo.elapsed_status.as_str(),
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
/// The search renderer also owns large-gap episode accounting. Replace that
/// advisory with its delivery-filtered view while retaining the context's
/// per-response stale-index fact. All other context degradations, including
/// transient reranker load failures, remain visible on every affected response.
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
        // The context renderer already records whether this pack used a
        // stale index. Search's once-per-episode warning suppression must not
        // erase that per-response fact. Only the large-gap advisory is
        // replaced with the session's delivery decision.
        entries.retain(|entry| {
            entry.get("code").and_then(serde_json::Value::as_str) != Some("search_index_large_gap")
        });
        for entry in &search_stale_entries {
            if !entries
                .iter()
                .any(|existing| existing.get("code") == entry.get("code"))
            {
                entries.push(entry.clone());
            }
        }
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
    // GH #49: one git probe per pack, so HEAD, commit distances, and captured
    // blobs are resolved once for all selected items instead of per item.
    let git = GitDriftProbe::new(workspace_path);
    for item in &mut draft.items {
        match connection.get_memory(&item.memory_id.to_string()) {
            Ok(Some(memory)) => {
                match memory_drift_selection_hint_for_memory(connection, &git, &memory) {
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
            // `lexicalScore` is contractually the raw engine BM25 value that
            // the normalized `relevanceScore` was projected from (README,
            // "Explainable Retrieval"). This path never reaches Frankensearch
            // — it is a direct database scan used when the index cannot serve
            // the request — so there is no BM25 value to report. Echoing the
            // coverage ratio here, as this did before
            // bd-fallback-relevance-floor-labeling-dlr6a, dressed a
            // term-overlap fraction up as an engine score and invited agents
            // to compare it against genuinely min-max-normalized BM25 pools.
            // Reporting no raw score is the truthful answer.
            lexical_score: None,
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
    }

    let requested = crate::core::workspace::stable_workspace_id(workspace_path);
    match crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[workspace_path],
    ) {
        Ok(Some(workspace)) => {
            ids.insert(workspace.id);
        }
        Ok(None) => {}
        Err(error) => push_degradation(
            degraded,
            "context_lexical_fallback_workspace_lookup_failed",
            ContextResponseSeverity::Low,
            format!(
                "Workspace lookup for {} failed: {}",
                workspace_path.display(),
                error.message()
            ),
            Some("ee doctor --json".to_owned()),
        ),
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
    crate::core::workspace::stable_workspace_id(path)
}

fn lexical_terms(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Term-coverage score for the degraded, index-free memory fallback
/// (bd-fallback-relevance-floor-labeling-dlr6a).
///
/// Two earlier behaviours made this number mean far more than it measured,
/// which is how an unrelated ticker's episodic note reached a UCU pack at
/// `0.60` and was rendered under `evidence`:
///
/// 1. The haystack was `"{level} {kind} {content}"`. `level` and `kind` are
///    structural facets, not evidence that a memory is about the query, so any
///    query containing a taxonomy word — `decision`, `rule`, `fact`, `risk`,
///    `episodic` — scored full term credit against *every* memory carrying
///    that facet. Only `content` is scored now.
/// 2. Matching was `haystack.contains(term)`, an unanchored substring test, so
///    a short query term like `ucu` matched `document` and `succumb`. Both
///    sides are now tokenized with [`lexical_terms`] and compared as whole
///    words, which is the same boundary the query itself is split on.
///
/// The result stays a plain matched/total ratio in `0.0..=1.0`. It is a
/// coverage fraction, not BM25 and not a calibrated probability — see
/// [`lexical_memory_fallback_hits`] for why no BM25 value is reported
/// alongside it.
fn lexical_memory_score(memory: &StoredMemory, query_terms: &BTreeSet<String>) -> Option<f32> {
    let content_terms = lexical_terms(&memory.content);
    let matched = query_terms
        .iter()
        .filter(|term| content_terms.contains(term.as_str()))
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
    // bd-redactor-prefix-divergence-lsy52: the eleven-prefix list that used to
    // sit here was one of twenty hand-copied copies. The shared predicate also
    // recognises Windows drive paths and UNC shares, which this walker could
    // never reach because it only scanned for '/'. The boundary stays local:
    // context renders prose, so a path ends at whitespace.
    crate::util::redact_path_like_segments(value, context_public_source_path_boundary)
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
    evidence_candidates: &mut Vec<DirectEvidencePackCandidate>,
    pagination: &Option<ContextPagination>,
    max_results: Option<u32>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> PaginationInfo {
    let Some(pagination) = pagination else {
        return PaginationInfo::default();
    };

    // The existing selector suppresses evidence represented by a selected
    // linked memory. Apply that rule to the whole paginated candidate set so
    // offsets and totals do not depend on which memory happens to be on this
    // page, and a later page cannot reintroduce its linked evidence duplicate.
    let memory_ids = candidates
        .iter()
        .map(|candidate| candidate.memory_id.to_string())
        .collect::<BTreeSet<_>>();
    evidence_candidates.retain(|candidate| {
        !candidate
            .linked_memory_id
            .as_ref()
            .is_some_and(|memory_id| memory_ids.contains(memory_id))
    });
    // Memory candidates were already query-capped before pagination. Apply
    // the remaining cap to the native tail before calculating page offsets.
    // Without pagination, selection retains its existing shared result cap.
    if let Some(max_results) = max_results {
        let evidence_limit = (max_results as usize).saturating_sub(candidates.len());
        if evidence_candidates.len() > evidence_limit {
            let trimmed = evidence_candidates.len() - evidence_limit;
            evidence_candidates.truncate(evidence_limit);
            push_direct_evidence_result_limit_degradation(degraded, trimmed);
        }
    }
    let memory_total = candidates.len();
    let total = memory_total.saturating_add(evidence_candidates.len());
    let offset = pagination.offset as usize;
    let limit = pagination.limit as usize;

    if offset >= total {
        candidates.clear();
        evidence_candidates.clear();
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
    *evidence_candidates = evidence_candidates
        .iter()
        .skip(offset.saturating_sub(memory_total))
        .take(page_size.saturating_sub(candidates.len()))
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

const TEAM_LANE_CONFLICT_DEFERRED_CODE: &str = "team_lane_conflict_deferred";
const TEAM_LANE_CONFLICT_UNASSESSED_CODE: &str = "team_lane_conflict_unassessed";
const TEAM_LANE_CORROBORATION_DEMOTE: f32 = 0.85;
const TEAM_PEER_CONFLICT_OBSERVED_AT: &str = "1970-01-01T00:00:00Z";

fn apply_team_lane_pack_policy(
    candidates: &mut [PackCandidate],
    global_store_memory_ids: &BTreeSet<String>,
    degraded: &mut Vec<ContextResponseDegradation>,
) {
    if candidates.len() < 2 {
        return;
    }
    let lane_candidates = team_lane_candidates(candidates, global_store_memory_ids);
    if !lane_candidates
        .iter()
        .any(|candidate| candidate.lane == crate::core::global_store::MemoryLane::Team)
    {
        return;
    }

    let conflicts = crate::core::global_store::surface_precedence_conflicts(&lane_candidates);
    annotate_precedence_conflicts(candidates, &conflicts);
    isolate_contradiction_diversity(candidates, &conflicts);
    demote_less_specific_corroborations(candidates, &conflicts);

    let unassessed = apply_team_peer_conflict_detector(candidates, &lane_candidates);
    push_team_lane_conflict_degradation(degraded, &conflicts);
    if unassessed > 0 {
        let suffix = if unassessed == 1 { "" } else { "s" };
        push_degradation(
            degraded,
            TEAM_LANE_CONFLICT_UNASSESSED_CODE,
            ContextResponseSeverity::Info,
            format!(
                "Team-lane peer conflict detector left {unassessed} memory{suffix} unassessed because the body is missing or sealed."
            ),
            Some(
                "Fetch or reveal the team body before treating absence of a conflict as agreement."
                    .to_string(),
            ),
        );
    }
}

fn team_lane_candidates(
    candidates: &[PackCandidate],
    global_store_memory_ids: &BTreeSet<String>,
) -> Vec<crate::core::global_store::LaneCandidate> {
    candidates
        .iter()
        .map(|candidate| crate::core::global_store::LaneCandidate {
            lane: pack_candidate_memory_lane(candidate, global_store_memory_ids),
            conflict_key: global_lane_conflict_key(candidate),
            content_hash: global_lane_content_hash(&candidate.content),
            id: candidate.memory_id.to_string(),
        })
        .collect()
}

fn pack_candidate_memory_lane(
    candidate: &PackCandidate,
    global_store_memory_ids: &BTreeSet<String>,
) -> crate::core::global_store::MemoryLane {
    let id = candidate.memory_id.to_string();
    if global_store_memory_ids.contains(&id) {
        crate::core::global_store::MemoryLane::Global
    } else if candidate.trust.class == TrustClass::PeerHumanAttested {
        crate::core::global_store::MemoryLane::Team
    } else {
        crate::core::global_store::MemoryLane::Workspace
    }
}

fn annotate_precedence_conflicts(
    candidates: &mut [PackCandidate],
    conflicts: &[crate::core::global_store::PrecedenceConflict],
) {
    if conflicts.is_empty() {
        return;
    }
    let mut by_memory_id: BTreeMap<String, Vec<&crate::core::global_store::PrecedenceConflict>> =
        BTreeMap::new();
    for conflict in conflicts {
        by_memory_id
            .entry(conflict.more_specific_id.clone())
            .or_default()
            .push(conflict);
        by_memory_id
            .entry(conflict.less_specific_id.clone())
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
                    "key={} kind={} moreSpecificLane={} moreSpecificId={} lessSpecificLane={} lessSpecificId={} bothSurfaced={} moreSpecificOverrides={}",
                    conflict.conflict_key,
                    conflict.kind.as_str(),
                    conflict.more_specific_lane.as_str(),
                    conflict.more_specific_id,
                    conflict.less_specific_lane.as_str(),
                    conflict.less_specific_id,
                    conflict.both_surfaced,
                    conflict.more_specific_overrides,
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        candidate.why = format!("{} teamLane={markers}.", candidate.why);
    }
}

fn isolate_contradiction_diversity(
    candidates: &mut [PackCandidate],
    conflicts: &[crate::core::global_store::PrecedenceConflict],
) {
    let contradiction_ids = conflicts
        .iter()
        .filter(|conflict| {
            matches!(
                conflict.kind,
                crate::core::global_store::LaneConflictKind::Contradiction
            )
        })
        .flat_map(|conflict| {
            [
                conflict.more_specific_id.as_str(),
                conflict.less_specific_id.as_str(),
            ]
        })
        .collect::<BTreeSet<_>>();
    if contradiction_ids.is_empty() {
        return;
    }
    for candidate in candidates {
        let memory_id = candidate.memory_id.to_string();
        if contradiction_ids.contains(memory_id.as_str()) {
            candidate.diversity_key = Some(format!("lane-conflict:{memory_id}"));
        }
    }
}

fn demote_less_specific_corroborations(
    candidates: &mut [PackCandidate],
    conflicts: &[crate::core::global_store::PrecedenceConflict],
) {
    let demote_ids = conflicts
        .iter()
        .filter(|conflict| conflict.more_specific_overrides)
        .map(|conflict| conflict.less_specific_id.as_str())
        .collect::<BTreeSet<_>>();
    if demote_ids.is_empty() {
        return;
    }
    for candidate in candidates {
        if !demote_ids.contains(candidate.memory_id.to_string().as_str()) {
            continue;
        }
        let demoted = candidate.relevance.into_inner() * TEAM_LANE_CORROBORATION_DEMOTE;
        if let Ok(score) = UnitScore::parse(demoted) {
            candidate.relevance = score;
        }
    }
}

fn apply_team_peer_conflict_detector(
    candidates: &mut [PackCandidate],
    lane_candidates: &[crate::core::global_store::LaneCandidate],
) -> usize {
    let mut unassessed = 0_usize;
    let mut facts = Vec::new();
    for candidate in candidates.iter() {
        if !pack_candidate_body_assessable(candidate) {
            if matches!(
                pack_candidate_lane_from_id(&candidate.memory_id.to_string(), lane_candidates),
                Some(crate::core::global_store::MemoryLane::Team)
            ) {
                unassessed = unassessed.saturating_add(1);
            }
            continue;
        }
        let memory_id = candidate.memory_id.to_string();
        facts.push(TeamPeerConflictFact {
            memory_id: memory_id.clone(),
            memory_hash: crate::core::memory::peer_conflict_hash("memory", &memory_id),
            content_hash: crate::core::memory::peer_conflict_content_hash(&candidate.content),
            content: candidate.content.clone(),
            simhash: crate::search::simhash::simhash_128(&candidate.content),
            trust_class: candidate.trust.class.as_str().to_owned(),
            lane: pack_candidate_lane_from_id(&memory_id, lane_candidates)
                .unwrap_or(crate::core::global_store::MemoryLane::Workspace),
        });
    }

    let workspace_facts = facts
        .iter()
        .filter(|fact| fact.lane == crate::core::global_store::MemoryLane::Workspace)
        .collect::<Vec<_>>();
    let team_facts = facts
        .iter()
        .filter(|fact| fact.lane == crate::core::global_store::MemoryLane::Team)
        .collect::<Vec<_>>();
    if workspace_facts.is_empty() || team_facts.is_empty() {
        return unassessed;
    }

    let workspace_hash = crate::core::memory::peer_conflict_hash("workspace", "pack");
    let options = crate::core::memory::PeerConflictDetectionOptions::new(
        &workspace_hash,
        TEAM_PEER_CONFLICT_OBSERVED_AT,
    );
    let mut events_by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut contradiction_ids = BTreeSet::new();
    for primary in &workspace_facts {
        let primary_view = crate::core::memory::PeerConflictMemory::new(
            &primary.memory_hash,
            &primary.content_hash,
            &primary.content,
            primary.simhash,
            &primary.trust_class,
        );
        let peer_views = team_facts
            .iter()
            .map(|peer| {
                crate::core::memory::PeerConflictMemory::new(
                    &peer.memory_hash,
                    &peer.content_hash,
                    &peer.content,
                    peer.simhash,
                    &peer.trust_class,
                )
            })
            .collect::<Vec<_>>();
        for event in
            crate::core::memory::detect_peer_memory_conflicts(&primary_view, &peer_views, &options)
        {
            let marker = format!(
                "kind={} verdict={} policy={}",
                event.kind, event.detector_verdict, event.rendering_policy
            );
            events_by_id
                .entry(primary.memory_id.clone())
                .or_default()
                .push(marker.clone());
            if event.detector_verdict == "contradiction" {
                contradiction_ids.insert(primary.memory_id.clone());
            }
            for peer in &team_facts {
                if event
                    .peer_memory_hashes
                    .iter()
                    .any(|hash| hash == &peer.memory_hash)
                {
                    events_by_id
                        .entry(peer.memory_id.clone())
                        .or_default()
                        .push(marker.clone());
                    if event.detector_verdict == "contradiction" {
                        contradiction_ids.insert(peer.memory_id.clone());
                    }
                }
            }
        }
    }

    for candidate in candidates.iter_mut() {
        let memory_id = candidate.memory_id.to_string();
        if let Some(markers) = events_by_id.get(&memory_id) {
            let joined = markers.join("; ");
            candidate.why = format!("{} peerConflict={joined}.", candidate.why);
        }
        if contradiction_ids.contains(&memory_id) {
            candidate.diversity_key = Some(format!("lane-conflict:{memory_id}"));
        }
    }
    unassessed
}

fn pack_candidate_lane_from_id(
    memory_id: &str,
    lane_candidates: &[crate::core::global_store::LaneCandidate],
) -> Option<crate::core::global_store::MemoryLane> {
    lane_candidates
        .iter()
        .find(|candidate| candidate.id == memory_id)
        .map(|candidate| candidate.lane)
}

fn pack_candidate_body_assessable(candidate: &PackCandidate) -> bool {
    let content = candidate.content.trim();
    !content.is_empty()
        && content != crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
        && !content.eq_ignore_ascii_case("[metadata-only]")
        && !content.starts_with("[body unavailable]")
}

fn push_team_lane_conflict_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    conflicts: &[crate::core::global_store::PrecedenceConflict],
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
        TEAM_LANE_CONFLICT_DEFERRED_CODE,
        ContextResponseSeverity::Info,
        format!(
            "Team/workspace/global lane contradiction detected for {} subject{subject_suffix} ({keys}); both sides remain in the pack candidate pool with teamLane markers and are not resolved by rank.",
            contradiction_keys.len(),
        ),
        Some(
            "Review the conflicting local and teammate memories and tombstone or revise the stale lane row."
                .to_string(),
        ),
    );
}

struct TeamPeerConflictFact {
    memory_id: String,
    memory_hash: String,
    content_hash: String,
    content: String,
    simhash: crate::search::simhash::SimHash128,
    trust_class: String,
    lane: crate::core::global_store::MemoryLane,
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
    let requested = crate::core::curate::stable_workspace_id(workspace_path);
    if let Ok(Some(workspace)) = crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[workspace_path],
    ) {
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
    let requested = crate::core::curate::stable_workspace_id(workspace_path);
    let workspace = crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[workspace_path],
    )
    .map_err(|error| format!("workspace lookup failed: {}", error.message()))?
    .ok_or_else(|| "workspace not found".to_string())?;
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
        task_paths: request.task_paths.clone(),
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

    options
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32);
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

fn context_pack_l2_path_is_definitely_absent(path: &Path) -> bool {
    matches!(
        fs::symlink_metadata(path),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            )
    )
}

fn context_pack_l2_bypass_reason(
    options: &ContextPackOptions,
    filters: &crate::models::QueryFilters,
) -> Option<&'static str> {
    if read_env_bool(EnvVar::L2PackCacheDisable) == Some(true) {
        return Some("disabled_by_environment");
    }
    if context_validity_reference_time(options, filters).is_none() {
        return Some("implicit_validity_reference_time");
    }
    if options.coordination_snapshot_path.is_some() {
        return Some("coordination_snapshot");
    }
    if !options.task_paths.is_empty() {
        return Some("task_paths_require_live_admission");
    }
    if options.task_lens.is_some() {
        return Some("task_lens");
    }
    if options.require_fresh_sentinels {
        return Some("fresh_sentinel_filter");
    }
    if options.no_lod {
        return Some("lod_disabled");
    }
    if options.changed_symbols_from_git {
        return Some("git_derived_changed_symbols");
    }
    if options.baseline_write.is_some() {
        return Some("baseline_write");
    }
    // There is no immutable store UUID in the current schema. Explicit paths
    // can address divergent stores that intentionally share a workspace ID and
    // generation, so a canonical path alone is not sufficient authorization
    // to reuse cached content for them.
    if options.database_path.is_some() {
        return Some("explicit_database_path");
    }
    if options.source_mode != SearchSourceMode::LexicalOnly {
        return Some("runtime_search_backend_state");
    }
    if crate::core::memory_scope::current_agent_name().is_some() {
        return Some("current_agent_identity");
    }
    if !cfg!(unix) {
        return Some("platform_file_identity_unavailable");
    }

    context_pack_l2_mutable_state_bypass_reason(options)
}

fn context_pack_l2_side_effect_bypass_reason(options: &ContextPackOptions) -> Option<&'static str> {
    // A cache hit returns before pack-record persistence, baseline ledger
    // writes, and the per-invocation audit. Until those side effects can be
    // replayed from a structured cache payload, write-bearing requests must
    // always assemble normally.
    if options.persist_pack {
        return Some("pack_persistence");
    }
    options.baseline_write.as_ref().map(|_| "baseline_write")
}

fn context_pack_l2_mutable_state_bypass_reason(
    options: &ContextPackOptions,
) -> Option<&'static str> {
    // These mutable files are consumed after the cache lookup. Absence is
    // rechecked on every request, so a newly-created file forces a fresh run;
    // any present, unreadable, or unsafe path bypasses replay entirely.
    let workspace_config_path = options.workspace_path.join(".ee").join("config.toml");
    if !context_pack_l2_path_is_definitely_absent(&workspace_config_path) {
        return Some("workspace_config_state");
    }
    if !context_pack_l2_path_is_definitely_absent(&focus_state_path(&options.workspace_path)) {
        return Some("focus_state");
    }

    if matches!(
        options.memory_scope,
        MemoryScope::Swarm | MemoryScope::Workspace | MemoryScope::Global
    ) {
        let Ok(paths) = crate::core::global_store::default_global_store_paths_from_env() else {
            return Some("global_store_path_unavailable");
        };
        if !context_pack_l2_path_is_definitely_absent(&paths.database_path) {
            return Some("global_store_state");
        }
    }

    None
}

fn context_pack_l2_prepare(
    options: &ContextPackOptions,
    connection: &DbConnection,
    request: &ContextRequest,
    filters: &crate::models::QueryFilters,
    runtime_profile: &RuntimeProfileReport,
    output_redaction_enabled: bool,
    embed_backend: EmbedBackend,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<ContextPackL2Context> {
    if let Some(reason) = context_pack_l2_bypass_reason(options, filters) {
        tracing::debug!(
            target: "ee::pack_l2",
            event = "pack_l2_cache_bypassed",
            reason,
        );
        return None;
    }
    // File-backed evidence and live code anchors are checked during assembly,
    // independently of database generations. Do not replay their freshness
    // until those external inputs have a bounded cache identity as well.
    let external_freshness = connection.query(
        "SELECT EXISTS(SELECT 1 FROM memories WHERE provenance_uri LIKE 'file:%'), \
                EXISTS(SELECT 1 FROM memory_anchors)",
        &[],
    );
    if !external_freshness.is_ok_and(|rows| {
        rows.first().is_some_and(|row| {
            row.get(0).and_then(SqlValue::as_i64) == Some(0)
                && row.get(1).and_then(SqlValue::as_i64) == Some(0)
        })
    }) {
        tracing::debug!(target: "ee::pack_l2", event = "pack_l2_cache_bypassed", reason = "external_evidence_freshness");
        return None;
    }
    let index_generation = match context_pack_l2_index_generation(options) {
        Ok(generation) => generation,
        Err(reason) => {
            tracing::debug!(target: "ee::pack_l2", event = "pack_l2_cache_bypassed", reason = %reason);
            return None;
        }
    };
    if index_generation != 0 {
        let index_options = crate::core::index::IndexStatusOptions {
            workspace_path: options.workspace_path.clone(),
            database_path: options.database_path.clone(),
            index_dir: options.index_dir.clone(),
        };
        if !crate::core::index::get_index_status_in_current_snapshot(&index_options, connection)
            .is_ok_and(|status| status.health == crate::core::index::IndexHealth::Ready)
        {
            tracing::debug!(target: "ee::pack_l2", event = "pack_l2_cache_bypassed", reason = "index_not_ready");
            return None;
        }
    }
    let database_identity = match context_pack_l2_database_identity(options) {
        Ok(identity) => identity,
        Err(message) => {
            push_pack_l2_unavailable(degraded, message);
            return None;
        }
    };
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
        database_identity,
        database_generation,
        index_generation,
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
            runtime_profile,
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
    if !degraded.is_empty() {
        return None;
    }
    if let Some(reason) = context_pack_l2_side_effect_bypass_reason(options) {
        tracing::debug!(
            target: "ee::pack_l2",
            event = "pack_l2_cache_hit_bypassed",
            command,
            reason,
        );
        return None;
    }
    let lookup_start = Instant::now();
    match l2_context.cache.peek(&l2_context.key) {
        Ok(PackL2CacheLookup::Hit(hit)) => {
            if context_pack_l2_mutable_state_bypass_reason(options).is_some()
                || context_pack_l2_index_generation(options).ok()
                    != Some(l2_context.key_input.index_generation)
            {
                tracing::debug!(target: "ee::pack_l2", event = "pack_l2_cache_hit_ignored", reason = "source_state_changed");
                return None;
            }
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
                                push_pack_l2_corruption(degraded, &l2_context.key, message);
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
                                // The cache key is derived from the current
                                // request, so the live request is authoritative
                                // for whether reranking was explicitly disabled.
                                // A synthetic missing-index report may not retain
                                // that source-mode detail in its cached payload.
                                options.source_mode
                                    == crate::core::search::SearchSourceMode::LexicalOnly,
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
                    push_pack_l2_corruption(degraded, &l2_context.key, message);
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
            // The lookup already ended in the `corruption` phase for this miss.
            if pack_l2_miss_is_corruption(&miss) {
                push_pack_l2_corruption_degradation(
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
    if !options.persist_pack {
        return;
    }
    if let Some(reason) = context_pack_l2_mutable_state_bypass_reason(options) {
        tracing::debug!(
            target: "ee::pack_l2",
            event = "pack_l2_cache_write_skipped",
            reason,
        );
        return;
    }
    if context_pack_l2_index_generation(options).ok() != Some(l2_context.key_input.index_generation)
    {
        tracing::debug!(target: "ee::pack_l2", event = "pack_l2_cache_write_skipped", reason = "index_changed_during_assembly");
        return;
    }
    if let Some(code) = response
        .data
        .degraded
        .iter()
        .map(|entry| entry.code.as_str())
        .find(|code| {
            matches!(
                *code,
                "context_pack_persist_failed"
                    | "pack_concurrent_limit_reached"
                    | "pack_slot_lock_unavailable"
                    | "read_pool_acquire_timeout"
                    | "read_pool_undersized"
            )
        })
    {
        tracing::debug!(
            target: "ee::pack_l2",
            event = "pack_l2_cache_write_skipped",
            reason = "transient_response_degradation",
            degraded_code = code,
        );
        return;
    }
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
    let requested = crate::core::workspace::stable_workspace_id(workspace_path);
    crate::core::workspace::bound_workspace_id_or_hash(connection, &requested, &[workspace_path])
        .unwrap_or(requested)
}

#[cfg(not(unix))]
fn context_pack_l2_database_identity(_options: &ContextPackOptions) -> Result<Vec<u8>, String> {
    Err("L2 pack cache requires a stable filesystem file identity on this platform".to_owned())
}

#[cfg(unix)]
fn context_pack_l2_database_identity(options: &ContextPackOptions) -> Result<Vec<u8>, String> {
    let database_path = options.workspace_path.join(".ee").join("ee.db");
    let canonical = database_path.canonicalize().map_err(|error| {
        format!(
            "L2 pack cache could not resolve the addressed database {}: {error}",
            database_path.display()
        )
    })?;
    let metadata = canonical.metadata().map_err(|error| {
        format!(
            "L2 pack cache could not inspect the addressed database {}: {error}",
            canonical.display()
        )
    })?;
    let mut identity = Vec::new();
    let path_bytes = canonical.as_os_str().as_encoded_bytes();
    identity.extend_from_slice(
        &u64::try_from(path_bytes.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    identity.extend_from_slice(path_bytes);
    // The source generation tracks logical content. File size and mtime also
    // change when this request persists its pack and audit, so they cannot be
    // part of the addressed-store identity shared with a read-only consumer.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        identity.extend_from_slice(&metadata.dev().to_le_bytes());
        identity.extend_from_slice(&metadata.ino().to_le_bytes());
    }
    let created_nanos = metadata
        .created()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(u128::MAX, |duration| duration.as_nanos());
    identity.extend_from_slice(&created_nanos.to_le_bytes());
    Ok(identity)
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

fn context_pack_l2_index_generation(options: &ContextPackOptions) -> Result<u64, String> {
    let started = Instant::now();
    // Result-cache admission needs the actual published bytes, not directory
    // timestamps or a manifest-only digest that misses segment corruption.
    // Larger indexes still assemble normally; never hash a truncated prefix.
    const MAX_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_ENTRIES: usize = 4096;
    let index_dir = crate::config::workspace::resolve_store_index_dir(
        &options.workspace_path,
        options.database_path.as_deref(),
        options.index_dir.as_deref(),
    );
    crate::core::index::ensure_index_path_has_no_symlinks(
        &index_dir,
        "fingerprint pack cache index",
    )
    .map_err(|error| error.to_string())?;
    if context_pack_l2_path_is_definitely_absent(&index_dir) {
        return Ok(0);
    }
    let mut pending = vec![index_dir.clone()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        if !fs::symlink_metadata(&directory)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_dir()
        {
            return Err("index fingerprint requires regular directories".to_owned());
        }
        for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entries.len() == MAX_ENTRIES {
                return Err("index fingerprint exceeds 4096 entries".to_owned());
            }
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                pending.push(path.clone());
            } else if !kind.is_file() {
                return Err("index fingerprint rejects symlinks and special files".to_owned());
            }
            entries.push((path, kind.is_dir()));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = blake3::Hasher::new();
    hash_labeled_bytes(&mut hasher, "index_fingerprint", b"bounded-content-v1");
    hash_labeled_bytes(
        &mut hasher,
        "index_dir",
        index_dir.as_os_str().as_encoded_bytes(),
    );
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    let entry_count = entries.len();
    for (path, is_directory) in entries {
        crate::core::index::ensure_index_path_has_no_symlinks(
            &path,
            "fingerprint pack cache index",
        )
        .map_err(|error| error.to_string())?;
        let relative = path
            .strip_prefix(&index_dir)
            .map_err(|error| error.to_string())?;
        hash_labeled_bytes(&mut hasher, "path", relative.as_os_str().as_encoded_bytes());
        hash_labeled_bool(&mut hasher, "directory", is_directory);
        if is_directory {
            continue;
        }
        let mut file =
            open_context_file_for_read_no_follow(&path).map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES - total_bytes {
            return Err("index fingerprint exceeds 64 MiB or contains a special file".to_owned());
        }
        hash_labeled_u64(&mut hasher, "length", metadata.len());
        let mut file_bytes = 0_u64;
        loop {
            let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            total_bytes += read as u64;
            file_bytes += read as u64;
            if total_bytes > MAX_BYTES {
                return Err("index fingerprint grew beyond 64 MiB".to_owned());
            }
            hasher.update(&buffer[..read]);
        }
        if file_bytes != metadata.len() {
            return Err("index file changed while computing its fingerprint".to_owned());
        }
    }
    tracing::debug!(
        target: "ee::pack_l2",
        event = "pack_l2_index_fingerprint",
        bytes_read = total_bytes,
        entry_count,
        elapsed_us = started.elapsed().as_micros() as u64,
    );
    Ok(blake3_u64(hasher).max(1))
}

fn context_pack_l2_feature_flags_hash(
    options: &ContextPackOptions,
    filters: &crate::models::QueryFilters,
    runtime_profile: &RuntimeProfileReport,
    output_redaction_enabled: bool,
) -> String {
    let mut hasher = blake3::Hasher::new();
    // Packs produced before the authority guard must never bypass it via L2.
    hash_labeled_bytes(&mut hasher, "instruction_authority_policy", b"v1");
    // Cached source-memory hydration predating native rule filters and
    // advisory posture must be reassembled under the current admission policy.
    hash_labeled_bytes(&mut hasher, "procedural_rule_admission_policy", b"v1");
    // Older responses classify elapsed breaches as within_budget and include
    // elapsed time in signed resource warnings. They cannot satisfy this policy.
    hash_labeled_bytes(&mut hasher, "pack_slo_diagnostics_policy", b"v2");
    if options.pagination.is_some() {
        // Cached pages from the memory-only offset policy can repeat native
        // evidence and claim an empty population. Recompute those pages.
        hash_labeled_bytes(&mut hasher, "native_evidence_pagination_policy", b"v1");
    }
    hash_labeled_bool(
        &mut hasher,
        "output_redaction_enabled",
        output_redaction_enabled,
    );
    hash_labeled_bytes(&mut hasher, "speed", options.speed.as_str().as_bytes());
    hash_labeled_bytes(
        &mut hasher,
        "runtime_profile",
        runtime_profile.active_profile.as_str().as_bytes(),
    );
    hash_labeled_bytes(
        &mut hasher,
        "runtime_profile_source",
        runtime_profile.source.as_bytes(),
    );
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
        "requested_max_tokens",
        format!("{:?}", options.max_tokens).as_bytes(),
    );
    hash_labeled_bytes(
        &mut hasher,
        "requested_candidate_pool",
        format!("{:?}", options.candidate_pool).as_bytes(),
    );
    hash_labeled_bytes(
        &mut hasher,
        "relevance_floor",
        &options
            .relevance_floor
            .unwrap_or(0.0)
            .to_bits()
            .to_le_bytes(),
    );
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

/// The lookup's `corruption` phase and this code use the same rule
/// ([`PackL2CacheMissReason::is_corruption`]), so they cannot disagree.
const fn pack_l2_miss_is_corruption(miss: &PackL2CacheMiss) -> bool {
    miss.reason.is_corruption()
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

/// A lookup or write error from the cache module. That module has already
/// emitted the matching `unavailable` phase, so this adds only the code.
fn push_pack_l2_cache_error(
    degraded: &mut Vec<ContextResponseDegradation>,
    error: PackL2CacheError,
) {
    push_pack_l2_unavailable_degradation(
        degraded,
        format!("L2 pack cache was unavailable; assembled fresh context instead: {error}"),
    );
}

/// `l2_pack_cache_unavailable` for a failure the cache module never saw (key
/// preparation), with its own `phase=unavailable` event (bd-ndzfg.4). The
/// cache key is not known yet at that point, so the event's key is empty.
fn push_pack_l2_unavailable(degraded: &mut Vec<ContextResponseDegradation>, message: String) {
    crate::cache::pack_l2::trace_pack_l2("unavailable", "", &message);
    push_pack_l2_unavailable_degradation(degraded, message);
}

fn push_pack_l2_unavailable_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    message: String,
) {
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

/// `l2_pack_cache_corruption` for a hit this module rejects AFTER the lookup,
/// with its own `phase=corruption` event (bd-ndzfg.4). The lookup's terminal
/// phase was `hit`, and it stays the only terminal phase of that lookup.
fn push_pack_l2_corruption(
    degraded: &mut Vec<ContextResponseDegradation>,
    key: &str,
    message: String,
) {
    crate::cache::pack_l2::trace_pack_l2("corruption", key, &message);
    push_pack_l2_corruption_degradation(degraded, message);
}

fn push_pack_l2_corruption_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    message: String,
) {
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

fn blake3_u64(hasher: blake3::Hasher) -> u64 {
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackL2CacheKeyInput {
    pub(crate) workspace_id: String,
    pub(crate) database_identity: Vec<u8>,
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
        PACK_L2_CACHE_KEY_SCHEMA_V7.as_bytes(),
    );
    hash_labeled_bytes(&mut hasher, "workspace_id", input.workspace_id.as_bytes());
    hash_labeled_bytes(&mut hasher, "database_identity", &input.database_identity);
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
    task_paths::hash(&mut hasher, &input.request.task_paths);
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

/// Set `draft.hash` and return the component digests behind it, which the
/// caller carries to the response's snapshot identity.
#[must_use]
fn refresh_context_pack_hash(
    request: &ContextRequest,
    draft: &mut crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
    coordination: Option<&PackCoordinationSnapshot>,
    read_snapshot_generation: Option<u64>,
    task_lens: Option<&ContextTaskLens>,
) -> crate::pack::PackHashComponentDigests {
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
    draft.hash = Some(components.composite_hash);
    components.digests
}

#[derive(Debug)]
struct PackHashComponents {
    digests: crate::pack::PackHashComponentDigests,
    composite_hash: String,
}

/// The v2 pack hash (ADR 0087 §4, §7, §8).
///
/// Each component is its own blake3 hasher, opened with the input schema tag
/// and the component name, over labeled, length-prefixed fields only
/// (`hash_labeled_*`), so no two field sequences can feed the same bytes. The
/// composite hashes the schema tag and the tagged component digests, never raw
/// fields, which is what lets a differing composite name the component that
/// differs. `omitted` enters the composite only when skipped items are shown,
/// and `rendered_text` only when the text is shown, as in v1.
fn compute_pack_hash_components(
    request: &ContextRequest,
    draft: &crate::pack::PackDraft,
    degraded: &[ContextResponseDegradation],
    output_options: ContextPackOutputOptions,
    coordination: Option<&PackCoordinationSnapshot>,
    read_snapshot_generation: Option<u64>,
    task_lens: Option<&ContextTaskLens>,
) -> PackHashComponents {
    let canonical_degraded = canonical_pack_hash_degraded(degraded);

    let mut request_hasher = pack_hash_component_hasher("request");
    hash_labeled_bytes(&mut request_hasher, "query", request.query.as_bytes());
    hash_labeled_bytes(
        &mut request_hasher,
        "profile",
        request.profile.as_str().as_bytes(),
    );
    hash_labeled_u64(
        &mut request_hasher,
        "budget.max_tokens",
        u64::from(request.budget.max_tokens()),
    );
    hash_labeled_bytes(
        &mut request_hasher,
        "output.profile",
        output_options.profile.as_str().as_bytes(),
    );
    hash_labeled_bytes(
        &mut request_hasher,
        "output.resource_profile",
        output_options.resource_profile.as_str().as_bytes(),
    );
    for (label, value) in [
        (
            "output.include_coverage_fill",
            output_options.include_coverage_fill,
        ),
        (
            "output.include_rendered_text",
            output_options.include_rendered_text,
        ),
        ("output.include_skipped", output_options.include_skipped),
        ("output.include_meta", output_options.include_meta),
        (
            "output.include_verbose_meta",
            output_options.include_verbose_meta,
        ),
    ] {
        hash_labeled_bool(&mut request_hasher, label, value);
    }
    hash_labeled_optional_u64(
        &mut request_hasher,
        "read_snapshot_generation",
        read_snapshot_generation,
    );
    hash_context_task_lens(&mut request_hasher, task_lens);
    task_paths::hash(&mut request_hasher, &request.task_paths);

    let mut items_hasher = pack_hash_component_hasher("items");
    hash_labeled_u64(
        &mut items_hasher,
        "used_tokens",
        u64::from(draft.used_tokens),
    );
    hash_labeled_count(&mut items_hasher, "item.count", draft.items.len());
    for item in &draft.items {
        hash_pack_hash_item(&mut items_hasher, item);
    }
    hash_labeled_count(
        &mut items_hasher,
        "evidence.count",
        draft.evidence_items.len(),
    );
    for item in &draft.evidence_items {
        hash_pack_hash_evidence_item(&mut items_hasher, item);
    }

    let mut omitted_hasher = pack_hash_component_hasher("omitted");
    hash_labeled_count(&mut omitted_hasher, "omission.count", draft.omitted.len());
    for omission in &draft.omitted {
        hash_labeled_bytes(
            &mut omitted_hasher,
            "omission.memory_id",
            omission.memory_id.to_string().as_bytes(),
        );
        hash_labeled_u64(
            &mut omitted_hasher,
            "omission.estimated_tokens",
            u64::from(omission.estimated_tokens),
        );
        hash_labeled_bytes(
            &mut omitted_hasher,
            "omission.reason",
            omission.reason.as_str().as_bytes(),
        );
        hash_attempt_family_multiplicity(
            &mut omitted_hasher,
            omission.attempt_family_multiplicity.as_ref(),
        );
    }

    let mut degraded_hasher = pack_hash_component_hasher("degraded");
    hash_labeled_count(
        &mut degraded_hasher,
        "degradation.count",
        canonical_degraded.len(),
    );
    for degradation in &canonical_degraded {
        hash_labeled_bytes(
            &mut degraded_hasher,
            "degradation.code",
            degradation.code.as_bytes(),
        );
        hash_labeled_bytes(
            &mut degraded_hasher,
            "degradation.severity",
            degradation.severity.as_str().as_bytes(),
        );
        hash_labeled_bytes(
            &mut degraded_hasher,
            "degradation.message",
            degradation.message.as_bytes(),
        );
        hash_labeled_optional_bytes(
            &mut degraded_hasher,
            "degradation.repair",
            degradation.repair.as_deref().map(str::as_bytes),
        );
    }

    let mut coordination_hasher = pack_hash_component_hasher("coordination");
    let coordination_input = coordination.map(coordination_snapshot_hash_input);
    hash_labeled_optional_bytes(
        &mut coordination_hasher,
        "coordination.snapshot",
        coordination_input.as_deref().map(str::as_bytes),
    );

    let rendered_text = crate::pack::render_context_markdown_with_analysis(
        request,
        draft,
        &canonical_degraded,
        &[],
        &[],
        coordination,
    );
    let mut rendered_text_hasher = pack_hash_component_hasher("rendered_text");
    hash_labeled_bytes(&mut rendered_text_hasher, "text", rendered_text.as_bytes());

    let digests = crate::pack::PackHashComponentDigests {
        request: finalize_blake3(request_hasher),
        items: finalize_blake3(items_hasher),
        omitted: finalize_blake3(omitted_hasher),
        degraded: finalize_blake3(degraded_hasher),
        coordination: finalize_blake3(coordination_hasher),
        rendered_text: finalize_blake3(rendered_text_hasher),
    };

    let mut composite_hasher = blake3::Hasher::new();
    hash_labeled_bytes(
        &mut composite_hasher,
        "schema",
        crate::pack::PACK_HASH_INPUT_SCHEMA_V2.as_bytes(),
    );
    hash_labeled_bytes(&mut composite_hasher, "request", digests.request.as_bytes());
    hash_labeled_bytes(&mut composite_hasher, "items", digests.items.as_bytes());
    if output_options.include_skipped {
        hash_labeled_bytes(&mut composite_hasher, "omitted", digests.omitted.as_bytes());
    }
    hash_labeled_bytes(
        &mut composite_hasher,
        "degraded",
        digests.degraded.as_bytes(),
    );
    hash_labeled_bytes(
        &mut composite_hasher,
        "coordination",
        digests.coordination.as_bytes(),
    );
    if output_options.include_rendered_text {
        hash_labeled_bytes(
            &mut composite_hasher,
            "rendered_text",
            digests.rendered_text.as_bytes(),
        );
    }

    PackHashComponents {
        composite_hash: finalize_pack_hash_v2(composite_hasher),
        digests,
    }
}

/// The degraded set the v2 pack hash binds (ADR 0087 §4): non-canonical
/// telemetry codes are dropped, the rest sorted by (code, severity, message,
/// repair) with exact duplicates removed. Emission order and repetition are
/// presentation, and timing is telemetry, so neither can fork `pack.hash`,
/// whichever `refresh_context_pack_hash` call site passes them in.
fn canonical_pack_hash_degraded(
    degraded: &[ContextResponseDegradation],
) -> Vec<ContextResponseDegradation> {
    let mut canonical: Vec<ContextResponseDegradation> = degraded
        .iter()
        .filter(|entry| !crate::pack::is_non_canonical_telemetry_degradation_code(&entry.code))
        .cloned()
        .collect();
    canonical.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| left.severity.as_str().cmp(right.severity.as_str()))
            .then_with(|| left.message.cmp(&right.message))
            .then_with(|| left.repair.cmp(&right.repair))
    });
    canonical.dedup();
    canonical
}

fn pack_hash_component_hasher(component: &str) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hash_labeled_bytes(
        &mut hasher,
        "schema",
        crate::pack::PACK_HASH_INPUT_SCHEMA_V2.as_bytes(),
    );
    hash_labeled_bytes(&mut hasher, "component", component.as_bytes());
    hasher
}

fn hash_labeled_count(hasher: &mut blake3::Hasher, label: &str, count: usize) {
    hash_labeled_u64(hasher, label, u64::try_from(count).unwrap_or(u64::MAX));
}

fn hash_labeled_optional_bytes(hasher: &mut blake3::Hasher, label: &str, value: Option<&[u8]>) {
    hash_labeled_bool(hasher, &format!("{label}.present"), value.is_some());
    if let Some(value) = value {
        hash_labeled_bytes(hasher, label, value);
    }
}

fn hash_labeled_q20_12(hasher: &mut blake3::Hasher, label: &str, value: f32) {
    hash_labeled_bytes(hasher, label, &crate::pack::q20_12_le_bytes(value));
}

fn hash_pack_hash_provenance(
    hasher: &mut blake3::Hasher,
    label: &str,
    provenance: &[crate::pack::PackProvenance],
) {
    hash_labeled_count(hasher, &format!("{label}.count"), provenance.len());
    for entry in provenance {
        hash_labeled_bytes(
            hasher,
            &format!("{label}.uri"),
            entry.uri.to_string().as_bytes(),
        );
        hash_labeled_bytes(hasher, &format!("{label}.note"), entry.note.as_bytes());
    }
}

fn hash_pack_hash_item(hasher: &mut blake3::Hasher, item: &crate::pack::PackDraftItem) {
    hash_labeled_bytes(
        hasher,
        "item.memory_id",
        item.memory_id.to_string().as_bytes(),
    );
    hash_labeled_u64(hasher, "item.rank", u64::from(item.rank));
    hash_labeled_bytes(hasher, "item.section", item.section.as_str().as_bytes());
    hash_labeled_bytes(hasher, "item.content", item.content.as_bytes());
    hash_labeled_u64(
        hasher,
        "item.estimated_tokens",
        u64::from(item.estimated_tokens),
    );
    hash_labeled_q20_12(hasher, "item.relevance", item.relevance.into_inner());
    hash_labeled_q20_12(hasher, "item.utility", item.utility.into_inner());
    hash_labeled_bool(
        hasher,
        "item.proximity_to_seed.present",
        item.proximity_to_seed.is_some(),
    );
    if let Some(proximity_to_seed) = item.proximity_to_seed {
        hash_labeled_q20_12(hasher, "item.proximity_to_seed", proximity_to_seed);
    }
    hash_labeled_bool(
        hasher,
        "item.score_breakdown.present",
        item.score_breakdown.is_some(),
    );
    if let Some(score_breakdown) = item.score_breakdown {
        hash_labeled_q20_12(
            hasher,
            "item.score_breakdown.text_score",
            score_breakdown.text_score,
        );
        hash_labeled_q20_12(
            hasher,
            "item.score_breakdown.ppr_score",
            score_breakdown.ppr_score,
        );
        hash_labeled_q20_12(
            hasher,
            "item.score_breakdown.combined_score",
            score_breakdown.combined_score,
        );
    }
    hash_attempt_family_multiplicity(hasher, item.attempt_family_multiplicity.as_ref());
    hash_labeled_bytes(hasher, "item.why", item.why.as_bytes());
    hash_labeled_bytes(
        hasher,
        "item.selected_in",
        item.selected_in.as_str().as_bytes(),
    );
    hash_pack_hash_provenance(hasher, "item.provenance", &item.provenance);
    hash_labeled_optional_bytes(
        hasher,
        "item.diversity_key",
        item.diversity_key.as_deref().map(str::as_bytes),
    );
    hash_labeled_bytes(
        hasher,
        "item.trust.class",
        item.trust.class.as_str().as_bytes(),
    );
    if item.trust.subclass.as_deref() == Some("procedural_rule") {
        // Bind changed authority semantics even when rendered text
        // is omitted; old authoritative rule packs have another hash.
        hash_labeled_bytes(hasher, "procedural_rule_posture_policy", b"advisory.v1");
    }
    hash_labeled_optional_bytes(
        hasher,
        "item.trust.subclass",
        item.trust.subclass.as_deref().map(str::as_bytes),
    );
    hash_labeled_optional_bytes(
        hasher,
        "item.tombstoned_at",
        item.tombstoned_at.as_deref().map(str::as_bytes),
    );
    hash_labeled_bool(hasher, "item.lifecycle.present", item.lifecycle.is_some());
    if let Some(lifecycle) = &item.lifecycle {
        hash_labeled_bytes(
            hasher,
            "item.lifecycle.validity_status",
            lifecycle.validity_status.as_bytes(),
        );
        hash_labeled_bytes(
            hasher,
            "item.lifecycle.validity_window_kind",
            lifecycle.validity_window_kind.as_bytes(),
        );
        hash_labeled_optional_bytes(
            hasher,
            "item.lifecycle.valid_from",
            lifecycle.valid_from.as_deref().map(str::as_bytes),
        );
        hash_labeled_optional_bytes(
            hasher,
            "item.lifecycle.valid_to",
            lifecycle.valid_to.as_deref().map(str::as_bytes),
        );
    }
    hash_labeled_count(hasher, "item.redaction.count", item.redactions.len());
    for redaction in &item.redactions {
        hash_labeled_bytes(hasher, "item.redaction.reason", redaction.reason.as_bytes());
        hash_labeled_bytes(
            hasher,
            "item.redaction.placeholder",
            redaction.placeholder.as_bytes(),
        );
    }
    hash_labeled_count(
        hasher,
        "item.freshness_facet.count",
        item.freshness_facets.len(),
    );
    for facet in &item.freshness_facets {
        for (label, value) in [
            ("facet.kind", facet.kind.as_str()),
            ("facet.freshness", facet.freshness.as_str()),
            ("facet.drift_status", facet.drift_status.as_str()),
            ("facet.severity", facet.severity.as_str()),
            ("facet.top_reason", facet.top_reason.as_str()),
            (
                "facet.revalidation_command",
                facet.revalidation_command.as_str(),
            ),
        ] {
            hash_labeled_bytes(hasher, label, value.as_bytes());
        }
        hash_labeled_bool(hasher, "facet.stale_anchor", facet.stale_anchor);
        for (label, value) in [
            ("facet.degraded_code", facet.degraded_code.as_deref()),
            (
                "facet.captured_at_commit",
                facet.captured_at_commit.as_deref(),
            ),
            ("facet.current_commit", facet.current_commit.as_deref()),
        ] {
            hash_labeled_optional_bytes(hasher, label, value.map(str::as_bytes));
        }
        hash_labeled_optional_u64(
            hasher,
            "facet.commit_distance",
            facet.commit_distance.map(u64::from),
        );
        hash_labeled_count(
            hasher,
            "facet.changed_region.count",
            facet.changed_regions.len(),
        );
        for changed_region in &facet.changed_regions {
            hash_labeled_bytes(hasher, "facet.changed_region", changed_region.as_bytes());
        }
        hash_labeled_count(hasher, "facet.anchor.count", facet.anchors.len());
        for anchor in &facet.anchors {
            for (label, value) in [
                ("anchor.kind", anchor.anchor_kind.as_str()),
                ("anchor.value_hash", anchor.anchor_value_hash.as_str()),
                (
                    "anchor.redacted_value",
                    anchor.redacted_anchor_value.as_str(),
                ),
                (
                    "anchor.captured_span_hash",
                    anchor.captured_span_hash.as_str(),
                ),
                ("anchor.freshness_state", anchor.freshness_state.as_str()),
                ("anchor.freshness", anchor.freshness.as_str()),
            ] {
                hash_labeled_bytes(hasher, label, value.as_bytes());
            }
            hash_labeled_bytes(
                hasher,
                "anchor.generation",
                &anchor.generation.to_le_bytes(),
            );
            hash_labeled_bool(hasher, "anchor.stale_anchor", anchor.stale_anchor);
        }
    }
}

fn hash_pack_hash_evidence_item(hasher: &mut blake3::Hasher, item: &crate::pack::PackEvidenceItem) {
    for (label, value) in [
        ("evidence.evidence_id", item.evidence_id.as_str()),
        ("evidence.entity_revision", item.entity_revision.as_str()),
        ("evidence.session_id", item.session_id.as_str()),
    ] {
        hash_labeled_bytes(hasher, label, value.as_bytes());
    }
    hash_labeled_u64(hasher, "evidence.start_line", u64::from(item.start_line));
    hash_labeled_u64(hasher, "evidence.end_line", u64::from(item.end_line));
    hash_labeled_u64(hasher, "evidence.rank", u64::from(item.rank));
    hash_labeled_bytes(hasher, "evidence.section", item.section.as_str().as_bytes());
    hash_labeled_bytes(hasher, "evidence.content", item.content.as_bytes());
    hash_labeled_u64(
        hasher,
        "evidence.estimated_tokens",
        u64::from(item.estimated_tokens),
    );
    // v1 fed these as raw f32 bytes; v2 quantizes every hashed score.
    hash_labeled_q20_12(hasher, "evidence.relevance", item.relevance.into_inner());
    hash_labeled_q20_12(hasher, "evidence.utility", item.utility.into_inner());
    hash_labeled_bytes(hasher, "evidence.why", item.why.as_bytes());
    hash_labeled_bytes(
        hasher,
        "evidence.trust.class",
        item.trust.class.as_str().as_bytes(),
    );
    hash_labeled_optional_bytes(
        hasher,
        "evidence.trust.subclass",
        item.trust.subclass.as_deref().map(str::as_bytes),
    );
    hash_pack_hash_provenance(hasher, "evidence.provenance", &item.provenance);
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
        &crate::pack::q20_12_le_bytes(snapshot.effective_discount_factor),
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
            &crate::pack::q20_12_le_bytes(membership.member_discount_factor),
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

/// The v2 composite `pack.hash` string. Its form is the self-identifying
/// knob ADR 0087 §8 names, kept in one place.
fn finalize_pack_hash_v2(hasher: blake3::Hasher) -> String {
    finalize_blake3(hasher)
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
            serde_json::Value::String(components.digests.request.clone()),
        )
        .with_field(
            "draft_items_hash",
            serde_json::Value::String(components.digests.items.clone()),
        )
        .with_field(
            "degraded_summary_hash",
            serde_json::Value::String(components.digests.degraded.clone()),
        )
        .with_field(
            "rendered_text_hash",
            serde_json::Value::String(components.digests.rendered_text.clone()),
        )
        .with_field(
            "composite_hash",
            serde_json::Value::String(components.composite_hash.clone()),
        )
        .with_field("run_index", serde_json::Value::from(run_index)),
    );
}

#[allow(clippy::type_complexity)]
#[cfg(test)]
fn candidates_from_search_with_metrics(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &crate::core::search::SearchReport,
    filters: &crate::models::QueryFilters,
    include_tombstoned: bool,
    degraded: &mut Vec<ContextResponseDegradation>,
    preloaded_memories: Option<&BTreeMap<String, StoredMemory>>,
) -> (Vec<PackCandidate>, CandidateResolutionMetrics) {
    candidates_from_search_for_task_paths(
        connection,
        workspace_path,
        search_report,
        filters,
        include_tombstoned,
        degraded,
        preloaded_memories,
        &[],
    )
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn candidates_from_search_for_task_paths(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &crate::core::search::SearchReport,
    filters: &crate::models::QueryFilters,
    include_tombstoned: bool,
    degraded: &mut Vec<ContextResponseDegradation>,
    preloaded_memories: Option<&BTreeMap<String, StoredMemory>>,
    targets: &[String],
) -> (Vec<PackCandidate>, CandidateResolutionMetrics) {
    let requested = crate::core::workspace::stable_workspace_id(workspace_path);
    let bound_workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
        connection,
        &requested,
        &[workspace_path],
    )
    .ok();
    let mut metrics = CandidateResolutionMetrics {
        search_hits: search_report.results.len(),
        ..CandidateResolutionMetrics::default()
    };

    // Phase 1: Resolve all memory IDs from hits (including artifact links).
    // This still does per-hit artifact link lookups but avoids O(k) memory/tag lookups.
    let hit_resolution_start = Instant::now();
    let mut mesh_blocked_hits = 0usize;
    // Keep the exact live rule projection admitted during ID resolution.
    // A second row lookup could detach the selected body from its checked
    // revision, tags and scope, even though the source MemoryId stays the same.
    let mut rules_map: BTreeMap<String, RuleIndexProjection> = BTreeMap::new();
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
        // A live, undistilled CASS span belongs to the direct-evidence lane.
        // Deferring it is successful resolution, not a failed memory lookup;
        // the native admission boundary below owns its selection and denial.
        let evidence_resolution =
            resolve_evidence_pack_hit(connection, workspace_path, hit, degraded);
        if matches!(
            evidence_resolution.as_ref(),
            Some(EvidencePackHitResolution::Direct)
        ) {
            continue;
        }
        let resolution = match MemoryId::from_str(&hit.doc_id) {
            Ok(id) => Some((id, None)),
            Err(_) => {
                metrics.artifact_link_lookups = metrics.artifact_link_lookups.saturating_add(1);
                artifact_linked_memory_id(connection, hit, degraded)
                    // Procedural-rule hits hydrate through their source
                    // memories the same way artifact hits hydrate through
                    // their memory links (bd-3h6bz).
                    .or_else(|| {
                        rule_linked_memory_id(connection, targets, workspace_path, hit, degraded)
                            .map(|(memory_id, projection)| {
                                let rule_id = projection.rule().id.clone();
                                rules_map.insert(rule_id.clone(), projection);
                                (memory_id, Some(rule_id))
                            })
                    })
                    .or(match evidence_resolution {
                        Some(EvidencePackHitResolution::Linked {
                            memory_id,
                            evidence_id,
                        }) => Some((memory_id, Some(evidence_id))),
                        Some(EvidencePackHitResolution::Direct) | None => None,
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
                let promoted_rule = artifact_id.as_deref().and_then(|id| rules_map.get(id));
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
                    let tags = promoted_rule.map_or_else(
                        || tags_map.get(&memory_key).map(Vec::as_slice).unwrap_or(&[]),
                        RuleIndexProjection::tags,
                    );
                    if !filters.matches_tags(tags) {
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
                    // The rule body has its own creation/revision time. Its
                    // source memory still has to satisfy the existing validity
                    // window, because it remains this v2 item's storage anchor.
                    let temporal = if let Some(projection) = promoted_rule {
                        let rule = projection.rule();
                        if temporal_record_matches(
                            &rule.created_at,
                            &rule.updated_at,
                            &filters.temporal,
                        ) {
                            temporal_memory_validity_outcome(memory, &filters.temporal)
                        } else {
                            TemporalCandidateOutcome::Exclude
                        }
                    } else {
                        temporal_memory_outcome(memory, &filters.temporal)
                    };
                    match temporal {
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
                    let (trust_class, posture) = promoted_rule.map_or_else(
                        || {
                            (
                                memory.trust_class.as_str(),
                                posture_for_trust_class(&memory.trust_class),
                            )
                        },
                        |projection| {
                            (
                                projection.rule().trust_class.as_str(),
                                PackTrustPosture::Advisory.as_str(),
                            )
                        },
                    );
                    if !filters.trust.matches(trust_class, posture) {
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
                    let content = promoted_rule.map_or(memory.content.as_str(), |projection| {
                        projection.rule().content.as_str()
                    });
                    if !redaction_allow_categories(content, &filters.redaction) {
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
                    bound_workspace_id: bound_workspace_id.as_deref(),
                    query: &search_report.query,
                    validity_reference_time: filters
                        .temporal
                        .validity
                        .as_ref()
                        .and_then(|validity| validity.reference_time)
                        .or(filters.temporal.as_of),
                    include_tombstoned,
                    freshness_file_cache: &mut freshness_file_cache,
                    rules: &rules_map,
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

    // The pinned FrankenNetworkX release does not expose deterministic
    // personalized PageRank. Production pack assembly must therefore preserve
    // the textual ranking and report the missing graph sub-capability instead
    // of executing ee's local reference implementation. Unit tests below keep
    // exercising that reference implementation until the upstream API lands.
    #[cfg(not(test))]
    {
        push_graph_ppr_upstream_unavailable_degradation(degraded);
        return PersonalizedPageRankRerankMetrics::default();
    }

    #[cfg(test)]
    {
        let seed_weights = seed_map
            .iter()
            .map(|(memory_id, weight)| (memory_id.to_string(), *weight))
            .collect::<BTreeMap<_, _>>();
        let policy = crate::graph::ppr::PersonalizedPageRankPolicy::default();
        let ppr_params =
            crate::graph::ppr::personalized_pagerank_cache_params(policy, &seed_weights);
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
        let cache_run =
            match crate::graph::ppr::compute_personalized_pagerank_result_cached_with_graph(
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
        .map(SearchHit::relevance_score)
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
        let Some(weight) = positive_f32_score(hit.relevance_score()) else {
            continue;
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
            Some(memory.workspace_id.as_str()),
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
    }

    let requested = crate::core::workspace::stable_workspace_id(workspace_path);
    match crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[workspace_path],
    ) {
        Ok(Some(workspace)) => {
            workspace_ids.insert(workspace.id);
        }
        Ok(None) => {}
        Err(error) => push_degradation(
            degraded,
            "context_graph_workspace_lookup_unavailable",
            ContextResponseSeverity::Low,
            format!(
                "Graph snapshot posture could not resolve workspace path {}: {}",
                workspace_path.display(),
                error.message()
            ),
            Some("ee status --json".to_string()),
        ),
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
    // `redaction_allow_categories` already returns true for an empty
    // allow-list, so guarding the call on `!is_empty()` only re-derives that
    // short-circuit behind a double negation.
    redaction_allow_categories(&memory.content, &filters.redaction)
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
                let in_scope = if scope_context.scope == MemoryScope::Verified
                    && candidate.trust.subclass.as_deref() == Some("procedural_rule")
                {
                    // Verified is a declared trust-class lane, not permission
                    // to treat unsigned guidance as authoritative. The rule's
                    // own class decides this lane; its parent cannot lend or
                    // remove verification. Its effective posture stays advisory.
                    matches!(
                        candidate.trust.class,
                        TrustClass::HumanExplicit
                            | TrustClass::PeerHumanAttested
                            | TrustClass::AgentValidated
                    )
                } else {
                    scope_context.memory_in_scope_with_tags(memory, tags)
                };
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

fn filter_candidates_by_instruction_authority(
    candidates: &mut Vec<PackCandidate>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<PackOmission> {
    let mut omitted = Vec::new();
    let mut signal_codes = BTreeSet::new();
    candidates.retain(|candidate| {
        let report = crate::policy::detect_instruction_like_content(&candidate.content);
        let codes = report.authority_signal_codes();
        if codes.is_empty() {
            return true;
        }
        signal_codes.extend(codes);
        omitted.push(PackOmission {
            memory_id: candidate.memory_id,
            estimated_tokens: candidate.estimated_tokens,
            relevance: candidate.relevance,
            utility: candidate.utility,
            attempt_family_multiplicity: candidate.attempt_family_multiplicity.clone(),
            reason: PackOmissionReason::ExcludedByPolicy,
            rejected_at: PackRejectionStage::CandidateFilter,
            feasible: false,
            could_fit_with_budget: None,
        });
        false
    });
    omitted.sort_by_key(|omission| omission.memory_id);
    if !omitted.is_empty() {
        push_degradation(
            degraded,
            "context_filtered_results",
            ContextResponseSeverity::Medium,
            format!(
                "{} candidate memories excluded by instruction-authority policy ({}). Stored memories remain unchanged.",
                omitted.len(),
                signal_codes.into_iter().collect::<Vec<_>>().join(", "),
            ),
            Some(
                "Inspect the excluded memory IDs with ee why before revising stored guidance."
                    .to_string(),
            ),
        );
    }
    omitted
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
    bound_workspace_id: Option<&str>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackCandidate> {
    let mut provenance = Vec::new();
    if let Some(memory_provenance) = provenance_for_memory(
        memory,
        memory_id,
        workspace_path,
        bound_workspace_id,
        degraded,
    ) {
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

    if !temporal_record_matches(&memory.created_at, &memory.updated_at, filters) {
        return TemporalCandidateOutcome::Exclude;
    }

    temporal_memory_validity_outcome(memory, filters)
}

fn temporal_memory_validity_outcome(
    memory: &StoredMemory,
    filters: &crate::models::QueryTemporalFilters,
) -> TemporalCandidateOutcome {
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

fn temporal_record_matches(
    created_at: &str,
    updated_at: &str,
    filters: &crate::models::QueryTemporalFilters,
) -> bool {
    if filters.is_empty() {
        return true;
    }
    let Some(created_at) = parse_stored_memory_timestamp(created_at) else {
        return false;
    };
    if filters.after.is_some_and(|after| created_at < after)
        || filters.before.is_some_and(|before| created_at > before)
    {
        return false;
    }
    filters.as_of.is_none_or(|as_of| {
        row_timestamp_within_bound(created_at, as_of)
            && parse_stored_memory_timestamp(updated_at)
                .is_some_and(|updated| row_timestamp_within_bound(updated, as_of))
    })
}

/// Compare a ROW BOOKKEEPING timestamp (`created_at`, `updated_at`) against an
/// `as_of` bound, at the granularity the bound is actually expressed in.
///
/// bd-docid. `--as-of` is routinely handed a value that was read out of an
/// AUTHOR VALIDITY column: the CLI surfaces take `valid_from` and
/// `superseded_at` as revision boundaries, and `normalize_validity_timestamp`
/// writes those at `SecondsFormat::Secs` (see `core::memory`, bd-o22r0). Row
/// bookkeeping columns are written by `normalize_row_timestamp` and carry
/// sub-second precision *by contract* — the two canons are deliberately
/// different and rewriting either one is what those beads forbid.
///
/// Comparing the canons directly with `<=` therefore excludes every row whose
/// row timestamp lands in the same second as the bound. `ee memory revise`
/// produces exactly that: the new revision's `valid_from` is the truncated
/// `revised_at` while its `created_at` is the untruncated instant, and the
/// superseded predecessor's `updated_at` is bumped the same way — so a pack
/// read at `--as-of <the revision boundary>` dropped BOTH revisions and came
/// back empty.
///
/// The write side already obeys "use the canon of what you are comparing to".
/// This is the read-side half of the same rule: when the bound carries no
/// sub-second component, compare seconds to seconds. A bound that does carry
/// sub-second precision is compared exactly, unchanged.
fn row_timestamp_within_bound(row: DateTime<Utc>, bound: DateTime<Utc>) -> bool {
    if bound.timestamp_subsec_nanos() == 0 {
        row.timestamp() <= bound.timestamp()
    } else {
        row <= bound
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
    bound_workspace_id: Option<&'a str>,
    query: &'a str,
    validity_reference_time: Option<DateTime<Utc>>,
    include_tombstoned: bool,
    freshness_file_cache: &'a mut crate::core::memory::EvidenceFreshnessFileCache,
    /// Promoted procedural rules referenced by rule-artifact hits, batched
    /// alongside memories so a rule hit hydrates its own body into the pack
    /// candidate instead of collapsing into its source memory (bd-3h6bz).
    rules: &'a BTreeMap<String, RuleIndexProjection>,
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
        source.bound_workspace_id,
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
    // A rule hit must hydrate that live rule. Its source memory remains the
    // identity/provenance anchor, but cannot inherit a missing or retired
    // rule's retrieval score by silently substituting its own body.
    let promoted_rule = match artifact_id
        .as_deref()
        .filter(|artifact| artifact.starts_with("rule_"))
    {
        Some(artifact) => {
            let Some(rule) = source
                .rules
                .get(artifact)
                .map(RuleIndexProjection::rule)
                .filter(|rule| rule.tombstoned_at.is_none())
            else {
                subspans.candidate_construction += construction_start.elapsed();
                return None;
            };
            Some(rule)
        }
        None => None,
    };
    // Unknown native trust is a denial, never a promotion to a verified class.
    let promoted_rule_trust = promoted_rule
        .map(|rule| TrustClass::from_str(&rule.trust_class))
        .transpose()
        .ok()?;
    let Some(utility) = unit_score(promoted_rule.map_or(memory.utility, |rule| rule.utility))
    else {
        subspans.candidate_construction += construction_start.elapsed();
        return None;
    };
    let content = match promoted_rule {
        Some(rule) => rule.content.clone(),
        None => memory.content.clone(),
    };
    let applicability = artifact_id
        .as_deref()
        .and_then(|id| source.rules.get(id))
        .and_then(task_paths::scope_explanation);
    let applicability_tokens = applicability.as_deref().map_or(0, estimate_tokens_default);
    let mut why = candidate_selection_why(
        source.query,
        hit.source.as_str(),
        relevance.into_inner(),
        utility.into_inner(),
        artifact_id.as_deref(),
    );
    if let Some(applicability) = applicability {
        why.push(' ');
        why.push_str(&applicability);
    }
    let mut candidate_provenances = vec![provenance];
    if let Some(rule) = promoted_rule {
        if let Ok(entry) = PackProvenance::new(
            candidate_provenances[0].uri.clone(),
            format!("Promoted procedural rule {}", rule.id),
        ) {
            candidate_provenances.push(entry);
        }
    }
    let candidate = match PackCandidate::new(PackCandidateInput {
        memory_id,
        section: promoted_rule
            .map(|_| PackSection::ProceduralRules)
            .unwrap_or_else(|| section_for_memory(memory)),
        estimated_tokens: estimate_tokens_default(&content).saturating_add(applicability_tokens),
        content,
        relevance,
        utility,
        provenance: candidate_provenances,
        why,
    }) {
        Ok(candidate) => candidate,
        Err(_) => {
            subspans.candidate_construction += construction_start.elapsed();
            return None;
        }
    };

    let trust = promoted_rule_trust.map_or_else(
        || trust_signal_for_memory(memory, memory_id, degraded),
        |class| PackTrustSignal::new(class, Some("procedural_rule".to_owned())),
    );
    let candidate = candidate
        .with_diversity_key(diversity_key_for_memory(memory, &tags))
        .with_trust_signal(trust)
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
    if let Some(memory_provenance) = provenance_for_memory(
        &memory,
        item.memory_id,
        source.workspace_path,
        source
            .workspace_ids
            .contains(&memory.workspace_id)
            .then_some(memory.workspace_id.as_str()),
        degraded,
    ) {
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
/// unit_score(search_hit.relevance_score())...]; formula=unit_score(field)=clamp(...);
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
    utility: f32,
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
        "matched '{display_query}' via {search_source} (relevance {search_score:.4}, utility {utility:.4})",
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
    targets: &[String],
    workspace_path: &Path,
    hit: &crate::core::search::SearchHit,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<(MemoryId, RuleIndexProjection)> {
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
    if !projection.is_pack_admissible() {
        push_degradation(
            degraded,
            "context_rule_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!("Rule {rule_id} is no longer pack-admissible."),
            Some("ee index rebuild --json".to_owned()),
        );
        return None;
    }
    if TrustClass::from_str(&projection.rule().trust_class).is_err() {
        push_degradation(
            degraded,
            "context_rule_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!("Rule {rule_id} has an invalid native trust class and was excluded."),
            Some(format!("ee rule show {rule_id} --json")),
        );
        return None;
    }
    if !task_paths::matches_rule(&projection, targets) {
        push_degradation(
            degraded,
            "context_rule_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!("Rule {rule_id} requires a matching literal task path and was excluded from this pack."),
            Some("Use ee pack \"<task>\" --task-path src/lib.rs --json with an applicable workspace-relative target.".to_owned()),
        );
        return None;
    }
    // Semantic hits carry ids and scores only. Treating missing
    // entity_revision as stale dropped every neural/hybrid rule hit from
    // packs while `ee search` still returned the rule (bd-3h6bz,
    // north_star_3b_pack_surfaces_promoted_rule on hz2).
    if let Some(indexed_revision) = hit
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("entity_revision"))
        .and_then(serde_json::Value::as_str)
        && indexed_revision != projection.entity_revision()
    {
        push_degradation(
            degraded,
            "context_rule_hit_unhydrated",
            ContextResponseSeverity::Low,
            format!("Rule {rule_id} matched a stale derived index revision."),
            Some("ee index rebuild --json".to_owned()),
        );
        return None;
    }

    let memory_id =
        projection.source_memory_ids().iter().find_map(
            |source_memory_id| match MemoryId::from_str(source_memory_id) {
                Ok(memory_id) => Some(memory_id),
                Err(_) => {
                    push_degradation(
                        degraded,
                        "context_rule_hit_unhydrated",
                        ContextResponseSeverity::Low,
                        format!("Rule {rule_id} references a malformed source memory identifier."),
                        Some(format!("ee rule show {rule_id} --json")),
                    );
                    None
                }
            },
        );
    if let Some(memory_id) = memory_id {
        return Some((memory_id, projection));
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

#[derive(Clone)]
struct DirectEvidencePackCandidate {
    item: PackEvidenceItem,
    linked_memory_id: Option<String>,
}

/// Admit and deduplicate native evidence before pagination, without spending
/// the page's token budget or assigning a selected rank. Pages can therefore
/// reach every eligible hit, including hits that did not fit an earlier page.
fn collect_direct_evidence_pack_candidates(
    connection: &DbConnection,
    workspace_path: &Path,
    search_report: &crate::core::search::SearchReport,
    request: &ContextRequest,
    filters: &crate::models::QueryFilters,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Vec<DirectEvidencePackCandidate> {
    if !request.sections.is_empty() && !request.sections.contains(&PackSection::Evidence) {
        return Vec::new();
    }

    let workspace_ids = context_workspace_ids(connection, workspace_path, degraded);
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut rejected_live_admission = 0_usize;
    let mut filtered_count = 0_usize;

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
        // Native evidence has its own identity and trust class. It must not
        // inherit tags or authority from a linked memory, or bypass filters
        // simply because it is appended after memory candidate selection.
        if !direct_evidence_matches_filters(&span, filters) {
            filtered_count = filtered_count.saturating_add(1);
            continue;
        }
        let estimated_tokens = estimate_tokens_default(&span.excerpt).max(1);
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
        // bd-reality-core-convergence-1azkt.11. Was
        // `.unwrap_or_else(|_| UnitScore::zero())`: a projection the unit type
        // REFUSED was admitted as a confident 0.0, and the `why` string two
        // statements below then reported that manufactured number to the agent
        // as "relevance {:.4}". Rejecting it uses the same `continue` this loop
        // already applies to an unconstructable provenance directly above, so
        // an unscorable hit is counted as a rejected admission rather than
        // admitted with an invented score.
        let Ok(relevance) = UnitScore::parse(hit.relevance_score()) else {
            rejected_live_admission = rejected_live_admission.saturating_add(1);
            continue;
        };
        let utility = UnitScore::neutral();
        let entity_revision = span.pack_entity_revision();
        let why = format!(
            "matched '{}' via {} (relevance {:.4}, utility 0.5000); selected live-admitted imported evidence {}",
            request.query,
            hit.source.as_str(),
            hit.relevance_score(),
            span.id
        );
        candidates.push(DirectEvidencePackCandidate {
            linked_memory_id: span.memory_id,
            item: PackEvidenceItem {
                rank: 0,
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
            },
        });
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
    if filtered_count > 0 {
        push_degradation(
            degraded,
            "context_filtered_results",
            ContextResponseSeverity::Low,
            format!("{filtered_count} imported evidence candidates excluded by query filters."),
            None,
        );
    }
    candidates
}

fn append_direct_evidence_pack_items(
    candidates: Vec<DirectEvidencePackCandidate>,
    request: &ContextRequest,
    draft: &mut PackDraft,
    degraded: &mut Vec<ContextResponseDegradation>,
) {
    let selected_memory_ids = draft
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect::<BTreeSet<_>>();
    let mut result_limit_count = 0_usize;
    for candidate in candidates {
        if candidate
            .linked_memory_id
            .as_ref()
            .is_some_and(|memory_id| selected_memory_ids.contains(memory_id))
        {
            continue;
        }
        if request.max_results.is_some_and(|limit| {
            draft.items.len().saturating_add(draft.evidence_items.len()) >= limit as usize
        }) {
            result_limit_count = result_limit_count.saturating_add(1);
            continue;
        }
        let mut item = candidate.item;
        let Some(next_used_tokens) = draft.used_tokens.checked_add(item.estimated_tokens) else {
            continue;
        };
        if next_used_tokens > draft.budget.max_tokens() {
            continue;
        }
        item.rank = u32::try_from(
            draft
                .items
                .len()
                .saturating_add(draft.evidence_items.len())
                .saturating_add(1),
        )
        .unwrap_or(u32::MAX);
        draft.evidence_items.push(item);
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
    push_direct_evidence_result_limit_degradation(degraded, result_limit_count);
}

fn push_direct_evidence_result_limit_degradation(
    degraded: &mut Vec<ContextResponseDegradation>,
    result_limit_count: usize,
) {
    if result_limit_count > 0 {
        push_degradation(
            degraded,
            "context_query_max_results_applied",
            ContextResponseSeverity::Low,
            format!(
                "{result_limit_count} imported evidence candidates excluded by query-file budget.maxResults."
            ),
            Some("Increase budget.maxResults in the query file.".to_owned()),
        );
    }
}

fn direct_evidence_matches_filters(
    span: &crate::db::StoredEvidenceSpan,
    filters: &crate::models::QueryFilters,
) -> bool {
    let trust_class = TrustClass::CassEvidence.as_str();
    if !filters
        .trust
        .matches(trust_class, posture_for_trust_class(trust_class))
        || !filters.matches_tags(&[])
        || !temporal_record_matches(&span.created_at, &span.updated_at, &filters.temporal)
    {
        return false;
    }
    // CASS spans are point-in-time evidence, with no memory validity window
    // or tag membership. Redaction categories are retained at ingestion even
    // though the original secret has already been removed from the excerpt.
    if !filters.redaction.allow_categories.is_empty() {
        let Ok(classes) = serde_json::from_str::<Vec<String>>(&span.redaction_classes_json) else {
            return false;
        };
        if classes
            .iter()
            .any(|class| !filters.redaction.allow_categories.contains(class))
        {
            return false;
        }
    }
    true
}

enum EvidencePackHitResolution {
    Linked {
        memory_id: MemoryId,
        evidence_id: String,
    },
    Direct,
}

/// Resolve imported evidence to its linked memory or defer it to native
/// evidence admission without reporting a failed memory conversion.
///
/// Search-hit metadata is a derived, staleable asset and therefore never
/// authorizes pack hydration. Reload the span, session, and linked memory,
/// verify that the evidence belongs to the requested workspace, and re-run
/// the current positive-admission policy before returning a memory id.
fn resolve_evidence_pack_hit(
    connection: &DbConnection,
    workspace_path: &Path,
    hit: &crate::core::search::SearchHit,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<EvidencePackHitResolution> {
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
        return Some(EvidencePackHitResolution::Direct);
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
        return Some(EvidencePackHitResolution::Linked {
            memory_id,
            evidence_id,
        });
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
    bound_workspace_id: Option<&str>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<PackProvenance> {
    let mut freshness_file_cache = crate::core::memory::EvidenceFreshnessFileCache::default();
    provenance_for_memory_cached(
        memory,
        memory_id,
        workspace_path,
        bound_workspace_id,
        degraded,
        &mut freshness_file_cache,
    )
}

fn provenance_for_memory_cached(
    memory: &StoredMemory,
    memory_id: MemoryId,
    workspace_path: &Path,
    bound_workspace_id: Option<&str>,
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
    let local_workspace = bound_workspace_id == Some(memory.workspace_id.as_str())
        || memory.workspace_id == active_workspace_id
        || context_workspace_path_keys(workspace_path)
            .into_iter()
            .any(|path| {
                memory.workspace_id == stable_context_workspace_id(&path)
                    || memory.workspace_id == crate::core::workspace::stable_workspace_id(&path)
            });
    let note = if local_workspace {
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

/// Delegates to the single taxonomy → section mapping
/// (bd-fallback-relevance-floor-labeling-dlr6a).
///
/// This function previously carried its own byte-identical copy of the match
/// arms in `crate::core::search`. Keeping two copies of the rule that decides
/// which heading an agent reads a memory under meant either could drift
/// silently; there is now one.
fn section_for_memory(memory: &StoredMemory) -> PackSection {
    crate::core::search::pack_section_for_level_and_kind(
        memory.level.as_str(),
        memory.kind.as_str(),
    )
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
#[path = "context_rule_admission_tests.rs"]
mod rule_admission_tests;

include!("context_test_module.rs");
