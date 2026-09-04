//! Global (user-level) memory tier — separate-store design (bd-2vq2z.13).
//!
//! ee memories are workspace-scoped, but a user's durable conventions
//! ("always run `cargo fmt --check` before release", "prefer X over Y",
//! "never use approach Z") should follow them into every repo. This module is
//! the decided core of the cross-workspace / user-global memory tier.
//!
//! # Decision (owner, 2026-06-17, bd-2vq2z.13)
//!
//! The tier is a **separate local store** at `<user-data-root>/global/` (its
//! own `ee.db` + index directory) with full schema/migration parity to a
//! workspace store, surfaced through the existing trust-lane scope machinery
//! ([`crate::models::query::MemoryScope::Global`]). It is local-first: the
//! store lives on the same machine; there is no cloud.
//!
//! This deliberately **supersedes** ADR 0069's rejection of a separate global
//! DB for the user-global tier (ADR 0069 chose a `scope` column inside one
//! store; bd-1bfwa.2, its storage engine, is blocked). See
//! `docs/adr/0083-user-global-memory-store.md`. The existing scope/quota
//! machinery is **reused, not broken**: `MemoryScope::Global` selects the
//! global store, and the house-rules quota
//! ([`crate::core::house_rules`]) bounds the fan-in budget so global rules
//! never crowd out project context.
//!
//! # What lives here
//!
//! This module is the pure, deterministic policy core — testable without a
//! database:
//! - path resolution for the separate global store
//!   ([`GlobalStorePaths`]);
//! - the include / opt-out decision matrix
//!   ([`resolve_global_inclusion`]) — config, per-invocation flag, and
//!   per-workspace participation, each "off" reason distinct so posture is
//!   explainable and never silently dropped;
//! - **surfaced (never silent) cross-lane conflict resolution**
//!   ([`surface_lane_conflicts`]) — the non-negotiable from the bead: a
//!   workspace override is allowed but recorded for audit, and contradictions
//!   keep both sides visible with a conflict marker (assembly must never
//!   resolve a cross-lane contradiction by rank);
//! - budget-bounded fan-in ([`bounded_global_fan_in`]);
//! - the `ee.global_memory.v1` store-metadata block
//!   ([`GlobalMemoryStoreMetadata`]).
//!
//! The CLI / `remember` / `search` / `pack` / `backup` wiring adapts real
//! records to these cores in the follow-on leaves; keeping the policy here
//! makes it unit-testable in isolation.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::core::house_rules::{
    HouseRulesQuotaInput, house_rules_quota, select_within_house_rules_quota,
};
use crate::db::{CreateWorkspaceInput, DbConnection, StoredMemory};

pub use crate::models::GLOBAL_MEMORY_SCHEMA_V1;

/// Degraded code emitted when the global tier is not contributing/consuming
/// (disabled store-wide, the workspace is not participating, or no store exists
/// yet). Mirrors ADR 0069's vocabulary so the two designs report posture with a
/// single code.
pub const GLOBAL_MEMORY_DISABLED_CODE: &str = "global_lane_disabled";

/// Marker substring in the read-path error raised when the user-global store
/// is present but needs a schema migration.
///
/// `core::search` matches on this to report the optional lane as skipped
/// (`global_lane_migration_required`, info) instead of as a failure to verify
/// the scope of the workspace's own results. The error is a plain `String`, so
/// the constant is the only thing keeping producer and consumer from drifting
/// apart silently.
pub const GLOBAL_STORE_NEEDS_MIGRATION_MARKER: &str = "needs migration";

/// The provenance lane label attached to every memory surfaced from the global
/// tier, so an agent always knows a memory came from the user-global store and
/// not the current workspace.
pub const GLOBAL_PROVENANCE_LANE: &str = "global";

/// Directory name of the separate global store under the user data root.
pub const GLOBAL_STORE_DIRNAME: &str = "global";

/// Default ee user-data suffix under `$HOME` when `XDG_DATA_HOME` is absent.
pub const DEFAULT_USER_DATA_SUFFIX: &str = ".local/share/ee";

/// Default fan-in budget for the global tier, in basis points of the pack
/// budget (1500 bp = 15%). A small bounded slice so global rules never crowd
/// out project context; configurable via `[memory] global_fan_in_basis_points`.
pub const DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS: u32 = 1_500;

/// Resolved on-disk locations for the separate global store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalStorePaths {
    /// The global-store root directory (e.g. `~/.local/share/ee/global`).
    pub root: PathBuf,
    /// The global store database (`<root>/ee.db`).
    pub database_path: PathBuf,
    /// The derived global search-index directory (`<root>/indexes`).
    pub index_dir: PathBuf,
}

impl GlobalStorePaths {
    /// Derive the global-store paths from the **user data root** — the
    /// directory the workspace store database lives in by default. With the
    /// built-in layout (`~/.local/share/ee/ee.db`) the global store is
    /// `~/.local/share/ee/global/{ee.db,indexes}`.
    ///
    /// The global store must follow the *user*, not a project-local workspace
    /// DB, so callers pass the resolved user data dir here — never a
    /// project-local `.ee/` path.
    #[must_use]
    pub fn from_data_root(user_data_root: &Path) -> Self {
        Self::from_root(&user_data_root.join(GLOBAL_STORE_DIRNAME))
    }

    /// Build paths from an explicit global-store root (config override).
    #[must_use]
    pub fn from_root(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            database_path: root.join("ee.db"),
            index_dir: root.join("indexes"),
        }
    }

    /// Resolve the global-store paths, honoring an optional configured root
    /// override; otherwise `<user_data_root>/global`.
    #[must_use]
    pub fn resolve(user_data_root: &Path, configured_root: Option<&Path>) -> Self {
        match configured_root {
            Some(root) => Self::from_root(root),
            None => Self::from_data_root(user_data_root),
        }
    }
}

/// Resolve the ee user-data root from environment values without touching the
/// filesystem. `XDG_DATA_HOME=/tmp/data` maps to `/tmp/data/ee`; otherwise
/// `HOME=/home/a` maps to `/home/a/.local/share/ee`.
#[must_use]
pub fn default_user_data_root_from_values(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    xdg_data_home
        .filter(|value| !value.is_empty())
        .map(|root| PathBuf::from(root).join("ee"))
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|root| PathBuf::from(root).join(DEFAULT_USER_DATA_SUFFIX))
        })
}

/// Resolve the current process' default ee user-data root.
///
/// This intentionally uses XDG/HOME rather than a workspace-local `.ee` path:
/// the global tier follows the user across repositories.
pub fn default_user_data_root_from_env() -> Result<PathBuf, String> {
    default_user_data_root_from_values(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .ok_or_else(|| "global memory store requires XDG_DATA_HOME or HOME".to_owned())
}

/// Resolve the current process' default separate global-store paths.
pub fn default_global_store_paths_from_env() -> Result<GlobalStorePaths, String> {
    default_user_data_root_from_env().map(|root| GlobalStorePaths::from_data_root(&root))
}

/// Why the global tier is or is not included in a retrieval. Each "off" reason
/// is distinct so the posture is explainable instead of a silent no-op.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalInclusionReason {
    /// The global tier is active and will be consulted.
    Included,
    /// `[memory] include_global = false` disabled it store-wide.
    DisabledByConfig,
    /// A per-invocation `--no-global` flag disabled it.
    DisabledByFlag,
    /// The workspace opted out of participation (`participate = false`).
    NotParticipating,
    /// No global store exists on disk yet.
    StoreAbsent,
}

impl GlobalInclusionReason {
    /// Stable machine token for the reason.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::DisabledByConfig => "disabled_by_config",
            Self::DisabledByFlag => "disabled_by_flag",
            Self::NotParticipating => "not_participating",
            Self::StoreAbsent => "store_absent",
        }
    }

    /// The degraded code to surface when this reason means the tier is off, or
    /// `None` when the tier is included. All "off" reasons share the
    /// [`GLOBAL_MEMORY_DISABLED_CODE`] posture code.
    #[must_use]
    pub const fn degraded_code(self) -> Option<&'static str> {
        match self {
            Self::Included => None,
            _ => Some(GLOBAL_MEMORY_DISABLED_CODE),
        }
    }
}

/// The resolved include/opt-out decision for one retrieval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalInclusionDecision {
    /// Whether the global tier participates in this retrieval.
    pub included: bool,
    /// The deciding reason (the first failing gate, or `Included`).
    pub reason: GlobalInclusionReason,
}

/// Inputs to the include/opt-out decision.
#[derive(Clone, Copy, Debug)]
pub struct GlobalInclusionInput {
    /// Whether the global store exists on disk.
    pub store_present: bool,
    /// Whether this workspace participates in the global tier
    /// (`participate = true`). The hard privacy boundary from ADR 0069 §5.
    pub participating: bool,
    /// Whether `[memory] include_global` is enabled store-wide.
    pub config_enabled: bool,
    /// Whether the per-invocation `--no-global` flag was set.
    pub no_global_flag: bool,
}

/// Resolve whether the global tier is included.
///
/// Precedence of "off" gates, evaluated in order so the surfaced reason is
/// stable: store presence → participation → config → per-invocation flag. A
/// missing store, a non-participating workspace, a store-wide config opt-out,
/// and a per-call `--no-global` are each reported distinctly; none silently
/// collapses into another.
#[must_use]
pub fn resolve_global_inclusion(input: &GlobalInclusionInput) -> GlobalInclusionDecision {
    let reason = if !input.store_present {
        GlobalInclusionReason::StoreAbsent
    } else if !input.participating {
        GlobalInclusionReason::NotParticipating
    } else if !input.config_enabled {
        GlobalInclusionReason::DisabledByConfig
    } else if input.no_global_flag {
        GlobalInclusionReason::DisabledByFlag
    } else {
        GlobalInclusionReason::Included
    };
    GlobalInclusionDecision {
        included: matches!(reason, GlobalInclusionReason::Included),
        reason,
    }
}

/// Which store a retrieval candidate came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLane {
    /// The current workspace store.
    Workspace,
    /// Team-synced inbound or teammate-attributed rows.
    Team,
    /// The user-global store.
    Global,
}

impl MemoryLane {
    /// Stable machine token for the lane.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Team => "team",
            Self::Global => "global",
        }
    }
}

/// ADR 0086 TC-D16 / bd-1bfwa: lower rank is more specific.
/// Local workspace beats team beats global on overlap.
#[must_use]
pub const fn lane_specificity_rank(lane: MemoryLane) -> u8 {
    match lane {
        MemoryLane::Workspace => 0,
        MemoryLane::Team => 1,
        MemoryLane::Global => 2,
    }
}

/// A minimal view of a retrieval candidate for cross-lane conflict analysis.
///
/// `conflict_key` is a normalized subject (e.g. a rule subject, a shared tag,
/// or a normalized title) the wiring layer derives; `content_hash`
/// distinguishes identical content from divergent content under the same
/// subject. Holding only ids/keys/hashes here keeps the policy decoupled from
/// the full memory record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneCandidate {
    /// The memory id.
    pub id: String,
    /// Which store the candidate came from.
    pub lane: MemoryLane,
    /// Normalized subject used to detect overlap/contradiction across lanes.
    pub conflict_key: String,
    /// Content fingerprint, used to tell corroboration from contradiction.
    pub content_hash: String,
}

/// How a workspace/global pair relate on a shared conflict key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneConflictKind {
    /// Same subject AND same content: repetition is a trust signal. The
    /// workspace row wins; the global row is annotated as corroboration.
    Corroboration,
    /// Same subject, **divergent** content: a genuine contradiction. Both sides
    /// are surfaced with a conflict marker; assembly must NOT resolve it by
    /// rank — hiding exactly the disagreement the user must see.
    Contradiction,
}

impl LaneConflictKind {
    /// Stable machine token for the conflict kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Corroboration => "corroboration",
            Self::Contradiction => "contradiction",
        }
    }
}

/// A surfaced cross-lane relationship between a workspace row and a global row.
///
/// Provenance is always explicit and nothing is silently dropped or merged: for
/// a contradiction both rows stay visible and neither auto-wins; for
/// corroboration the workspace row takes precedence but that decision is
/// recorded (`workspace_overrides`) for audit rather than applied invisibly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneConflict {
    /// The shared subject the two rows collide on.
    pub conflict_key: String,
    /// Corroboration vs contradiction.
    pub kind: LaneConflictKind,
    /// The workspace-store memory id.
    pub workspace_id: String,
    /// The global-store memory id.
    pub global_id: String,
    /// Whether both rows remain visible. Always `true` — the non-negotiable.
    pub both_surfaced: bool,
    /// Whether the workspace row takes recorded precedence. `true` only for
    /// corroboration; `false` for a contradiction (which routes to review).
    pub workspace_overrides: bool,
}

impl LaneConflict {
    /// Render as a redaction-safe JSON marker (ids/keys/kind only).
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "conflictKey": self.conflict_key,
            "kind": self.kind.as_str(),
            "workspaceId": self.workspace_id,
            "globalId": self.global_id,
            "bothSurfaced": self.both_surfaced,
            "workspaceOverrides": self.workspace_overrides,
        })
    }
}

/// Surface cross-lane conflicts between workspace and global candidates.
///
/// Deterministic and insertion-order independent: candidates are grouped by
/// `conflict_key` (via a [`BTreeMap`]); within a shared key, every
/// workspace×global pair is classified. Same content hash → corroboration
/// (workspace wins, recorded; global kept as corroboration); divergent content
/// → contradiction (both surfaced, no silent resolution). Output is sorted by
/// `(conflict_key, workspace_id, global_id)` for byte-stable JSON.
#[must_use]
pub fn surface_lane_conflicts(candidates: &[LaneCandidate]) -> Vec<LaneConflict> {
    let mut by_key: BTreeMap<&str, (Vec<&LaneCandidate>, Vec<&LaneCandidate>)> = BTreeMap::new();
    for candidate in candidates {
        let entry = by_key.entry(candidate.conflict_key.as_str()).or_default();
        match candidate.lane {
            MemoryLane::Workspace => entry.0.push(candidate),
            MemoryLane::Global => entry.1.push(candidate),
            MemoryLane::Team => {}
        }
    }

    let mut out = Vec::new();
    for (key, (workspace_rows, global_rows)) in by_key {
        for workspace in &workspace_rows {
            for global in &global_rows {
                let kind = if workspace.content_hash == global.content_hash {
                    LaneConflictKind::Corroboration
                } else {
                    LaneConflictKind::Contradiction
                };
                out.push(LaneConflict {
                    conflict_key: key.to_owned(),
                    kind,
                    workspace_id: workspace.id.clone(),
                    global_id: global.id.clone(),
                    both_surfaced: true,
                    workspace_overrides: matches!(kind, LaneConflictKind::Corroboration),
                });
            }
        }
    }

    out.sort_by(|a, b| {
        a.conflict_key
            .cmp(&b.conflict_key)
            .then_with(|| a.workspace_id.cmp(&b.workspace_id))
            .then_with(|| a.global_id.cmp(&b.global_id))
    });
    out
}

/// A surfaced relationship between two different lanes on a shared subject.
///
/// Workspace × global pairs still also project through [`LaneConflict`]. This
/// type is the three-lane surface cited by ADR 0086 TC-D16: more-specific
/// context wins on overlap; contradiction keeps both sides and records that
/// neither auto-wins.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrecedenceConflict {
    /// The shared subject the two rows collide on.
    pub conflict_key: String,
    /// Corroboration vs contradiction.
    pub kind: LaneConflictKind,
    /// More-specific lane in the TC-D16 chain.
    pub more_specific_lane: MemoryLane,
    /// Memory id on the more-specific lane.
    pub more_specific_id: String,
    /// Less-specific lane in the TC-D16 chain.
    pub less_specific_lane: MemoryLane,
    /// Memory id on the less-specific lane.
    pub less_specific_id: String,
    /// Whether both rows remain visible. Always `true`.
    pub both_surfaced: bool,
    /// Whether the more-specific row takes recorded precedence. `true` only
    /// for corroboration; `false` for contradiction.
    pub more_specific_overrides: bool,
}

impl PrecedenceConflict {
    /// Render as a redaction-safe JSON marker (ids/keys/kind/lanes only).
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "conflictKey": self.conflict_key,
            "kind": self.kind.as_str(),
            "moreSpecificLane": self.more_specific_lane.as_str(),
            "moreSpecificId": self.more_specific_id,
            "lessSpecificLane": self.less_specific_lane.as_str(),
            "lessSpecificId": self.less_specific_id,
            "bothSurfaced": self.both_surfaced,
            "moreSpecificOverrides": self.more_specific_overrides,
        })
    }
}

/// Surface cross-lane conflicts across workspace, team, and global.
///
/// Deterministic and insertion-order independent. Every pair of distinct
/// lanes that share a `conflict_key` is classified. Same content hash →
/// corroboration (more-specific wins, recorded); divergent content →
/// contradiction (both surfaced, no silent resolution). Output is sorted by
/// `(conflict_key, more_specific_id, less_specific_id, more_specific_lane,
/// less_specific_lane)`.
#[must_use]
pub fn surface_precedence_conflicts(candidates: &[LaneCandidate]) -> Vec<PrecedenceConflict> {
    let mut by_key: BTreeMap<&str, BTreeMap<MemoryLane, Vec<&LaneCandidate>>> = BTreeMap::new();
    for candidate in candidates {
        by_key
            .entry(candidate.conflict_key.as_str())
            .or_default()
            .entry(candidate.lane)
            .or_default()
            .push(candidate);
    }

    let lanes = [MemoryLane::Workspace, MemoryLane::Team, MemoryLane::Global];
    let mut out = Vec::new();
    for (key, by_lane) in by_key {
        for (left_index, left_lane) in lanes.iter().enumerate() {
            for right_lane in lanes.iter().skip(left_index.saturating_add(1)) {
                let Some(left_rows) = by_lane.get(left_lane) else {
                    continue;
                };
                let Some(right_rows) = by_lane.get(right_lane) else {
                    continue;
                };
                for left in left_rows {
                    for right in right_rows {
                        // Iteration order is workspace, team, global, so
                        // `left_lane` is always more specific.
                        let (more, less) = (*left, *right);
                        let kind = if more.content_hash == less.content_hash {
                            LaneConflictKind::Corroboration
                        } else {
                            LaneConflictKind::Contradiction
                        };
                        out.push(PrecedenceConflict {
                            conflict_key: key.to_owned(),
                            kind,
                            more_specific_lane: more.lane,
                            more_specific_id: more.id.clone(),
                            less_specific_lane: less.lane,
                            less_specific_id: less.id.clone(),
                            both_surfaced: true,
                            more_specific_overrides: matches!(
                                kind,
                                LaneConflictKind::Corroboration
                            ),
                        });
                    }
                }
            }
        }
    }

    out.sort_by(|left, right| {
        left.conflict_key
            .cmp(&right.conflict_key)
            .then_with(|| left.more_specific_id.cmp(&right.more_specific_id))
            .then_with(|| left.less_specific_id.cmp(&right.less_specific_id))
            .then_with(|| {
                left.more_specific_lane
                    .as_str()
                    .cmp(right.more_specific_lane.as_str())
            })
            .then_with(|| {
                left.less_specific_lane
                    .as_str()
                    .cmp(right.less_specific_lane.as_str())
            })
    });
    out
}

/// The result of bounding the global tier's fan-in to a share of the budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalFanIn {
    /// The hard token cap for global candidates this pack.
    pub cap_tokens: u64,
    /// `false` when the workspace opted out (cap is then zero).
    pub enabled: bool,
    /// Indices (into the input cost slice) of the selected global candidates,
    /// in caller-supplied priority order.
    pub selected: Vec<usize>,
}

/// Select global candidates whose cumulative token cost stays within a bounded
/// share of the pack budget.
///
/// Reuses the house-rules quota ([`crate::core::house_rules`]) so the global
/// tier obeys the same "never crowd out project context" guarantee. A
/// per-workspace opt-out yields a zero cap and an empty selection.
#[must_use]
pub fn bounded_global_fan_in(
    global_item_token_costs: &[u64],
    total_budget_tokens: u64,
    quota_basis_points: u32,
    opted_out: bool,
) -> GlobalFanIn {
    let quota = house_rules_quota(&HouseRulesQuotaInput {
        total_budget_tokens,
        quota_basis_points,
        workspace_opted_out: opted_out,
    });
    let selected = if quota.enabled {
        select_within_house_rules_quota(global_item_token_costs, quota.cap_tokens)
    } else {
        Vec::new()
    };
    GlobalFanIn {
        cap_tokens: quota.cap_tokens,
        enabled: quota.enabled,
        selected,
    }
}

/// Store-metadata block (`ee.global_memory.v1`) describing the separate global
/// store: where it lives, whether it is enabled/participating, and a content
/// census. Counts/paths/flags only — never memory content (redaction-safe).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalMemoryStoreMetadata {
    /// Absolute path to the global store database.
    pub database_path: String,
    /// Absolute path to the global store's search-index directory.
    pub index_dir: String,
    /// Whether the store exists on disk.
    pub present: bool,
    /// Whether `[memory] include_global` is enabled store-wide.
    pub enabled: bool,
    /// Whether the current workspace participates in the global tier.
    pub participating: bool,
    /// The schema/migration version of the global store (parity with workspace).
    pub schema_version: String,
    /// Number of memories in the global store.
    pub memory_count: u64,
    /// RFC 3339 last-modified timestamp of the store, when known.
    pub last_modified: Option<String>,
}

impl GlobalMemoryStoreMetadata {
    /// Render the redaction-safe `ee.global_memory.v1` metadata block.
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "schema": GLOBAL_MEMORY_SCHEMA_V1,
            "databasePath": self.database_path,
            "indexDir": self.index_dir,
            "present": self.present,
            "enabled": self.enabled,
            "participating": self.participating,
            "schemaVersion": self.schema_version,
            "memoryCount": self.memory_count,
            "lastModified": self.last_modified,
        })
    }
}

/// Display name for the single workspace row inside the user-global store.
pub const GLOBAL_WORKSPACE_NAME: &str = "ee-global";

/// The store-root path string used as the workspace key for the global store.
///
/// `remember`/search derive the workspace id from this same path
/// through the occupancy-aware workspace bind, so writes and reads
/// agree on a single stable global workspace even when the store root
/// is spelled lexically versus canonically.
#[must_use]
pub fn global_workspace_key(paths: &GlobalStorePaths) -> String {
    paths.root.to_string_lossy().into_owned()
}

/// Deterministic workspace id for the global store, matching the id `remember`
/// derives from the store root so the write path (a `remember` pointed at the
/// global db) and the read path resolve the same workspace.
#[must_use]
pub fn global_workspace_id(paths: &GlobalStorePaths) -> String {
    crate::core::curate::stable_workspace_id(&paths.root)
}

/// Open the separate user-global store at `paths`, creating the directory,
/// database, schema, and the single stable global workspace row if absent.
///
/// This is the real storage engine for the user-global memory tier (ADR 0083,
/// bd-1pq3c): the global store is a standalone `ee.db` with full schema parity
/// to a workspace store, so the engine reuses [`DbConnection`]/`migrate` and a
/// deterministic workspace id derived from the store root. It mirrors the
/// `ee init` create→migrate→workspace flow so a `remember` pointed at the
/// returned database writes into the same workspace this returns.
///
/// Idempotent: re-opening an existing store returns the existing workspace id.
///
/// # Errors
///
/// Returns a message when the directory, database, migration, or workspace row
/// cannot be created.
pub fn open_or_create_global_store(
    paths: &GlobalStorePaths,
) -> Result<(DbConnection, String), String> {
    std::fs::create_dir_all(&paths.root).map_err(|error| {
        format!(
            "failed to create global store directory {}: {error}",
            paths.root.display()
        )
    })?;
    let connection = DbConnection::open_file(&paths.database_path)
        .map_err(|error| format!("failed to open global store database: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("failed to migrate global store database: {error}"))?;
    let workspace_id = ensure_global_workspace_row(&connection, paths)?;
    Ok((connection, workspace_id))
}

/// Ensure the single global workspace row exists, returning its id. Resolves an
/// existing row by path first so the id stays stable across calls.
fn ensure_global_workspace_row(
    connection: &DbConnection,
    paths: &GlobalStorePaths,
) -> Result<String, String> {
    let workspace_key = global_workspace_key(paths);
    let requested = global_workspace_id(paths);
    if let Some(existing) = crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[paths.root.as_path()],
    )
    .map_err(|error| format!("failed to check global workspace row: {}", error.message()))?
    {
        return Ok(existing.id);
    }
    connection
        .insert_workspace(
            &requested,
            &CreateWorkspaceInput {
                path: workspace_key,
                name: Some(GLOBAL_WORKSPACE_NAME.to_owned()),
            },
        )
        .map_err(|error| format!("failed to create global workspace row: {error}"))?;
    Ok(requested)
}

/// Read memories persisted in the user-global store (read-only).
///
/// Returns an empty vector when the store does not exist yet (no `remember
/// --global` has run), so callers can include the global tier unconditionally
/// without a pre-existence check. Resolves the workspace by path so it reads
/// exactly what the write path persisted.
///
/// # Errors
///
/// Returns a message when an existing store cannot be opened or queried.
pub fn read_global_store_memories(
    paths: &GlobalStorePaths,
    include_tombstoned: bool,
) -> Result<Vec<StoredMemory>, String> {
    if !paths.database_path.exists() {
        return Ok(Vec::new());
    }
    let connection = DbConnection::open_file_read_only(&paths.database_path)
        .map_err(|error| format!("failed to open global store database read-only: {error}"))?;
    let needs_migration = connection
        .needs_migration()
        .map_err(|error| format!("failed to inspect global store migration state: {error}"))?;
    if needs_migration {
        // Must keep containing `GLOBAL_STORE_NEEDS_MIGRATION_MARKER`; the
        // search read path classifies this case by that substring.
        return Err(
            "global store database needs migration; skipping read-only global tier".to_owned(),
        );
    }
    let requested = global_workspace_id(paths);
    let Some(workspace) = crate::core::workspace::select_existing_workspace_row(
        &connection,
        &requested,
        &[paths.root.as_path()],
    )
    .map_err(|error| {
        format!(
            "failed to resolve global workspace row: {}",
            error.message()
        )
    })?
    else {
        return Ok(Vec::new());
    };
    connection
        .list_memories(&workspace.id, None, include_tombstoned)
        .map_err(|error| format!("failed to list global store memories: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_safe_tempdir(prefix: &str) -> Result<tempfile::TempDir, String> {
        let system_temp = Path::new("/tmp");
        if system_temp.is_dir() {
            let root =
                std::fs::canonicalize(system_temp).unwrap_or_else(|_| system_temp.to_path_buf());
            return tempfile::Builder::new()
                .prefix(prefix)
                .tempdir_in(root)
                .map_err(|error| error.to_string());
        }
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| error.to_string())
    }

    #[test]
    fn global_store_create_is_idempotent_and_persists_memories() -> Result<(), String> {
        use crate::db::CreateMemoryInput;

        let tempdir = path_safe_tempdir("ee-global-store.")?;
        let paths = GlobalStorePaths::from_root(&tempdir.path().join("global"));

        // Reading before any write returns empty — the store does not exist yet,
        // so callers can include the global tier without a pre-existence check.
        assert!(read_global_store_memories(&paths, false)?.is_empty());

        // First open creates the directory, database, schema, and workspace row.
        let (connection, workspace_id) = open_or_create_global_store(&paths)?;
        assert!(paths.database_path.exists());
        assert_eq!(workspace_id, global_workspace_id(&paths));

        // Write a memory into the same store `remember --global` targets.
        connection
            .insert_memory(
                &crate::testing::mem("global"),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "always run cargo fmt --check before release".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.0,
                    importance: 0.0,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["global".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        // Re-opening is idempotent: the workspace id stays stable.
        let (again, workspace_id_again) = open_or_create_global_store(&paths)?;
        assert_eq!(workspace_id, workspace_id_again);
        drop(again);

        // The write persists and is read back from a fresh connection.
        let memories = read_global_store_memories(&paths, false)?;
        assert_eq!(memories.len(), 1);
        assert_eq!(
            memories[0].content,
            "always run cargo fmt --check before release"
        );
        assert_eq!(memories[0].workspace_id, workspace_id);
        Ok(())
    }

    #[test]
    fn read_global_store_memories_reports_pending_migration_without_writes() -> Result<(), String> {
        let tempdir = path_safe_tempdir("ee-global-store-stale.")?;
        let paths = GlobalStorePaths::from_root(&tempdir.path().join("global"));
        std::fs::create_dir_all(&paths.root).map_err(|error| error.to_string())?;

        {
            let connection =
                DbConnection::open_file(&paths.database_path).map_err(|error| error.to_string())?;
            assert!(
                !connection
                    .migration_table_exists()
                    .map_err(|error| error.to_string())?,
                "fresh database must not start with a migration table"
            );
        }

        let Err(error) = read_global_store_memories(&paths, false) else {
            return Err("stale global store read must report pending migration".to_owned());
        };
        assert!(
            error.contains(GLOBAL_STORE_NEEDS_MIGRATION_MARKER),
            "the read path's pending-migration error must keep containing \
             `{GLOBAL_STORE_NEEDS_MIGRATION_MARKER}`; core::search classifies \
             the skipped global lane by that substring. Got {error}"
        );

        let connection = DbConnection::open_file_read_only(&paths.database_path)
            .map_err(|error| error.to_string())?;
        assert!(
            connection
                .needs_migration()
                .map_err(|error| error.to_string())?,
            "read-only global store read must leave migration state pending"
        );
        assert!(
            !connection
                .migration_table_exists()
                .map_err(|error| error.to_string())?,
            "read-only global store read must not create migration metadata"
        );
        Ok(())
    }

    fn candidate(id: &str, lane: MemoryLane, key: &str, hash: &str) -> LaneCandidate {
        LaneCandidate {
            id: id.to_owned(),
            lane,
            conflict_key: key.to_owned(),
            content_hash: hash.to_owned(),
        }
    }

    #[test]
    fn paths_derive_under_global_subdir_of_data_root() {
        let paths = GlobalStorePaths::from_data_root(Path::new("/home/agent/.local/share/ee"));
        assert_eq!(
            paths.root,
            PathBuf::from("/home/agent/.local/share/ee/global")
        );
        assert_eq!(
            paths.database_path,
            PathBuf::from("/home/agent/.local/share/ee/global/ee.db")
        );
        assert_eq!(
            paths.index_dir,
            PathBuf::from("/home/agent/.local/share/ee/global/indexes")
        );
    }

    #[test]
    fn resolve_honors_configured_root_override() {
        let from_root =
            GlobalStorePaths::resolve(Path::new("/data"), Some(Path::new("/custom/global")));
        assert_eq!(
            from_root.database_path,
            PathBuf::from("/custom/global/ee.db")
        );

        let from_default = GlobalStorePaths::resolve(Path::new("/data"), None);
        assert_eq!(
            from_default.database_path,
            PathBuf::from("/data/global/ee.db")
        );
    }

    #[test]
    fn inclusion_decision_matrix_is_explainable() {
        // Fully enabled.
        let on = resolve_global_inclusion(&GlobalInclusionInput {
            store_present: true,
            participating: true,
            config_enabled: true,
            no_global_flag: false,
        });
        assert!(on.included);
        assert_eq!(on.reason, GlobalInclusionReason::Included);
        assert_eq!(on.reason.degraded_code(), None);

        // Absent store dominates every other gate.
        let absent = resolve_global_inclusion(&GlobalInclusionInput {
            store_present: false,
            participating: false,
            config_enabled: false,
            no_global_flag: true,
        });
        assert!(!absent.included);
        assert_eq!(absent.reason, GlobalInclusionReason::StoreAbsent);

        // Participation gate (hard privacy boundary) beats config and flag.
        let opted_out = resolve_global_inclusion(&GlobalInclusionInput {
            store_present: true,
            participating: false,
            config_enabled: true,
            no_global_flag: false,
        });
        assert_eq!(opted_out.reason, GlobalInclusionReason::NotParticipating);

        // Store-wide config opt-out.
        let config_off = resolve_global_inclusion(&GlobalInclusionInput {
            store_present: true,
            participating: true,
            config_enabled: false,
            no_global_flag: false,
        });
        assert_eq!(config_off.reason, GlobalInclusionReason::DisabledByConfig);

        // Per-invocation --no-global.
        let flag_off = resolve_global_inclusion(&GlobalInclusionInput {
            store_present: true,
            participating: true,
            config_enabled: true,
            no_global_flag: true,
        });
        assert_eq!(flag_off.reason, GlobalInclusionReason::DisabledByFlag);

        // Every "off" reason reports the shared posture code.
        for reason in [
            GlobalInclusionReason::DisabledByConfig,
            GlobalInclusionReason::DisabledByFlag,
            GlobalInclusionReason::NotParticipating,
            GlobalInclusionReason::StoreAbsent,
        ] {
            assert_eq!(reason.degraded_code(), Some(GLOBAL_MEMORY_DISABLED_CODE));
        }
    }

    #[test]
    fn identical_content_across_lanes_is_corroboration_workspace_wins() {
        let conflicts = surface_lane_conflicts(&[
            candidate("ws1", MemoryLane::Workspace, "fmt-before-release", "h1"),
            candidate("gl1", MemoryLane::Global, "fmt-before-release", "h1"),
        ]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, LaneConflictKind::Corroboration);
        assert!(
            conflicts[0].both_surfaced,
            "global row stays as corroboration"
        );
        assert!(
            conflicts[0].workspace_overrides,
            "workspace row wins on identical content"
        );
    }

    #[test]
    fn divergent_content_across_lanes_is_a_surfaced_contradiction() {
        let conflicts = surface_lane_conflicts(&[
            candidate(
                "ws1",
                MemoryLane::Workspace,
                "rebase-policy",
                "rebase-always",
            ),
            candidate("gl1", MemoryLane::Global, "rebase-policy", "rebase-never"),
        ]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, LaneConflictKind::Contradiction);
        // The non-negotiable: both surfaced, neither silently wins.
        assert!(conflicts[0].both_surfaced);
        assert!(
            !conflicts[0].workspace_overrides,
            "a contradiction must route to review, not auto-resolve by lane"
        );
    }

    #[test]
    fn unrelated_subjects_do_not_conflict() {
        let conflicts = surface_lane_conflicts(&[
            candidate("ws1", MemoryLane::Workspace, "subject-a", "h1"),
            candidate("gl1", MemoryLane::Global, "subject-b", "h2"),
        ]);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn empty_conflict_input_returns_empty_marker_list() {
        let conflicts = surface_lane_conflicts(&[]);
        assert!(
            conflicts.is_empty(),
            "empty candidate sets must not fabricate global/workspace conflicts"
        );
    }

    #[test]
    fn conflict_surfacing_is_insertion_order_independent() {
        let forward = surface_lane_conflicts(&[
            candidate("ws1", MemoryLane::Workspace, "k", "a"),
            candidate("gl1", MemoryLane::Global, "k", "b"),
            candidate("gl2", MemoryLane::Global, "k", "a"),
        ]);
        let reversed = surface_lane_conflicts(&[
            candidate("gl2", MemoryLane::Global, "k", "a"),
            candidate("gl1", MemoryLane::Global, "k", "b"),
            candidate("ws1", MemoryLane::Workspace, "k", "a"),
        ]);
        assert_eq!(forward, reversed, "conflict output must be deterministic");
        assert_eq!(forward.len(), 2, "one corroboration + one contradiction");
    }

    #[test]
    fn lane_specificity_is_workspace_then_team_then_global() {
        assert!(
            lane_specificity_rank(MemoryLane::Workspace) < lane_specificity_rank(MemoryLane::Team)
        );
        assert!(
            lane_specificity_rank(MemoryLane::Team) < lane_specificity_rank(MemoryLane::Global)
        );
    }

    #[test]
    fn identical_content_across_workspace_and_team_is_local_override() {
        let conflicts = surface_precedence_conflicts(&[
            candidate("ws1", MemoryLane::Workspace, "fmt-before-release", "h1"),
            candidate("tm1", MemoryLane::Team, "fmt-before-release", "h1"),
        ]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, LaneConflictKind::Corroboration);
        assert_eq!(conflicts[0].more_specific_lane, MemoryLane::Workspace);
        assert_eq!(conflicts[0].less_specific_lane, MemoryLane::Team);
        assert!(conflicts[0].both_surfaced);
        assert!(
            conflicts[0].more_specific_overrides,
            "local workspace wins on overlap with team"
        );
    }

    #[test]
    fn identical_content_across_team_and_global_is_team_override() {
        let conflicts = surface_precedence_conflicts(&[
            candidate("tm1", MemoryLane::Team, "fmt-before-release", "h1"),
            candidate("gl1", MemoryLane::Global, "fmt-before-release", "h1"),
        ]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].more_specific_lane, MemoryLane::Team);
        assert_eq!(conflicts[0].less_specific_lane, MemoryLane::Global);
        assert!(conflicts[0].more_specific_overrides);
    }

    #[test]
    fn workspace_team_contradiction_does_not_auto_resolve() {
        let conflicts = surface_precedence_conflicts(&[
            candidate(
                "ws1",
                MemoryLane::Workspace,
                "rebase-policy",
                "rebase-always",
            ),
            candidate("tm1", MemoryLane::Team, "rebase-policy", "rebase-never"),
        ]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, LaneConflictKind::Contradiction);
        assert!(conflicts[0].both_surfaced);
        assert!(
            !conflicts[0].more_specific_overrides,
            "cross-lane contradiction must not silently pick a winner"
        );
    }

    #[test]
    fn three_lane_overlap_is_insertion_order_independent() {
        let forward = surface_precedence_conflicts(&[
            candidate("ws1", MemoryLane::Workspace, "k", "a"),
            candidate("tm1", MemoryLane::Team, "k", "a"),
            candidate("gl1", MemoryLane::Global, "k", "b"),
        ]);
        let reversed = surface_precedence_conflicts(&[
            candidate("gl1", MemoryLane::Global, "k", "b"),
            candidate("tm1", MemoryLane::Team, "k", "a"),
            candidate("ws1", MemoryLane::Workspace, "k", "a"),
        ]);
        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 3);
        assert!(forward.iter().any(|conflict| conflict.more_specific_lane
            == MemoryLane::Workspace
            && conflict.less_specific_lane == MemoryLane::Team
            && conflict.kind == LaneConflictKind::Corroboration));
        assert!(
            forward
                .iter()
                .any(|conflict| conflict.kind == LaneConflictKind::Contradiction
                    && !conflict.more_specific_overrides)
        );
    }

    #[test]
    fn fan_in_respects_bounded_budget() {
        // 1500 bp of 1000 tokens = 150-token cap.
        let fan = bounded_global_fan_in(
            &[100, 100, 40],
            1_000,
            DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS,
            false,
        );
        assert!(fan.enabled);
        assert_eq!(fan.cap_tokens, 150);
        // First item (100) fits; second (100) overflows; third (40) backfills.
        assert_eq!(fan.selected, vec![0, 2]);
    }

    #[test]
    fn fan_in_opt_out_selects_nothing() {
        let fan =
            bounded_global_fan_in(&[10, 10], 10_000, DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS, true);
        assert!(!fan.enabled);
        assert_eq!(fan.cap_tokens, 0);
        assert!(fan.selected.is_empty());
    }

    #[test]
    fn fan_in_zero_budget_selects_nothing_even_when_enabled() {
        let fan = bounded_global_fan_in(&[1, 2, 3], 0, DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS, false);
        assert!(fan.enabled);
        assert_eq!(fan.cap_tokens, 0);
        assert!(
            fan.selected.is_empty(),
            "zero-token packs cannot admit global memories"
        );
    }

    #[test]
    fn store_metadata_json_is_stable_and_redaction_safe() {
        let meta = GlobalMemoryStoreMetadata {
            database_path: "/home/agent/.local/share/ee/global/ee.db".to_owned(),
            index_dir: "/home/agent/.local/share/ee/global/indexes".to_owned(),
            present: true,
            enabled: true,
            participating: true,
            schema_version: "v77".to_owned(),
            memory_count: 12,
            last_modified: Some("2026-06-17T00:00:00Z".to_owned()),
        };
        let value = meta.data_json();
        assert_eq!(value["schema"], GLOBAL_MEMORY_SCHEMA_V1);
        assert_eq!(value["memoryCount"], 12);
        assert_eq!(value["participating"], true);
        assert_eq!(value["databasePath"], meta.database_path);
        // Content is never present in the metadata block.
        assert!(value.get("content").is_none());
    }

    #[test]
    fn provenance_lane_label_is_global() {
        assert_eq!(GLOBAL_PROVENANCE_LANE, "global");
    }
}
