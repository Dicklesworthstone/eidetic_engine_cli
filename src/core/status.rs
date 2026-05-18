//! Status command handler (EE-024).
//!
//! Gathers subsystem status data and returns a structured report that
//! the output layer renders as JSON or human-readable text.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use crate::config::{
    EnvVar, GRAPH_FEATURE_SKYLINE_ENABLED_KEY, WorkspaceDiagnostic, WorkspaceDiagnosticSeverity,
    WorkspaceResolution, WorkspaceResolutionMode, WorkspaceResolutionRequest,
    WorkspaceResolutionSource, diagnose_workspace_resolution, read_env_var, resolve_workspace,
};
use crate::db::{
    CreateWorkspaceInput, DbConnection, FeedbackSourceHarmfulCount, GraphSnapshotStatus,
    GraphSnapshotType, MeshStorageStatus, PROVENANCE_CHAIN_HASH_VERSION,
    PROVENANCE_STATUS_UNVERIFIED, StoredAuditEntry, StoredCurationCandidate,
    StoredCurationTtlPolicy, StoredMemory, StoredMemoryLink, audit_actions,
    default_curation_ttl_policy_id_for_review_state, read_pool::PoolStats,
};
use crate::models::degradation::GRAPH_SKYLINE_DEGENERATE_COMMUNITIES_CODE;
use crate::models::posture::{
    OperationPostureReport, SubsystemPostureReport, SubsystemPostureStatus, WorkspacePostureReport,
};
use crate::models::{CapabilityStatus, MemoryId, SingleFlightPostureReport};
use crate::policy::{MEMORY_DECAY_SOURCE, MemoryDecayThresholds, evaluate_memory_decay};

use super::agent_detect::AgentInventoryReport;
use super::curate::stable_workspace_id;
use super::index::{IndexHealth, IndexStatusOptions, get_index_status};
use super::outcome::{DEFAULT_HARMFUL_BURST_WINDOW_SECONDS, DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR};
use super::tailscale_probe::{
    SystemTailscaleCliProbeRunner, SystemTailscaleSocketProbeRunner,
    TAILSCALE_BINARY_INAUTHENTIC_CODE, TAILSCALE_DAEMON_UNREACHABLE_CODE,
    TAILSCALE_NOT_AUTHENTICATED_CODE, TAILSCALE_NOT_INSTALLED_CODE, TAILSCALE_PROBE_TIMEOUT_CODE,
    TAILSCALE_PROBE_UNAVAILABLE_CODE, TAILSCALE_SHIELDS_UP_CODE, TailscaleCliProbeConfig,
    TailscaleLocalReport, TailscalePlatform, TailscaleSocketProbeConfig,
    probe_tailscale_local_with_runners, tailscale_probe_timeout_ms_from_env_value,
};
use super::{build_info, runtime_status};

const GRAPH_SNAPSHOT_ASSET_NAME: &str = "graph_snapshot_artifact";
const GRAPH_SNAPSHOT_ASSET_KIND: &str = "persisted_snapshot";
const SEARCH_INDEX_ASSET_KIND: &str = "persisted_index";
const GRAPH_SNAPSHOT_PATH: &str = ".ee/graph";
const GRAPH_SNAPSHOT_REFRESH_COMMAND: &str = "ee graph centrality-refresh --workspace .";
const GRAPH_LIVE_COMPUTE_AVAILABLE: &str = "live_compute_available";
#[cfg(not(feature = "graph"))]
const GRAPH_LIVE_COMPUTE_UNAVAILABLE: &str = "live_compute_unavailable";
const SKYLINE_MIN_COMMUNITY_COUNT: usize = 3;
const FNX_RUNTIME_VERSION: &str = "0.1.0";
const GRAPH_COMPUTE_ALGORITHMS: &[&str] = &[
    "pagerank",
    "betweenness",
    "hits",
    "louvain",
    "communities",
    "k_core",
    "articulation",
    "path",
    "explain_link",
    "centrality_refresh",
    "feature_enrichment",
    "neighborhood",
];
const PACK_BUDGET_BUCKET_SCHEMA_V1: &str = "ee.status.pack_budget_buckets.v1";
const PACK_BUDGET_BUCKET_WINDOW_HOURS: u32 = 24;

/// Memory subsystem health status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryHealthStatus {
    /// Memory subsystem is healthy with active memories.
    Healthy,
    /// Memory subsystem is operational but has warnings.
    Degraded,
    /// No memories stored yet.
    Empty,
    /// Memory subsystem is unavailable.
    Unavailable,
}

impl MemoryHealthStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Empty => "empty",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Memory subsystem health report (EE-309).
#[derive(Clone, Debug)]
pub struct MemoryHealthReport {
    /// Overall health status.
    pub status: MemoryHealthStatus,
    /// Total memory count (including tombstoned).
    pub total_count: u32,
    /// Active (non-tombstoned) memory count.
    pub active_count: u32,
    /// Tombstoned memory count.
    pub tombstoned_count: u32,
    /// Memories not accessed in the last 30 days.
    pub stale_count: u32,
    /// Average confidence score (0.0-1.0), None if no memories.
    pub average_confidence: Option<f32>,
    /// Percentage of memories with provenance attached.
    pub provenance_coverage: Option<f32>,
    /// Conservative aggregate health score (0.0-1.0), None if unavailable.
    pub health_score: Option<f32>,
    /// Component scores used to compute the conservative health score.
    pub score_components: Option<MemoryHealthScoreComponents>,
}

/// Deterministic component scores for memory health.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryHealthScoreComponents {
    /// Ratio of non-tombstoned memories to total memories.
    pub active_ratio: f32,
    /// Freshness score after accounting for stale active memories.
    pub freshness_score: f32,
    /// Machine-readable source for the freshness score.
    pub freshness_sourced_from: &'static str,
    /// Average confidence normalized to 0.0-1.0.
    pub confidence_score: f32,
    /// Provenance coverage normalized to 0.0-1.0.
    pub provenance_score: f32,
    /// Tombstoned-memory penalty normalized to 0.0-1.0.
    pub tombstone_penalty: f32,
}

impl MemoryHealthReport {
    /// Gather memory health without a workspace-bound store.
    #[must_use]
    pub fn gather() -> Self {
        Self {
            status: MemoryHealthStatus::Unavailable,
            total_count: 0,
            active_count: 0,
            tombstoned_count: 0,
            stale_count: 0,
            average_confidence: None,
            provenance_coverage: None,
            health_score: None,
            score_components: None,
        }
    }

    /// Recompute conservative score fields from the current metrics.
    #[must_use]
    pub fn with_conservative_score(mut self) -> Self {
        self.score_components = self.conservative_score_components();
        self.health_score = self
            .score_components
            .map(MemoryHealthScoreComponents::health_score);
        self
    }

    fn conservative_score_components(&self) -> Option<MemoryHealthScoreComponents> {
        if self.total_count == 0 {
            return None;
        }

        let active_ratio = bounded_ratio(self.active_count, self.total_count);
        let stale_ratio = if self.active_count == 0 {
            1.0
        } else {
            bounded_ratio(self.stale_count.min(self.active_count), self.active_count)
        };
        let freshness_score = 1.0 - stale_ratio;
        let confidence_score = bounded_score(self.average_confidence);
        let provenance_score = bounded_score(self.provenance_coverage);
        let tombstone_penalty = bounded_ratio(self.tombstoned_count, self.total_count);

        Some(MemoryHealthScoreComponents {
            active_ratio,
            freshness_score,
            freshness_sourced_from: "stale_ratio_legacy",
            confidence_score,
            provenance_score,
            tombstone_penalty,
        })
    }

    /// Create a healthy report for testing.
    #[cfg(test)]
    pub fn healthy_fixture() -> Self {
        Self {
            status: MemoryHealthStatus::Healthy,
            total_count: 100,
            active_count: 95,
            tombstoned_count: 5,
            stale_count: 10,
            average_confidence: Some(0.85),
            provenance_coverage: Some(0.92),
            health_score: None,
            score_components: None,
        }
        .with_conservative_score()
    }
}

impl MemoryHealthScoreComponents {
    /// Conservative aggregate score. Weak components dominate instead of
    /// averaging away missing evidence.
    #[must_use]
    pub fn health_score(self) -> f32 {
        let base_score = self
            .active_ratio
            .min(self.freshness_score)
            .min(self.confidence_score)
            .min(self.provenance_score);
        (base_score * (1.0 - self.tombstone_penalty)).clamp(0.0, 1.0)
    }
}

fn bounded_ratio(count: u32, total: u32) -> f32 {
    if total == 0 {
        return 0.0;
    }

    (count.min(total) as f32 / total as f32).clamp(0.0, 1.0)
}

fn bounded_score(score: Option<f32>) -> f32 {
    score
        .filter(|score| score.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0)
}

/// Curation review queue health status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurationHealthStatus {
    /// The review queue has no open TTL attention items.
    Healthy,
    /// The review queue has due TTL decisions that can be handled deterministically.
    Due,
    /// Harmful/rejected candidates need human attention.
    Escalated,
    /// Queue metrics were gathered but some rows could not be evaluated.
    Degraded,
    /// The queue exists and has no candidates.
    Empty,
    /// No workspace was provided for inspection.
    NotInspected,
    /// Curation storage could not be inspected.
    Unavailable,
}

impl CurationHealthStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Due => "due",
            Self::Escalated => "escalated",
            Self::Degraded => "degraded",
            Self::Empty => "empty",
            Self::NotInspected => "not_inspected",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Read-only health snapshot for curation TTL policies and review queue state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurationHealthReport {
    pub status: CurationHealthStatus,
    pub total_count: u32,
    pub pending_count: u32,
    pub accepted_count: u32,
    pub snoozed_count: u32,
    pub rejected_count: u32,
    pub due_count: u32,
    pub prompt_count: u32,
    pub escalation_count: u32,
    pub blocked_count: u32,
    pub policy_count: u32,
    pub auto_promote_enabled_count: u32,
    pub oldest_pending_age_days: Option<i64>,
    pub mean_review_latency_days: Option<i64>,
    pub next_scheduled_at: Option<String>,
}

impl CurationHealthReport {
    #[must_use]
    pub const fn not_inspected() -> Self {
        Self {
            status: CurationHealthStatus::NotInspected,
            total_count: 0,
            pending_count: 0,
            accepted_count: 0,
            snoozed_count: 0,
            rejected_count: 0,
            due_count: 0,
            prompt_count: 0,
            escalation_count: 0,
            blocked_count: 0,
            policy_count: 0,
            auto_promote_enabled_count: 0,
            oldest_pending_age_days: None,
            mean_review_latency_days: None,
            next_scheduled_at: None,
        }
    }

    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            status: CurationHealthStatus::Unavailable,
            total_count: 0,
            pending_count: 0,
            accepted_count: 0,
            snoozed_count: 0,
            rejected_count: 0,
            due_count: 0,
            prompt_count: 0,
            escalation_count: 0,
            blocked_count: 0,
            policy_count: 0,
            auto_promote_enabled_count: 0,
            oldest_pending_age_days: None,
            mean_review_latency_days: None,
            next_scheduled_at: None,
        }
    }
}

/// Derived asset freshness classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DerivedAssetStatus {
    /// The derived asset was inspected and is current with its source.
    Current,
    /// The source has advanced beyond the derived asset's high watermark.
    Stale,
    /// The asset surface exists, but no artifact has been built yet.
    Empty,
    /// The derived asset is expected but no usable files were found.
    Missing,
    /// The derived asset exists but is not usable.
    Corrupt,
    /// The asset was not inspected because no workspace was supplied.
    NotInspected,
    /// The asset cannot be inspected in the current build or state.
    Unavailable,
    /// The asset is planned but no persistent surface exists yet.
    Unimplemented,
}

impl DerivedAssetStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Empty => "empty",
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
            Self::NotInspected => "not_inspected",
            Self::Unavailable => "unavailable",
            Self::Unimplemented => "unimplemented",
        }
    }
}

/// Read-only freshness report for a rebuildable derived asset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetReport {
    pub name: &'static str,
    pub kind: &'static str,
    pub status: DerivedAssetStatus,
    pub source_high_watermark: Option<u64>,
    pub asset_high_watermark: Option<u64>,
    pub high_watermark_lag: Option<u64>,
    pub path: &'static str,
    pub last_built_at: Option<String>,
    pub memory_graph: Option<GraphSnapshotMemoryGraphReport>,
    pub repair: Option<&'static str>,
}

impl DerivedAssetReport {
    #[must_use]
    pub const fn not_inspected(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            kind: SEARCH_INDEX_ASSET_KIND,
            status: DerivedAssetStatus::NotInspected,
            source_high_watermark: None,
            asset_high_watermark: None,
            high_watermark_lag: None,
            path,
            last_built_at: None,
            memory_graph: None,
            repair: Some("Run `ee status --workspace . --json` to inspect this asset."),
        }
    }

    #[must_use]
    pub const fn unimplemented(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            kind: GRAPH_SNAPSHOT_ASSET_KIND,
            status: DerivedAssetStatus::Unimplemented,
            source_high_watermark: None,
            asset_high_watermark: None,
            high_watermark_lag: None,
            path,
            last_built_at: None,
            memory_graph: None,
            repair: Some("Implement the persistent derived asset before reporting a watermark."),
        }
    }

    #[must_use]
    pub const fn unavailable(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            kind: SEARCH_INDEX_ASSET_KIND,
            status: DerivedAssetStatus::Unavailable,
            source_high_watermark: None,
            asset_high_watermark: None,
            high_watermark_lag: None,
            path,
            last_built_at: None,
            memory_graph: None,
            repair: Some("Run `ee doctor --json` to inspect storage and filesystem access."),
        }
    }

    #[must_use]
    pub fn from_index_status(report: &super::index::IndexStatusReport) -> Self {
        let status = match report.health {
            IndexHealth::Ready => DerivedAssetStatus::Current,
            IndexHealth::Stale => DerivedAssetStatus::Stale,
            IndexHealth::Missing => DerivedAssetStatus::Missing,
            IndexHealth::Corrupt => DerivedAssetStatus::Corrupt,
        };

        Self {
            name: "search_index",
            kind: SEARCH_INDEX_ASSET_KIND,
            status,
            source_high_watermark: report.db_generation,
            asset_high_watermark: report.index_generation,
            high_watermark_lag: high_watermark_lag(report.db_generation, report.index_generation),
            path: ".ee/index",
            last_built_at: report.last_rebuild_at.clone(),
            memory_graph: None,
            repair: report.repair_hint,
        }
    }

    #[must_use]
    pub fn from_graph_snapshot_artifact(report: &GraphSnapshotArtifactReport) -> Self {
        Self {
            name: GRAPH_SNAPSHOT_ASSET_NAME,
            kind: GRAPH_SNAPSHOT_ASSET_KIND,
            status: report.status,
            source_high_watermark: Some(report.memory_graph.generation),
            asset_high_watermark: report.snapshot_generation,
            high_watermark_lag: high_watermark_lag(
                Some(report.memory_graph.generation),
                report.snapshot_generation,
            ),
            path: GRAPH_SNAPSHOT_PATH,
            last_built_at: report.last_built_at.clone(),
            memory_graph: Some(report.memory_graph.clone()),
            repair: Some(GRAPH_SNAPSHOT_REFRESH_COMMAND),
        }
    }
}

fn high_watermark_lag(source: Option<u64>, asset: Option<u64>) -> Option<u64> {
    match (source, asset) {
        (Some(source), Some(asset)) => Some(source.saturating_sub(asset)),
        (Some(source), None) => Some(source),
        _ => None,
    }
}

/// Inputs for workspace-aware status inspection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StatusOptions {
    pub workspace_path: Option<PathBuf>,
}

/// Schema emitted by `ee status --skyline`.
pub const STATUS_SKYLINE_SCHEMA_V1: &str = "ee.status.skyline.v1";

/// Summary block for the status skyline surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusSkylineSummaryReport {
    pub community_count: usize,
    pub highest_risk_community_id: Option<String>,
    pub load_bearing_memory_count: usize,
    pub stale_community_count: usize,
}

/// One status-skyline community row.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusSkylineCommunityReport {
    pub community_id: String,
    pub memory_count: usize,
    pub mean_trust: f32,
    pub mean_age_days: f32,
    pub onion_layer: u32,
    pub structural_health: String,
}

/// A schema-valid status skyline report.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusSkylineReport {
    pub schema: &'static str,
    pub snapshot_version: u64,
    pub summary: StatusSkylineSummaryReport,
    pub skyline: Vec<StatusSkylineCommunityReport>,
    pub degraded: Vec<DegradationReport>,
}

impl StatusSkylineReport {
    /// Gather the currently available status skyline posture for a workspace.
    #[must_use]
    pub fn gather_for_workspace(workspace_path: Option<&Path>) -> Self {
        let skyline_feature_enabled = status_skyline_feature_enabled(workspace_path);
        let skyline_community_count = if skyline_feature_enabled == Some(true) {
            gather_status_skyline_community_count(workspace_path)
        } else {
            None
        };
        let mut degraded = Vec::new();
        push_status_skyline_feature_disabled_degradation(&mut degraded, skyline_feature_enabled);
        push_skyline_degenerate_communities_degradation(&mut degraded, skyline_community_count);

        Self {
            schema: STATUS_SKYLINE_SCHEMA_V1,
            snapshot_version: 0,
            summary: StatusSkylineSummaryReport {
                community_count: skyline_community_count.unwrap_or(0),
                highest_risk_community_id: None,
                load_bearing_memory_count: 0,
                stale_community_count: 0,
            },
            skyline: Vec::new(),
            degraded,
        }
    }
}

/// Live graph algorithm readiness, independent of any persisted snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphComputeStatus {
    Available,
    Degraded,
    Unavailable,
}

impl GraphComputeStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Status for live FrankenNetworkX-backed graph computation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphComputeReport {
    pub status: GraphComputeStatus,
    pub available_algorithms: &'static [&'static str],
    pub live_compute_supported: bool,
    pub fnx_runtime_version: &'static str,
    pub result_cache: GraphAlgorithmResultCacheReport,
    pub last_used_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphAlgorithmResultCacheReport {
    pub status: &'static str,
    pub cached_result_count: u32,
    pub observed_compute_count: u32,
    pub cache_hit_rate_basis_points: Option<u32>,
}

impl GraphAlgorithmResultCacheReport {
    #[must_use]
    pub const fn not_inspected() -> Self {
        Self {
            status: "not_inspected",
            cached_result_count: 0,
            observed_compute_count: 0,
            cache_hit_rate_basis_points: None,
        }
    }

    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            status: "unavailable",
            cached_result_count: 0,
            observed_compute_count: 0,
            cache_hit_rate_basis_points: None,
        }
    }
}

/// Current memory-link graph facts exposed with the snapshot artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphSnapshotMemoryGraphReport {
    pub node_count: u32,
    pub edge_count: u32,
    pub generation: u64,
    pub matches_db_generation: bool,
    pub availability: &'static str,
}

/// Persisted graph snapshot freshness, separate from live compute readiness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphSnapshotArtifactReport {
    pub status: DerivedAssetStatus,
    pub last_built_at: Option<String>,
    pub snapshot_path: Option<&'static str>,
    pub snapshot_generation: Option<u64>,
    pub memory_graph: GraphSnapshotMemoryGraphReport,
    pub next_refresh_via: &'static str,
}

/// Workspace selection and ambiguity diagnostics for status output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceStatusReport {
    pub source: WorkspaceResolutionSource,
    pub root: PathBuf,
    pub config_dir: PathBuf,
    pub marker_present: bool,
    pub canonical_root: PathBuf,
    pub fingerprint: String,
    pub scope_kind: String,
    pub repository_root: Option<PathBuf>,
    pub repository_fingerprint: Option<String>,
    pub subproject_path: Option<PathBuf>,
    pub diagnostics: Vec<WorkspaceDiagnosticReport>,
}

impl WorkspaceStatusReport {
    fn from_resolution(
        resolution: WorkspaceResolution,
        diagnostics: Vec<WorkspaceDiagnostic>,
    ) -> Self {
        Self {
            source: resolution.source,
            root: resolution.location.root,
            config_dir: resolution.location.config_dir,
            marker_present: resolution.marker_present,
            canonical_root: resolution.canonical_root,
            fingerprint: resolution.fingerprint,
            scope_kind: resolution.scope.kind.as_str().to_string(),
            repository_root: resolution.scope.repository_root,
            repository_fingerprint: resolution.scope.repository_fingerprint,
            subproject_path: resolution.scope.subproject_path,
            diagnostics: diagnostics
                .into_iter()
                .map(WorkspaceDiagnosticReport::from)
                .collect(),
        }
    }
}

/// A stable, renderable workspace diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceDiagnosticReport {
    pub code: &'static str,
    pub severity: WorkspaceDiagnosticSeverity,
    pub message: String,
    pub repair: String,
    pub selected_source: Option<WorkspaceResolutionSource>,
    pub selected_root: Option<PathBuf>,
    pub conflicting_source: Option<WorkspaceResolutionSource>,
    pub conflicting_root: Option<PathBuf>,
    pub marker_roots: Vec<PathBuf>,
}

impl From<WorkspaceDiagnostic> for WorkspaceDiagnosticReport {
    fn from(diagnostic: WorkspaceDiagnostic) -> Self {
        Self {
            code: diagnostic.code,
            severity: diagnostic.severity,
            message: diagnostic.message,
            repair: diagnostic.repair,
            selected_source: diagnostic.selected_source,
            selected_root: diagnostic.selected_root,
            conflicting_source: diagnostic.conflicting_source,
            conflicting_root: diagnostic.conflicting_root,
            marker_roots: diagnostic.marker_roots,
        }
    }
}

/// Describes the readiness of each ee subsystem.
#[derive(Clone, Debug)]
pub struct CapabilityReport {
    pub runtime: CapabilityStatus,
    pub storage: CapabilityStatus,
    pub search: CapabilityStatus,
    pub mesh: CapabilityStatus,
    pub output_toon: CapabilityStatus,
    pub agent_detection: CapabilityStatus,
}

impl CapabilityReport {
    #[must_use]
    pub fn gather() -> Self {
        let workspace_path = default_workspace_path();
        Self::gather_with_workspace(workspace_path.as_deref())
    }

    #[must_use]
    pub fn gather_for_workspace(workspace_path: &Path) -> Self {
        Self::gather_with_workspace(Some(workspace_path))
    }

    #[must_use]
    pub fn gather_with_workspace(workspace_path: Option<&Path>) -> Self {
        Self {
            runtime: probe_runtime_capability(),
            storage: probe_storage_capability(workspace_path),
            search: probe_search_capability(workspace_path),
            mesh: probe_mesh_capability(),
            output_toon: probe_toon_output_capability(),
            agent_detection: CapabilityStatus::Ready,
        }
    }
}

#[must_use]
pub fn default_workspace_path() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

#[must_use]
pub fn probe_storage_capability(workspace_path: Option<&Path>) -> CapabilityStatus {
    if diag_forced_capability_gap("storage") {
        return CapabilityStatus::Unimplemented;
    }

    let Some(workspace_path) = workspace_path else {
        return CapabilityStatus::Pending;
    };

    let database_path = workspace_database_path(workspace_path);
    if !database_path.exists() {
        return CapabilityStatus::Pending;
    }

    match DbConnection::open_file(&database_path).and_then(|connection| {
        connection.ping()?;
        connection.needs_migration()
    }) {
        Ok(false) => CapabilityStatus::Ready,
        Ok(true) | Err(_) => CapabilityStatus::Degraded,
    }
}

#[must_use]
pub fn probe_search_capability(workspace_path: Option<&Path>) -> CapabilityStatus {
    if diag_forced_capability_gap("search") {
        return CapabilityStatus::Unimplemented;
    }

    let Some(workspace_path) = workspace_path else {
        return CapabilityStatus::Pending;
    };

    match probe_storage_capability(Some(workspace_path)) {
        CapabilityStatus::Ready => {}
        CapabilityStatus::Pending => return CapabilityStatus::Pending,
        CapabilityStatus::Degraded | CapabilityStatus::Unimplemented => {
            return CapabilityStatus::Degraded;
        }
    }

    let options = IndexStatusOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: None,
        index_dir: None,
    };

    match get_index_status(&options) {
        Ok(report) if report.health == IndexHealth::Ready => CapabilityStatus::Ready,
        Ok(_) | Err(_) => CapabilityStatus::Degraded,
    }
}

#[must_use]
pub fn probe_toon_output_capability() -> CapabilityStatus {
    if crate::output::toon_output_available() {
        CapabilityStatus::Ready
    } else {
        CapabilityStatus::Degraded
    }
}

fn workspace_database_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("ee.db")
}

#[must_use]
pub fn probe_cass_capability() -> CapabilityStatus {
    cass_discovery_to_capability(crate::cass::discover_import_binary(None))
}

fn cass_discovery_to_capability(
    discovery: Result<crate::cass::DiscoveredBinary, crate::cass::CassError>,
) -> CapabilityStatus {
    match discovery {
        Ok(_) => CapabilityStatus::Ready,
        Err(crate::cass::CassError::BinaryNotFound { .. }) => CapabilityStatus::Pending,
        Err(_) => CapabilityStatus::Degraded,
    }
}

#[must_use]
pub fn probe_runtime_capability() -> CapabilityStatus {
    if diag_forced_capability_gap("runtime") {
        return CapabilityStatus::Unimplemented;
    }

    match super::build_cli_runtime() {
        Ok(_) => CapabilityStatus::Ready,
        Err(_) => CapabilityStatus::Degraded,
    }
}

#[must_use]
pub fn probe_graph_capability() -> CapabilityStatus {
    if diag_forced_capability_gap("graph") {
        return CapabilityStatus::Unimplemented;
    }

    if cfg!(feature = "graph") {
        CapabilityStatus::Ready
    } else {
        CapabilityStatus::Pending
    }
}

#[must_use]
pub fn probe_mesh_capability() -> CapabilityStatus {
    if diag_forced_capability_gap("mesh") {
        return CapabilityStatus::Unimplemented;
    }

    match read_env_var(EnvVar::MeshEnabled).as_deref() {
        Some("true") => CapabilityStatus::Unimplemented,
        _ => CapabilityStatus::Pending,
    }
}

#[must_use]
pub fn diag_forced_capability_gap(capability: &str) -> bool {
    let Some(raw) = read_env_var(EnvVar::DiagForceCapabilityGap) else {
        return false;
    };

    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| part.eq_ignore_ascii_case(capability) || part.eq_ignore_ascii_case("all"))
}

/// Runtime engine details.
#[derive(Clone, Debug)]
pub struct RuntimeReport {
    pub engine: &'static str,
    pub profile: &'static str,
    pub worker_threads: usize,
    pub async_boundary: &'static str,
}

impl RuntimeReport {
    #[must_use]
    pub fn gather() -> Self {
        let status = runtime_status();
        Self {
            engine: status.engine,
            profile: status.profile.as_str(),
            worker_threads: status.worker_threads(),
            async_boundary: status.async_boundary,
        }
    }
}

/// Process-local read-pool counters exposed by `ee status --json`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadPoolStatusReport {
    pub active: usize,
    pub idle: usize,
    pub active_pins: usize,
    pub expired_pins: usize,
    pub max_seen: usize,
    pub drops: u64,
    pub release_failures: u64,
    pub ad_hoc_bypass_count: u64,
    pub acquire_wait: ReadPoolAcquireWaitReport,
}

/// Sliding-window read-pool acquire wait summary exposed by `ee status --json`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadPoolAcquireWaitReport {
    pub samples: usize,
    pub p50_ns: u128,
    pub p99_ns: u128,
}

impl ReadPoolStatusReport {
    #[must_use]
    pub const fn gather() -> Self {
        Self {
            active: 0,
            idle: 0,
            active_pins: 0,
            expired_pins: 0,
            max_seen: 0,
            drops: 0,
            release_failures: 0,
            ad_hoc_bypass_count: 0,
            acquire_wait: ReadPoolAcquireWaitReport {
                samples: 0,
                p50_ns: 0,
                p99_ns: 0,
            },
        }
    }
}

impl From<PoolStats> for ReadPoolStatusReport {
    fn from(stats: PoolStats) -> Self {
        Self {
            active: stats.active,
            idle: stats.idle,
            active_pins: stats.active_pins,
            expired_pins: stats.expired_pins,
            max_seen: stats.max_seen,
            drops: stats.drops,
            release_failures: stats.release_failures,
            ad_hoc_bypass_count: stats.ad_hoc_bypass_count,
            acquire_wait: ReadPoolAcquireWaitReport {
                samples: stats.acquire_wait.samples,
                p50_ns: stats.acquire_wait.p50_ns,
                p99_ns: stats.acquire_wait.p99_ns,
            },
        }
    }
}

/// Feedback-loop health status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackHealthStatus {
    /// Feedback storage is readable and no quarantine review is pending.
    Healthy,
    /// Quarantined feedback is awaiting review.
    ReviewQueued,
    /// No workspace was provided for inspection.
    NotInspected,
    /// Feedback storage could not be inspected.
    Unavailable,
}

impl FeedbackHealthStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::ReviewQueued => "review_queued",
            Self::NotInspected => "not_inspected",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Harmful feedback count for one source in the active burst window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackSourceHealth {
    pub source_id: String,
    pub harmful_count: u32,
}

impl From<FeedbackSourceHarmfulCount> for FeedbackSourceHealth {
    fn from(value: FeedbackSourceHarmfulCount) -> Self {
        Self {
            source_id: redact_feedback_health_source_id(&value.source_id),
            harmful_count: value.harmful_count,
        }
    }
}

fn redact_feedback_health_source_id(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_feedback_health_source_path_segments(&secret_redacted)
}

fn redact_feedback_health_source_path_segments(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some((relative_index, _)) = value[cursor..].char_indices().find(|(_, c)| *c == '/')
        else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_index;
        if !feedback_health_source_path_starts_sensitive_segment(&value[start..]) {
            output.push_str(&value[cursor..=start]);
            cursor = start + 1;
            continue;
        }

        output.push_str(&value[cursor..start]);
        output.push_str("[REDACTED_PATH]");
        cursor = value[start..]
            .char_indices()
            .find_map(|(index, c)| feedback_health_source_path_boundary(c).then_some(start + index))
            .unwrap_or(value.len());
    }
    output
}

fn feedback_health_source_path_starts_sensitive_segment(value: &str) -> bool {
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

    PREFIXES.iter().any(|prefix| value.starts_with(prefix))
}

fn feedback_health_source_path_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '?' | '#' | '"' | '\'' | ')' | ']' | '}' | ',' | ';')
}

/// Read-only feedback health snapshot for `ee status --json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackHealthReport {
    pub status: FeedbackHealthStatus,
    pub harmful_per_source_per_hour: u32,
    pub harmful_burst_window_seconds: u32,
    pub per_source_harmful_counts: Vec<FeedbackSourceHealth>,
    pub quarantine_queue_depth: u32,
    pub protected_rule_count: u32,
    pub last_inversion_event: Option<String>,
    pub next_deterministic_action: String,
}

impl FeedbackHealthReport {
    #[must_use]
    pub fn not_inspected() -> Self {
        Self {
            status: FeedbackHealthStatus::NotInspected,
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
            per_source_harmful_counts: Vec::new(),
            quarantine_queue_depth: 0,
            protected_rule_count: 0,
            last_inversion_event: None,
            next_deterministic_action: "provide --workspace to inspect feedback health".to_owned(),
        }
    }

    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            status: FeedbackHealthStatus::Unavailable,
            next_deterministic_action: "run ee init --workspace .".to_owned(),
            ..Self::not_inspected()
        }
    }
}

/// A single degradation notice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DegradationReport {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

/// Redaction-safe mesh persistence posture for status surfaces.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshStorageStatusReport {
    pub peer_count: u32,
    pub cursor_count: u32,
    pub imported_event_count: u32,
    pub policy_decision_event_count: u32,
    pub policy_failure_event_count: u32,
    pub mapped_memory_count: u32,
    pub cached_body_count: u32,
}

impl MeshStorageStatusReport {
    fn add(&mut self, status: &MeshStorageStatus) {
        self.peer_count = self.peer_count.saturating_add(status.peer_count);
        self.cursor_count = self.cursor_count.saturating_add(status.cursor_count);
        self.imported_event_count = self
            .imported_event_count
            .saturating_add(status.imported_event_count);
        self.policy_decision_event_count = self
            .policy_decision_event_count
            .saturating_add(status.policy_decision_event_count);
        self.policy_failure_event_count = self
            .policy_failure_event_count
            .saturating_add(status.policy_failure_event_count);
        self.mapped_memory_count = self
            .mapped_memory_count
            .saturating_add(status.mapped_memory_count);
        self.cached_body_count = self
            .cached_body_count
            .saturating_add(status.cached_body_count);
    }

    #[must_use]
    pub const fn has_rows(&self) -> bool {
        self.peer_count > 0
            || self.cursor_count > 0
            || self.imported_event_count > 0
            || self.policy_decision_event_count > 0
            || self.policy_failure_event_count > 0
            || self.mapped_memory_count > 0
            || self.cached_body_count > 0
    }
}

/// Full status report returned by the status command.
#[derive(Clone, Debug)]
pub struct StatusReport {
    pub version: &'static str,
    pub workspace: Option<WorkspaceStatusReport>,
    pub posture: WorkspacePostureReport,
    pub capabilities: CapabilityReport,
    pub runtime: RuntimeReport,
    pub read_pool: ReadPoolStatusReport,
    pub pack_budget_buckets: PackBudgetBucketReport,
    pub qos_posture: super::qos::QosLaneSummary,
    pub memory_health: MemoryHealthReport,
    pub curation_health: CurationHealthReport,
    pub feedback_health: FeedbackHealthReport,
    pub singleflight_posture: SingleFlightPostureReport,
    pub graph_compute: GraphComputeReport,
    pub graph_snapshot_artifact: GraphSnapshotArtifactReport,
    pub derived_assets: Vec<DerivedAssetReport>,
    pub mesh_storage: Option<MeshStorageStatusReport>,
    pub tailscale_local: Option<TailscaleLocalReport>,
    pub agent_inventory: AgentInventoryReport,
    pub degradations: Vec<DegradationReport>,
}

/// Last-24h context-pack token-budget bucket counts for tuning adaptive packs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackBudgetBucketReport {
    pub schema: &'static str,
    pub window_hours: u32,
    pub total_invocations: u32,
    pub adaptive_invocations: u32,
    pub non_adaptive_invocations: u32,
    pub below_one_k: u32,
    pub one_to_two_k: u32,
    pub two_to_four_k: u32,
    pub four_to_eight_k: u32,
    pub eight_k_plus: u32,
}

impl Default for PackBudgetBucketReport {
    fn default() -> Self {
        Self {
            schema: PACK_BUDGET_BUCKET_SCHEMA_V1,
            window_hours: PACK_BUDGET_BUCKET_WINDOW_HOURS,
            total_invocations: 0,
            adaptive_invocations: 0,
            non_adaptive_invocations: 0,
            below_one_k: 0,
            one_to_two_k: 0,
            two_to_four_k: 0,
            four_to_eight_k: 0,
            eight_k_plus: 0,
        }
    }
}

impl StatusReport {
    /// Gather current subsystem status, defaulting to current directory as
    /// workspace when available.
    #[must_use]
    pub fn gather() -> Self {
        Self::gather_with_options(&StatusOptions {
            workspace_path: default_workspace_path(),
        })
    }

    /// Gather current subsystem status and inspect rebuildable assets when
    /// an explicit workspace is available.
    #[must_use]
    pub fn gather_for_workspace(workspace_path: &Path) -> Self {
        Self::gather_with_options(&StatusOptions {
            workspace_path: Some(workspace_path.to_path_buf()),
        })
    }

    /// Gather current subsystem status with explicit options.
    #[must_use]
    pub fn gather_with_options(options: &StatusOptions) -> Self {
        let capabilities =
            CapabilityReport::gather_with_workspace(options.workspace_path.as_deref());
        let runtime = RuntimeReport::gather();
        let read_pool = ReadPoolStatusReport::gather();
        let pack_budget_buckets = gather_pack_budget_buckets(options.workspace_path.as_deref());
        let qos_posture = gather_qos_posture(options.workspace_path.as_deref());
        let (memory_health, memory_health_degradations) =
            gather_memory_health(options.workspace_path.as_deref());
        let workspace = gather_workspace_status(options.workspace_path.as_deref());
        let graph_compute = gather_graph_compute(options.workspace_path.as_deref());
        let graph_snapshot_artifact =
            gather_graph_snapshot_artifact(options.workspace_path.as_deref());
        let skyline_feature_enabled =
            status_skyline_feature_enabled(options.workspace_path.as_deref());
        let skyline_community_count = if skyline_feature_enabled == Some(true) {
            gather_status_skyline_community_count(options.workspace_path.as_deref())
        } else {
            None
        };
        let derived_assets =
            gather_derived_assets(options.workspace_path.as_deref(), &graph_snapshot_artifact);
        let (curation_health, curation_degradations) =
            gather_curation_health(options.workspace_path.as_deref());
        let (feedback_health, feedback_degradations) =
            gather_feedback_health(options.workspace_path.as_deref());
        let singleflight_posture = super::singleflight::singleflight_posture_report();
        let mesh_storage = gather_mesh_storage_status(options.workspace_path.as_deref());
        let tailscale_local = gather_tailscale_local_report();
        let agent_inventory = AgentInventoryReport::not_inspected();

        let mut degradations = Vec::new();

        push_runtime_capability_degradation(&mut degradations, capabilities.runtime);
        push_storage_capability_degradation(
            &mut degradations,
            capabilities.storage,
            options.workspace_path.as_deref(),
        );
        push_search_capability_degradation(
            &mut degradations,
            capabilities.search,
            options.workspace_path.as_deref(),
        );
        push_graph_capability_degradation(&mut degradations, graph_compute.status);
        push_status_skyline_feature_disabled_degradation(
            &mut degradations,
            skyline_feature_enabled,
        );
        push_skyline_degenerate_communities_degradation(&mut degradations, skyline_community_count);
        push_toon_output_capability_degradation(&mut degradations, capabilities.output_toon);

        degradations.extend(memory_health_degradations);
        degradations.extend(curation_degradations);
        degradations.extend(feedback_degradations);
        if let Some(tailscale) = tailscale_local.as_ref() {
            push_tailscale_local_degradations(&mut degradations, tailscale);
        }

        let posture = status_posture_report(
            options,
            &capabilities,
            &memory_health,
            &curation_health,
            &feedback_health,
            &singleflight_posture,
            &graph_compute,
            &derived_assets,
            &degradations,
        );

        Self {
            version: build_info().version,
            workspace,
            posture,
            capabilities,
            runtime,
            read_pool,
            pack_budget_buckets,
            qos_posture,
            memory_health,
            curation_health,
            feedback_health,
            singleflight_posture,
            graph_compute,
            graph_snapshot_artifact,
            derived_assets,
            mesh_storage,
            tailscale_local,
            agent_inventory,
            degradations,
        }
    }
}

fn gather_mesh_storage_status(workspace_path: Option<&Path>) -> Option<MeshStorageStatusReport> {
    let workspace_path = workspace_path?;
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return None;
    }
    let connection = DbConnection::open_file(&database_path).ok()?;
    gather_mesh_storage_status_from_connection(&connection, workspace_path)
}

fn gather_mesh_storage_status_from_connection(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Option<MeshStorageStatusReport> {
    let mut report = MeshStorageStatusReport::default();
    for workspace_id in resolve_status_workspace_ids(connection, workspace_path) {
        let status = connection.mesh_storage_status(&workspace_id).ok()?;
        report.add(&status);
    }
    Some(report)
}

fn gather_pack_budget_buckets(workspace_path: Option<&Path>) -> PackBudgetBucketReport {
    let Some(workspace_path) = workspace_path else {
        return PackBudgetBucketReport::default();
    };
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return PackBudgetBucketReport::default();
    }
    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return PackBudgetBucketReport::default();
    };
    let Ok(entries) = connection.list_audit_entries(Some(&workspace_id), None) else {
        return PackBudgetBucketReport::default();
    };
    pack_budget_buckets_from_audit_entries(&entries, Utc::now())
}

fn pack_budget_buckets_from_audit_entries(
    entries: &[StoredAuditEntry],
    now: DateTime<Utc>,
) -> PackBudgetBucketReport {
    let window_start = now - ChronoDuration::hours(i64::from(PACK_BUDGET_BUCKET_WINDOW_HOURS));
    let mut report = PackBudgetBucketReport::default();
    for entry in entries {
        if entry.action != audit_actions::PACK_ASSEMBLED {
            continue;
        }
        let Ok(timestamp) = DateTime::parse_from_rfc3339(&entry.timestamp) else {
            continue;
        };
        if timestamp.with_timezone(&Utc) < window_start {
            continue;
        }
        let Some((budget, adaptive)) = entry
            .details
            .as_deref()
            .and_then(pack_budget_from_audit_details)
        else {
            continue;
        };
        report.total_invocations = report.total_invocations.saturating_add(1);
        if adaptive {
            report.adaptive_invocations = report.adaptive_invocations.saturating_add(1);
        } else {
            report.non_adaptive_invocations = report.non_adaptive_invocations.saturating_add(1);
        }
        match budget {
            0..=999 => report.below_one_k = report.below_one_k.saturating_add(1),
            1_000..=1_999 => report.one_to_two_k = report.one_to_two_k.saturating_add(1),
            2_000..=3_999 => report.two_to_four_k = report.two_to_four_k.saturating_add(1),
            4_000..=7_999 => report.four_to_eight_k = report.four_to_eight_k.saturating_add(1),
            _ => report.eight_k_plus = report.eight_k_plus.saturating_add(1),
        }
    }
    report
}

fn pack_budget_from_audit_details(details: &str) -> Option<(u32, bool)> {
    let value = serde_json::from_str::<serde_json::Value>(details).ok()?;
    let budget = value
        .get("budget")
        .or_else(|| value.get("maxTokens"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())?;
    let adaptive = value
        .get("adaptiveBudget")
        .and_then(|adaptive| adaptive.get("adaptive"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some((budget, adaptive))
}

fn gather_qos_posture(workspace_path: Option<&Path>) -> super::qos::QosLaneSummary {
    let workspace = workspace_path.unwrap_or_else(|| Path::new("."));
    let workspace_identity = workspace
        .to_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(".");
    let now_epoch_ms = Utc::now().timestamp_millis().try_into().unwrap_or_default();
    super::qos::summarize_qos_lane_registry(workspace, workspace_identity, now_epoch_ms)
}

fn gather_tailscale_local_report() -> Option<TailscaleLocalReport> {
    if !mesh_enabled_for_tailscale_probe() {
        return None;
    }

    let timeout_ms = tailscale_probe_timeout_ms_from_env_value(
        read_env_var(EnvVar::TailscaleProbeTimeoutMs).as_deref(),
    );
    let mut config = TailscaleCliProbeConfig::mesh_enabled();
    config.timeout_ms = timeout_ms;
    config.binary_override = read_env_var(EnvVar::TailscaleBinaryOverride).map(PathBuf::from);
    config.platform_hint = current_tailscale_platform();

    let mut socket_config = TailscaleSocketProbeConfig::mesh_enabled();
    socket_config.timeout_ms = timeout_ms;
    socket_config.platform_hint = current_tailscale_platform();
    apply_tailscale_socket_override(
        &mut socket_config,
        read_env_var(EnvVar::TailscaleProbeSocketOverride),
    );

    let mut socket_runner = SystemTailscaleSocketProbeRunner;
    let mut cli_runner = SystemTailscaleCliProbeRunner;
    Some(probe_tailscale_local_with_runners(
        &socket_config,
        &config,
        &mut socket_runner,
        &mut cli_runner,
    ))
}

fn apply_tailscale_socket_override(
    config: &mut TailscaleSocketProbeConfig,
    override_path: Option<String>,
) {
    let Some(override_path) = override_path
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    config.socket_candidates = vec![PathBuf::from(override_path)];
}

fn mesh_enabled_for_tailscale_probe() -> bool {
    read_env_var(EnvVar::MeshEnabled)
        .as_deref()
        .is_some_and(matches_truthy_env)
}

fn matches_truthy_env(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn current_tailscale_platform() -> TailscalePlatform {
    if cfg!(target_os = "linux") {
        TailscalePlatform::Linux
    } else if cfg!(target_os = "macos") {
        TailscalePlatform::MacosOpen
    } else if cfg!(target_os = "windows") {
        TailscalePlatform::Windows
    } else {
        TailscalePlatform::Other
    }
}

fn push_tailscale_local_degradations(
    degradations: &mut Vec<DegradationReport>,
    report: &TailscaleLocalReport,
) {
    for degradation in &report.degradations {
        degradations.push(tailscale_degradation_report(degradation.code));
    }
}

fn tailscale_degradation_report(code: &'static str) -> DegradationReport {
    match code {
        TAILSCALE_NOT_INSTALLED_CODE => DegradationReport {
            code: TAILSCALE_NOT_INSTALLED_CODE,
            severity: "warning",
            message: "Tailscale binary and local daemon socket were not found.",
            repair: "Install Tailscale, then run tailscale up if you want optional mesh memory.",
        },
        TAILSCALE_DAEMON_UNREACHABLE_CODE => DegradationReport {
            code: TAILSCALE_DAEMON_UNREACHABLE_CODE,
            severity: "warning",
            message: "Tailscale daemon was not reachable.",
            repair: "Run tailscale status and inspect the local tailscaled service.",
        },
        TAILSCALE_NOT_AUTHENTICATED_CODE => DegradationReport {
            code: TAILSCALE_NOT_AUTHENTICATED_CODE,
            severity: "warning",
            message: "Tailscale daemon is running but this node is not authenticated.",
            repair: "Run tailscale up.",
        },
        TAILSCALE_BINARY_INAUTHENTIC_CODE => DegradationReport {
            code: TAILSCALE_BINARY_INAUTHENTIC_CODE,
            severity: "high",
            message: "Tailscale binary authenticity check failed.",
            repair: "Run which tailscale, verify provenance, and reinstall Tailscale if needed.",
        },
        TAILSCALE_SHIELDS_UP_CODE => DegradationReport {
            code: TAILSCALE_SHIELDS_UP_CODE,
            severity: "warning",
            message: "Tailscale shields-up mode is enabled; peers cannot initiate discovery.",
            repair: "Run tailscale set --shields-up=false if you want symmetric mesh discovery.",
        },
        TAILSCALE_PROBE_TIMEOUT_CODE => DegradationReport {
            code: TAILSCALE_PROBE_TIMEOUT_CODE,
            severity: "warning",
            message: "Tailscale probe exceeded its configured timeout budget.",
            repair: "Run tailscale status directly or raise EE_TAILSCALE_PROBE_TIMEOUT_MS.",
        },
        _ => DegradationReport {
            code: TAILSCALE_PROBE_UNAVAILABLE_CODE,
            severity: "info",
            message: "Tailscale probe skipped because mesh is disabled.",
            repair: "Set EE_MESH_ENABLED=1 to enable optional mesh-memory probes.",
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn status_posture_report(
    options: &StatusOptions,
    capabilities: &CapabilityReport,
    memory_health: &MemoryHealthReport,
    curation_health: &CurationHealthReport,
    feedback_health: &FeedbackHealthReport,
    singleflight_posture: &SingleFlightPostureReport,
    graph_compute: &GraphComputeReport,
    derived_assets: &[DerivedAssetReport],
    degradations: &[DegradationReport],
) -> WorkspacePostureReport {
    let workspace_path = options.workspace_path.as_deref();
    let write_replay_required =
        workspace_path.is_some_and(super::write_owner::workspace_write_replay_required);
    let storage_status =
        storage_posture_status(capabilities.storage, workspace_path, write_replay_required);
    let search_status = search_posture_status(capabilities.search, storage_status);
    let graph_status = graph_compute_posture_status(graph_compute.status);

    let subsystems = vec![
        posture_row(
            "runtime",
            capability_posture_status(capabilities.runtime, workspace_path),
            None,
            None,
        ),
        posture_row(
            "storage",
            storage_status,
            storage_posture_reason(capabilities.storage, workspace_path, write_replay_required),
            storage_posture_fallback(capabilities.storage, workspace_path),
        ),
        posture_row(
            "search",
            search_status,
            search_posture_reason(capabilities.search, storage_status),
            search_posture_fallback(capabilities.search, storage_status),
        ),
        posture_row(
            "memory",
            memory_posture_status(memory_health.status, workspace_path),
            memory_posture_reason(memory_health.status, workspace_path),
            memory_posture_fallback(memory_health.status, workspace_path),
        ),
        posture_row(
            "graph_compute",
            graph_status,
            graph_compute_posture_reason(graph_compute.status),
            graph_compute_posture_fallback(graph_compute.status),
        ),
        posture_row(
            "pack",
            pack_posture_status(storage_status, search_status),
            pack_posture_reason(storage_status, search_status),
            pack_posture_fallback(storage_status, search_status),
        ),
        posture_row(
            "curate",
            curation_posture_status(curation_health.status, storage_status),
            curation_posture_reason(curation_health.status, storage_status),
            curation_posture_fallback(curation_health.status, storage_status),
        ),
        posture_row(
            "feedback",
            feedback_posture_status(feedback_health.status, storage_status),
            feedback_posture_reason(feedback_health.status, storage_status),
            feedback_posture_fallback(feedback_health.status, storage_status),
        ),
        posture_row(
            "singleflight",
            singleflight_posture_status(singleflight_posture),
            singleflight_posture_reason(singleflight_posture),
            singleflight_posture_fallback(singleflight_posture),
        ),
        posture_row(
            "maintenance",
            maintenance_posture_status(derived_assets),
            maintenance_posture_reason(derived_assets),
            maintenance_posture_fallback(derived_assets),
        ),
        posture_row(
            "agent_detection",
            capability_posture_status(capabilities.agent_detection, workspace_path),
            None,
            None,
        ),
    ];
    let operation = OperationPostureReport {
        status: operation_posture_status(capabilities),
        subsystems_used: vec![
            "runtime",
            "storage",
            "search",
            "memory",
            "graph_compute",
            "curate",
            "feedback",
            "singleflight",
            "maintenance",
            "agent_detection",
        ],
        subsystems_skipped: vec!["pack"],
        degradations_applied: degradations
            .iter()
            .map(|degradation| degradation.code)
            .collect(),
    };

    WorkspacePostureReport::new(subsystems, operation)
}

fn posture_row(
    id: &'static str,
    status: SubsystemPostureStatus,
    reason: Option<&'static str>,
    fallback: Option<&'static str>,
) -> SubsystemPostureReport {
    let mut row =
        SubsystemPostureReport::new(id, status).with_checks_passed(checks_passed_for(status));
    if let Some(reason) = reason {
        row = row.with_reason(reason);
    }
    if let Some(fallback) = fallback {
        row = row.with_fallback(fallback);
    }
    row
}

const fn checks_passed_for(status: SubsystemPostureStatus) -> u32 {
    match status {
        SubsystemPostureStatus::Ok => 1,
        SubsystemPostureStatus::DegradedRecoverable
        | SubsystemPostureStatus::DegradedRequired
        | SubsystemPostureStatus::Blocked
        | SubsystemPostureStatus::Unimplemented
        | SubsystemPostureStatus::Initializing => 0,
    }
}

const fn operation_posture_status(capabilities: &CapabilityReport) -> SubsystemPostureStatus {
    if matches!(capabilities.runtime, CapabilityStatus::Ready)
        && matches!(capabilities.agent_detection, CapabilityStatus::Ready)
    {
        SubsystemPostureStatus::Ok
    } else {
        SubsystemPostureStatus::DegradedRecoverable
    }
}

const fn capability_posture_status(
    status: CapabilityStatus,
    workspace_path: Option<&Path>,
) -> SubsystemPostureStatus {
    match status {
        CapabilityStatus::Ready => SubsystemPostureStatus::Ok,
        CapabilityStatus::Pending if workspace_path.is_none() => {
            SubsystemPostureStatus::Initializing
        }
        CapabilityStatus::Pending => SubsystemPostureStatus::Initializing,
        CapabilityStatus::Degraded => SubsystemPostureStatus::DegradedRequired,
        CapabilityStatus::Unimplemented => SubsystemPostureStatus::Unimplemented,
    }
}

const fn storage_posture_status(
    status: CapabilityStatus,
    workspace_path: Option<&Path>,
    write_replay_required: bool,
) -> SubsystemPostureStatus {
    if write_replay_required {
        return SubsystemPostureStatus::DegradedRecoverable;
    }
    match status {
        CapabilityStatus::Ready => SubsystemPostureStatus::Ok,
        CapabilityStatus::Pending if workspace_path.is_none() => {
            SubsystemPostureStatus::Initializing
        }
        CapabilityStatus::Pending => SubsystemPostureStatus::Blocked,
        CapabilityStatus::Degraded => SubsystemPostureStatus::DegradedRequired,
        CapabilityStatus::Unimplemented => SubsystemPostureStatus::Unimplemented,
    }
}

const fn storage_posture_reason(
    status: CapabilityStatus,
    workspace_path: Option<&Path>,
    write_replay_required: bool,
) -> Option<&'static str> {
    if write_replay_required {
        return Some("uncommitted_write_replay_required");
    }
    match status {
        CapabilityStatus::Ready => None,
        CapabilityStatus::Pending if workspace_path.is_none() => Some("workspace_not_selected"),
        CapabilityStatus::Pending => Some("storage_not_initialized"),
        CapabilityStatus::Degraded => Some("storage_degraded"),
        CapabilityStatus::Unimplemented => Some("storage_unimplemented"),
    }
}

const fn storage_posture_fallback(
    status: CapabilityStatus,
    workspace_path: Option<&Path>,
) -> Option<&'static str> {
    match status {
        CapabilityStatus::Ready => None,
        CapabilityStatus::Pending if workspace_path.is_none() => {
            Some("ee status --workspace . --json")
        }
        CapabilityStatus::Pending => Some("ee init --workspace ."),
        CapabilityStatus::Degraded => Some("ee doctor --json"),
        CapabilityStatus::Unimplemented => Some("use a binary built with storage support"),
    }
}

const fn search_posture_status(
    status: CapabilityStatus,
    storage_status: SubsystemPostureStatus,
) -> SubsystemPostureStatus {
    match status {
        CapabilityStatus::Ready => SubsystemPostureStatus::Ok,
        CapabilityStatus::Pending => SubsystemPostureStatus::Initializing,
        CapabilityStatus::Degraded
            if matches!(
                storage_status,
                SubsystemPostureStatus::Blocked | SubsystemPostureStatus::DegradedRequired
            ) =>
        {
            SubsystemPostureStatus::DegradedRequired
        }
        CapabilityStatus::Degraded => SubsystemPostureStatus::DegradedRecoverable,
        CapabilityStatus::Unimplemented => SubsystemPostureStatus::Unimplemented,
    }
}

const fn search_posture_reason(
    status: CapabilityStatus,
    storage_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match status {
        CapabilityStatus::Ready => None,
        CapabilityStatus::Pending
            if matches!(
                storage_status,
                SubsystemPostureStatus::Blocked
                    | SubsystemPostureStatus::DegradedRequired
                    | SubsystemPostureStatus::Initializing
            ) =>
        {
            Some("waiting_for_storage")
        }
        CapabilityStatus::Pending => Some("search_initializing"),
        CapabilityStatus::Degraded => Some("search_index_degraded"),
        CapabilityStatus::Unimplemented => Some("search_unimplemented"),
    }
}

const fn search_posture_fallback(
    status: CapabilityStatus,
    storage_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match status {
        CapabilityStatus::Ready => None,
        CapabilityStatus::Pending
            if matches!(
                storage_status,
                SubsystemPostureStatus::Blocked | SubsystemPostureStatus::DegradedRequired
            ) =>
        {
            Some("ee init --workspace .")
        }
        CapabilityStatus::Pending => Some("ee index status --workspace . --json"),
        CapabilityStatus::Degraded => Some("ee index status --workspace . --json"),
        CapabilityStatus::Unimplemented => Some("use a binary built with search support enabled"),
    }
}

const fn graph_compute_posture_status(status: GraphComputeStatus) -> SubsystemPostureStatus {
    match status {
        GraphComputeStatus::Available => SubsystemPostureStatus::Ok,
        GraphComputeStatus::Degraded => SubsystemPostureStatus::DegradedRecoverable,
        GraphComputeStatus::Unavailable => SubsystemPostureStatus::Unimplemented,
    }
}

const fn graph_compute_posture_reason(status: GraphComputeStatus) -> Option<&'static str> {
    match status {
        GraphComputeStatus::Available => None,
        GraphComputeStatus::Degraded => Some("graph_compute_degraded"),
        GraphComputeStatus::Unavailable => Some("graph_compute_unimplemented"),
    }
}

const fn graph_compute_posture_fallback(status: GraphComputeStatus) -> Option<&'static str> {
    match status {
        GraphComputeStatus::Available => None,
        GraphComputeStatus::Degraded => Some("ee doctor --json"),
        GraphComputeStatus::Unavailable => Some("use a binary built with graph support enabled"),
    }
}

const fn pack_posture_status(
    storage_status: SubsystemPostureStatus,
    search_status: SubsystemPostureStatus,
) -> SubsystemPostureStatus {
    match storage_status {
        SubsystemPostureStatus::Blocked => SubsystemPostureStatus::Blocked,
        SubsystemPostureStatus::DegradedRequired => SubsystemPostureStatus::DegradedRequired,
        SubsystemPostureStatus::Initializing => SubsystemPostureStatus::Initializing,
        SubsystemPostureStatus::Unimplemented => SubsystemPostureStatus::Unimplemented,
        SubsystemPostureStatus::Ok | SubsystemPostureStatus::DegradedRecoverable => {
            match search_status {
                SubsystemPostureStatus::Ok => SubsystemPostureStatus::Ok,
                SubsystemPostureStatus::Blocked => SubsystemPostureStatus::Blocked,
                SubsystemPostureStatus::DegradedRequired => {
                    SubsystemPostureStatus::DegradedRequired
                }
                SubsystemPostureStatus::DegradedRecoverable
                | SubsystemPostureStatus::Unimplemented => {
                    SubsystemPostureStatus::DegradedRecoverable
                }
                SubsystemPostureStatus::Initializing => SubsystemPostureStatus::Initializing,
            }
        }
    }
}

const fn pack_posture_reason(
    storage_status: SubsystemPostureStatus,
    search_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match pack_posture_status(storage_status, search_status) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Blocked => Some("pack_blocked_by_storage"),
        SubsystemPostureStatus::DegradedRequired => Some("pack_requires_storage_repair"),
        SubsystemPostureStatus::DegradedRecoverable => Some("pack_uses_degraded_search"),
        SubsystemPostureStatus::Unimplemented => Some("pack_dependency_unimplemented"),
        SubsystemPostureStatus::Initializing => Some("pack_waiting_for_workspace"),
    }
}

const fn pack_posture_fallback(
    storage_status: SubsystemPostureStatus,
    search_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match pack_posture_status(storage_status, search_status) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Blocked | SubsystemPostureStatus::Initializing => {
            Some("ee init --workspace .")
        }
        SubsystemPostureStatus::DegradedRequired => Some("ee doctor --json"),
        SubsystemPostureStatus::DegradedRecoverable => Some("ee index rebuild --workspace ."),
        SubsystemPostureStatus::Unimplemented => {
            Some("use a binary built with required pack dependencies")
        }
    }
}

const fn memory_posture_status(
    status: MemoryHealthStatus,
    workspace_path: Option<&Path>,
) -> SubsystemPostureStatus {
    match status {
        MemoryHealthStatus::Healthy | MemoryHealthStatus::Empty => SubsystemPostureStatus::Ok,
        MemoryHealthStatus::Degraded => SubsystemPostureStatus::DegradedRecoverable,
        MemoryHealthStatus::Unavailable if workspace_path.is_none() => {
            SubsystemPostureStatus::Initializing
        }
        MemoryHealthStatus::Unavailable => SubsystemPostureStatus::Blocked,
    }
}

const fn memory_posture_reason(
    status: MemoryHealthStatus,
    workspace_path: Option<&Path>,
) -> Option<&'static str> {
    match memory_posture_status(status, workspace_path) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => Some("memory_waiting_for_workspace"),
        SubsystemPostureStatus::DegradedRecoverable => Some("memory_health_degraded"),
        SubsystemPostureStatus::Blocked => Some("memory_unavailable"),
        SubsystemPostureStatus::DegradedRequired => Some("memory_repair_required"),
        SubsystemPostureStatus::Unimplemented => Some("memory_unimplemented"),
    }
}

const fn memory_posture_fallback(
    status: MemoryHealthStatus,
    workspace_path: Option<&Path>,
) -> Option<&'static str> {
    match memory_posture_status(status, workspace_path) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => Some("ee status --workspace . --json"),
        SubsystemPostureStatus::DegradedRecoverable => Some("ee memory list --workspace . --json"),
        SubsystemPostureStatus::Blocked => Some("ee init --workspace ."),
        SubsystemPostureStatus::DegradedRequired => Some("ee doctor --json"),
        SubsystemPostureStatus::Unimplemented => Some("use a binary built with memory support"),
    }
}

const fn curation_posture_status(
    status: CurationHealthStatus,
    storage_status: SubsystemPostureStatus,
) -> SubsystemPostureStatus {
    match status {
        CurationHealthStatus::Healthy
        | CurationHealthStatus::Empty
        | CurationHealthStatus::NotInspected => SubsystemPostureStatus::Ok,
        CurationHealthStatus::Due | CurationHealthStatus::Degraded => {
            SubsystemPostureStatus::DegradedRecoverable
        }
        CurationHealthStatus::Escalated => SubsystemPostureStatus::DegradedRequired,
        CurationHealthStatus::Unavailable
            if matches!(
                storage_status,
                SubsystemPostureStatus::Blocked | SubsystemPostureStatus::Initializing
            ) =>
        {
            SubsystemPostureStatus::Initializing
        }
        CurationHealthStatus::Unavailable => SubsystemPostureStatus::DegradedRecoverable,
    }
}

const fn curation_posture_reason(
    status: CurationHealthStatus,
    storage_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match curation_posture_status(status, storage_status) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => Some("curation_waiting_for_storage"),
        SubsystemPostureStatus::DegradedRecoverable => Some("curation_attention_available"),
        SubsystemPostureStatus::DegradedRequired => Some("curation_escalated"),
        SubsystemPostureStatus::Blocked => Some("curation_blocked"),
        SubsystemPostureStatus::Unimplemented => Some("curation_unimplemented"),
    }
}

const fn curation_posture_fallback(
    status: CurationHealthStatus,
    storage_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match curation_posture_status(status, storage_status) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => Some("ee init --workspace ."),
        SubsystemPostureStatus::DegradedRecoverable => {
            Some("ee curate candidates --workspace . --json")
        }
        SubsystemPostureStatus::DegradedRequired => Some("ee curate review --workspace . --json"),
        SubsystemPostureStatus::Blocked => Some("ee doctor --json"),
        SubsystemPostureStatus::Unimplemented => Some("use a binary built with curation support"),
    }
}

const fn feedback_posture_status(
    status: FeedbackHealthStatus,
    storage_status: SubsystemPostureStatus,
) -> SubsystemPostureStatus {
    match status {
        FeedbackHealthStatus::Healthy | FeedbackHealthStatus::NotInspected => {
            SubsystemPostureStatus::Ok
        }
        FeedbackHealthStatus::ReviewQueued => SubsystemPostureStatus::DegradedRecoverable,
        FeedbackHealthStatus::Unavailable
            if matches!(
                storage_status,
                SubsystemPostureStatus::Blocked | SubsystemPostureStatus::Initializing
            ) =>
        {
            SubsystemPostureStatus::Initializing
        }
        FeedbackHealthStatus::Unavailable => SubsystemPostureStatus::DegradedRecoverable,
    }
}

const fn feedback_posture_reason(
    status: FeedbackHealthStatus,
    storage_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match feedback_posture_status(status, storage_status) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => Some("feedback_waiting_for_storage"),
        SubsystemPostureStatus::DegradedRecoverable => Some("feedback_review_available"),
        SubsystemPostureStatus::DegradedRequired => Some("feedback_repair_required"),
        SubsystemPostureStatus::Blocked => Some("feedback_blocked"),
        SubsystemPostureStatus::Unimplemented => Some("feedback_unimplemented"),
    }
}

const fn feedback_posture_fallback(
    status: FeedbackHealthStatus,
    storage_status: SubsystemPostureStatus,
) -> Option<&'static str> {
    match feedback_posture_status(status, storage_status) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => Some("ee init --workspace ."),
        SubsystemPostureStatus::DegradedRecoverable => {
            Some("ee outcome quarantine list --workspace . --json")
        }
        SubsystemPostureStatus::DegradedRequired | SubsystemPostureStatus::Blocked => {
            Some("ee doctor --json")
        }
        SubsystemPostureStatus::Unimplemented => Some("use a binary built with feedback support"),
    }
}

fn singleflight_posture_status(report: &SingleFlightPostureReport) -> SubsystemPostureStatus {
    match report.status.as_str() {
        "state_poisoned" | "observed_failures" => SubsystemPostureStatus::DegradedRecoverable,
        "active" | "idle" => SubsystemPostureStatus::Ok,
        _ => SubsystemPostureStatus::Initializing,
    }
}

fn singleflight_posture_reason(report: &SingleFlightPostureReport) -> Option<&'static str> {
    match singleflight_posture_status(report) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => Some("singleflight_surface_unconfigured"),
        SubsystemPostureStatus::DegradedRecoverable => Some("singleflight_observed_failures"),
        SubsystemPostureStatus::DegradedRequired => Some("singleflight_repair_required"),
        SubsystemPostureStatus::Blocked => Some("singleflight_blocked"),
        SubsystemPostureStatus::Unimplemented => Some("singleflight_unimplemented"),
    }
}

fn singleflight_posture_fallback(report: &SingleFlightPostureReport) -> Option<&'static str> {
    match singleflight_posture_status(report) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::Initializing => {
            Some("rerun a read-heavy command with matching keys")
        }
        SubsystemPostureStatus::DegradedRecoverable => {
            Some("inspect status singleFlight counts before rerunning duplicate work")
        }
        SubsystemPostureStatus::DegradedRequired | SubsystemPostureStatus::Blocked => {
            Some("ee doctor --json")
        }
        SubsystemPostureStatus::Unimplemented => {
            Some("enable a single-flight surface before expecting coalescing")
        }
    }
}

fn maintenance_posture_status(assets: &[DerivedAssetReport]) -> SubsystemPostureStatus {
    let statuses = assets
        .iter()
        .map(|asset| match asset.status {
            DerivedAssetStatus::Current
            | DerivedAssetStatus::Empty
            | DerivedAssetStatus::NotInspected => SubsystemPostureStatus::Ok,
            DerivedAssetStatus::Stale
            | DerivedAssetStatus::Missing
            | DerivedAssetStatus::Corrupt => SubsystemPostureStatus::DegradedRecoverable,
            DerivedAssetStatus::Unavailable => SubsystemPostureStatus::DegradedRecoverable,
            DerivedAssetStatus::Unimplemented => SubsystemPostureStatus::Unimplemented,
        })
        .collect::<Vec<_>>();
    SubsystemPostureStatus::aggregate(&statuses)
}

fn maintenance_posture_reason(assets: &[DerivedAssetReport]) -> Option<&'static str> {
    match maintenance_posture_status(assets) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::DegradedRecoverable => Some("derived_asset_attention_available"),
        SubsystemPostureStatus::DegradedRequired => Some("derived_asset_repair_required"),
        SubsystemPostureStatus::Blocked => Some("derived_asset_blocked"),
        SubsystemPostureStatus::Unimplemented => Some("derived_asset_unimplemented"),
        SubsystemPostureStatus::Initializing => Some("derived_asset_initializing"),
    }
}

fn maintenance_posture_fallback(assets: &[DerivedAssetReport]) -> Option<&'static str> {
    match maintenance_posture_status(assets) {
        SubsystemPostureStatus::Ok => None,
        SubsystemPostureStatus::DegradedRecoverable => Some("ee index rebuild --workspace ."),
        SubsystemPostureStatus::DegradedRequired
        | SubsystemPostureStatus::Blocked
        | SubsystemPostureStatus::Initializing => Some("ee doctor --json"),
        SubsystemPostureStatus::Unimplemented => {
            Some("implement the persistent derived asset before reporting a watermark")
        }
    }
}

fn push_storage_capability_degradation(
    degradations: &mut Vec<DegradationReport>,
    status: CapabilityStatus,
    workspace_path: Option<&Path>,
) {
    match status {
        CapabilityStatus::Ready => {}
        CapabilityStatus::Pending if workspace_path.is_none() => {
            degradations.push(DegradationReport {
                code: "storage_not_inspected",
                severity: "low",
                message: "Storage readiness was not inspected because no workspace was selected.",
                repair: "Run `ee status --workspace . --json`.",
            });
        }
        CapabilityStatus::Pending => {
            degradations.push(DegradationReport {
                code: "storage_not_initialized",
                severity: "medium",
                message: "Workspace storage is unavailable because .ee/ee.db is missing.",
                repair: "Run `ee init --workspace .`.",
            });
        }
        CapabilityStatus::Degraded => {
            degradations.push(DegradationReport {
                code: "storage_degraded",
                severity: "medium",
                message: "Workspace storage exists but could not be opened or needs migration.",
                repair: "Run `ee doctor --json`.",
            });
            degradations.push(DegradationReport {
                code: "storage_unavailable",
                severity: "high",
                message: "Workspace storage is unavailable because the selected database failed readiness checks.",
                repair: "Run `ee doctor --json`.",
            });
        }
        CapabilityStatus::Unimplemented => {
            degradations.push(DegradationReport {
                code: "storage_unimplemented",
                severity: "high",
                message: "Storage has no compiled implementation in this binary.",
                repair: "Use a binary built with the storage subsystem enabled.",
            });
        }
    }
}

fn push_runtime_capability_degradation(
    degradations: &mut Vec<DegradationReport>,
    status: CapabilityStatus,
) {
    match status {
        CapabilityStatus::Ready => {}
        CapabilityStatus::Pending
        | CapabilityStatus::Degraded
        | CapabilityStatus::Unimplemented => {
            degradations.push(DegradationReport {
                code: "runtime_unavailable",
                severity: "high",
                message: "runtime failed readiness checks for this command.",
                repair: "Run `ee doctor --json`.",
            });
        }
    }
}

fn push_search_capability_degradation(
    degradations: &mut Vec<DegradationReport>,
    status: CapabilityStatus,
    workspace_path: Option<&Path>,
) {
    match status {
        CapabilityStatus::Ready => {}
        CapabilityStatus::Pending if workspace_path.is_none() => {
            degradations.push(DegradationReport {
                code: "search_not_inspected",
                severity: "low",
                message: "Search readiness was not inspected because no workspace was selected.",
                repair: "Run `ee status --workspace . --json`.",
            });
        }
        CapabilityStatus::Pending => {
            degradations.push(DegradationReport {
                code: "search_waiting_for_storage",
                severity: "medium",
                message: "Search readiness is pending until workspace storage is initialized.",
                repair: "Run `ee init --workspace .`.",
            });
        }
        CapabilityStatus::Degraded => {
            degradations.push(DegradationReport {
                code: "search_index_degraded",
                severity: "medium",
                message: "Search is compiled but the selected workspace index is missing, stale, corrupt, or unreadable.",
                repair: "Run `ee index status --workspace . --json`.",
            });
            degradations.push(DegradationReport {
                code: "search_unavailable",
                severity: "medium",
                message: "Workspace search is unavailable because the selected index is missing, stale, corrupt, or unreadable.",
                repair: "Run `ee index status --workspace . --json`.",
            });
        }
        CapabilityStatus::Unimplemented => {
            degradations.push(DegradationReport {
                code: "search_unimplemented",
                severity: "high",
                message: "Search has no compiled implementation in this binary.",
                repair: "Use a binary built with search support enabled.",
            });
        }
    }
}

fn push_graph_capability_degradation(
    degradations: &mut Vec<DegradationReport>,
    status: GraphComputeStatus,
) {
    if matches!(status, GraphComputeStatus::Unavailable) {
        degradations.push(DegradationReport {
            code: "graph_feature_disabled",
            severity: "medium",
            message: "Graph algorithm execution requires the graph feature.",
            repair: "Rebuild ee with --features graph.",
        });
    }
}

fn push_skyline_degenerate_communities_degradation(
    degradations: &mut Vec<DegradationReport>,
    community_count: Option<usize>,
) {
    let Some(community_count) = community_count else {
        return;
    };
    if community_count >= SKYLINE_MIN_COMMUNITY_COUNT {
        return;
    }
    degradations.push(DegradationReport {
        code: GRAPH_SKYLINE_DEGENERATE_COMMUNITIES_CODE,
        severity: "info",
        message: "Knowledge skyline communities are degenerate because fewer than three Louvain communities were found.",
        repair: "No operator action required; treat skyline separation as informational until the workspace has more connected evidence.",
    });
}

fn push_status_skyline_feature_disabled_degradation(
    degradations: &mut Vec<DegradationReport>,
    enabled: Option<bool>,
) {
    if enabled != Some(false) {
        return;
    }
    degradations.push(DegradationReport {
        code: "graph_feature_disabled",
        severity: "medium",
        message: "Knowledge skyline status is disabled by graph.feature.skyline.enabled.",
        repair: "ee config set graph.feature.skyline.enabled true",
    });
}

fn push_toon_output_capability_degradation(
    degradations: &mut Vec<DegradationReport>,
    status: CapabilityStatus,
) {
    match status {
        CapabilityStatus::Ready | CapabilityStatus::Pending => {}
        CapabilityStatus::Degraded => {
            degradations.push(DegradationReport {
                code: "toon_unavailable",
                severity: "medium",
                message: "TOON output is unavailable because the TOON renderer capability is disabled.",
                repair: "Unset `EE_DISABLE_TOON` or use `--format json`.",
            });
        }
        CapabilityStatus::Unimplemented => {
            degradations.push(DegradationReport {
                code: "toon_unavailable",
                severity: "medium",
                message: "TOON output is unavailable because the TOON renderer is not linked in this binary.",
                repair: "Use `--format json` or a binary built with TOON output support.",
            });
        }
    }
}

fn gather_status_skyline_community_count(workspace_path: Option<&Path>) -> Option<usize> {
    let workspace_path = workspace_path?;
    #[cfg(feature = "graph")]
    {
        let database_path = workspace_path.join(".ee").join("ee.db");
        if !database_path.exists() {
            return None;
        }
        let connection = DbConnection::open_file(&database_path).ok()?;
        let links = status_visible_memory_links(connection.list_all_memory_links(None).ok()?);
        Some(status_skyline_community_count_from_links(&links))
    }
    #[cfg(not(feature = "graph"))]
    {
        let _ = workspace_path;
        None
    }
}

fn status_skyline_feature_enabled(workspace_path: Option<&Path>) -> Option<bool> {
    let workspace_root = workspace_path?;
    let options = crate::core::config_surface::ConfigSurfaceOptions {
        workspace_root: workspace_root.to_path_buf(),
        config_path: None,
    };
    crate::core::config_surface::get_config(&options, GRAPH_FEATURE_SKYLINE_ENABLED_KEY)
        .ok()
        .map(|report| report.value == "true")
}

#[cfg(feature = "graph")]
fn status_skyline_community_count_from_links(links: &[StoredMemoryLink]) -> usize {
    if links.is_empty() {
        return 0;
    }
    let graph = status_skyline_graph_from_links(links);
    crate::graph::health::detect_louvain_communities(&graph).len()
}

#[cfg(feature = "graph")]
fn status_skyline_graph_from_links(links: &[StoredMemoryLink]) -> fnx_classes::Graph {
    let mut graph = fnx_classes::Graph::strict();
    for link in links {
        graph.add_node(&link.src_memory_id);
        graph.add_node(&link.dst_memory_id);
        let _ = graph
            .extend_edges_unrecorded([(link.src_memory_id.as_str(), link.dst_memory_id.as_str())]);
    }
    graph
}

fn gather_workspace_status(workspace_path: Option<&Path>) -> Option<WorkspaceStatusReport> {
    let workspace_path = workspace_path?;
    let request = match WorkspaceResolutionRequest::from_process(
        Some(workspace_path.to_path_buf()),
        WorkspaceResolutionMode::AllowUninitialized,
    ) {
        Ok(request) => request,
        Err(_) => WorkspaceResolutionRequest::new(
            PathBuf::from("."),
            WorkspaceResolutionMode::AllowUninitialized,
        )
        .with_explicit_workspace(workspace_path.to_path_buf()),
    };
    let resolution = resolve_workspace(&request).ok()?;
    let diagnostics = diagnose_workspace_resolution(&request, &resolution);
    Some(WorkspaceStatusReport::from_resolution(
        resolution,
        diagnostics,
    ))
}

fn gather_derived_assets(
    workspace_path: Option<&Path>,
    graph_snapshot_artifact: &GraphSnapshotArtifactReport,
) -> Vec<DerivedAssetReport> {
    let search_index = match workspace_path {
        Some(path) => {
            let options = IndexStatusOptions {
                workspace_path: path.to_path_buf(),
                database_path: None,
                index_dir: None,
            };
            match get_index_status(&options) {
                Ok(report) => DerivedAssetReport::from_index_status(&report),
                Err(_) => DerivedAssetReport::unavailable("search_index", ".ee/index"),
            }
        }
        None => DerivedAssetReport::not_inspected("search_index", ".ee/index"),
    };

    let graph_snapshot = DerivedAssetReport::from_graph_snapshot_artifact(graph_snapshot_artifact);

    vec![search_index, graph_snapshot]
}

fn gather_graph_compute(workspace_path: Option<&Path>) -> GraphComputeReport {
    if diag_forced_capability_gap("graph") {
        return GraphComputeReport {
            status: GraphComputeStatus::Unavailable,
            available_algorithms: &[],
            live_compute_supported: false,
            fnx_runtime_version: FNX_RUNTIME_VERSION,
            result_cache: GraphAlgorithmResultCacheReport::not_inspected(),
            last_used_at: None,
        };
    }

    #[cfg(feature = "graph")]
    {
        GraphComputeReport {
            status: GraphComputeStatus::Available,
            available_algorithms: GRAPH_COMPUTE_ALGORITHMS,
            live_compute_supported: true,
            fnx_runtime_version: FNX_RUNTIME_VERSION,
            result_cache: gather_graph_algorithm_result_cache(workspace_path),
            last_used_at: None,
        }
    }
    #[cfg(not(feature = "graph"))]
    {
        GraphComputeReport {
            status: GraphComputeStatus::Unavailable,
            available_algorithms: &[],
            live_compute_supported: false,
            fnx_runtime_version: FNX_RUNTIME_VERSION,
            result_cache: GraphAlgorithmResultCacheReport::not_inspected(),
            last_used_at: None,
        }
    }
}

#[cfg(feature = "graph")]
fn gather_graph_algorithm_result_cache(
    workspace_path: Option<&Path>,
) -> GraphAlgorithmResultCacheReport {
    let Some(workspace_path) = workspace_path else {
        return GraphAlgorithmResultCacheReport::not_inspected();
    };
    let database_path = workspace_path.join(".ee").join("ee.db");
    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return GraphAlgorithmResultCacheReport::unavailable();
    };

    let mut cached_result_count = 0_u32;
    let mut observed_compute_count = 0_u32;
    for workspace_id in resolve_status_workspace_ids(&connection, workspace_path) {
        let Ok(Some(snapshot)) =
            connection.get_latest_graph_snapshot(&workspace_id, GraphSnapshotType::MemoryLinks)
        else {
            continue;
        };
        let Ok(results) =
            connection.list_graph_algorithm_results(&workspace_id, &snapshot.id, None)
        else {
            return GraphAlgorithmResultCacheReport::unavailable();
        };
        let Ok(witnesses) =
            connection.list_graph_algorithm_witnesses(&workspace_id, &snapshot.id, None)
        else {
            return GraphAlgorithmResultCacheReport::unavailable();
        };
        cached_result_count =
            cached_result_count.saturating_add(u32::try_from(results.len()).unwrap_or(u32::MAX));
        observed_compute_count = observed_compute_count
            .saturating_add(u32::try_from(witnesses.len()).unwrap_or(u32::MAX));
    }

    let total_observed = cached_result_count.saturating_add(observed_compute_count);
    let cache_hit_rate_basis_points = cached_result_count
        .saturating_mul(10_000)
        .checked_div(total_observed);
    let status = if total_observed == 0 {
        "empty"
    } else if cached_result_count == 0 {
        "cold"
    } else {
        "observed"
    };

    GraphAlgorithmResultCacheReport {
        status,
        cached_result_count,
        observed_compute_count,
        cache_hit_rate_basis_points,
    }
}

fn gather_graph_snapshot_artifact(workspace_path: Option<&Path>) -> GraphSnapshotArtifactReport {
    let Some(workspace_path) = workspace_path else {
        return graph_snapshot_artifact_report(
            DerivedAssetStatus::NotInspected,
            None,
            None,
            None,
            GraphSnapshotMemoryGraphReport {
                node_count: 0,
                edge_count: 0,
                generation: 0,
                matches_db_generation: false,
                availability: graph_live_compute_availability(),
            },
        );
    };

    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return graph_snapshot_artifact_report(
            DerivedAssetStatus::Unavailable,
            None,
            None,
            None,
            GraphSnapshotMemoryGraphReport {
                node_count: 0,
                edge_count: 0,
                generation: 0,
                matches_db_generation: false,
                availability: graph_live_compute_availability(),
            },
        );
    }

    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(_) => {
            return graph_snapshot_artifact_report(
                DerivedAssetStatus::Unavailable,
                None,
                None,
                None,
                GraphSnapshotMemoryGraphReport {
                    node_count: 0,
                    edge_count: 0,
                    generation: 0,
                    matches_db_generation: false,
                    availability: graph_live_compute_availability(),
                },
            );
        }
    };

    gather_graph_snapshot_artifact_from_connection(&connection, workspace_path)
}

fn gather_graph_snapshot_artifact_from_connection(
    connection: &DbConnection,
    workspace_path: &Path,
) -> GraphSnapshotArtifactReport {
    let (current_generation, node_count, edge_count) =
        memory_graph_generation(connection).unwrap_or((0, 0, 0));
    let mut snapshot = None;
    for workspace_id in resolve_status_workspace_ids(connection, workspace_path) {
        match connection.get_latest_graph_snapshot(&workspace_id, GraphSnapshotType::MemoryLinks) {
            Ok(Some(candidate)) => {
                if snapshot
                    .as_ref()
                    .is_none_or(|current: &crate::db::StoredGraphSnapshot| {
                        candidate.snapshot_version > current.snapshot_version
                    })
                {
                    snapshot = Some(candidate);
                }
            }
            Ok(None) => {}
            Err(_) => {
                return graph_snapshot_artifact_report(
                    DerivedAssetStatus::Unavailable,
                    None,
                    None,
                    None,
                    GraphSnapshotMemoryGraphReport {
                        node_count,
                        edge_count,
                        generation: current_generation,
                        matches_db_generation: false,
                        availability: graph_live_compute_availability(),
                    },
                );
            }
        }
    }

    let Some(snapshot) = snapshot else {
        return graph_snapshot_artifact_report(
            DerivedAssetStatus::Empty,
            None,
            None,
            None,
            GraphSnapshotMemoryGraphReport {
                node_count,
                edge_count,
                generation: current_generation,
                matches_db_generation: false,
                availability: graph_live_compute_availability(),
            },
        );
    };

    let snapshot_generation = u64::from(snapshot.source_generation);
    let matches_db_generation = snapshot_generation == current_generation;
    let status = match snapshot.status {
        GraphSnapshotStatus::Invalid | GraphSnapshotStatus::Archived => DerivedAssetStatus::Corrupt,
        GraphSnapshotStatus::Stale => DerivedAssetStatus::Stale,
        GraphSnapshotStatus::Valid if matches_db_generation => DerivedAssetStatus::Current,
        GraphSnapshotStatus::Valid => DerivedAssetStatus::Stale,
    };

    graph_snapshot_artifact_report(
        status,
        Some(snapshot.created_at),
        None,
        Some(snapshot_generation),
        GraphSnapshotMemoryGraphReport {
            node_count: node_count.max(snapshot.node_count),
            edge_count: edge_count.max(snapshot.edge_count),
            generation: current_generation,
            matches_db_generation,
            availability: graph_live_compute_availability(),
        },
    )
}

fn graph_snapshot_artifact_report(
    status: DerivedAssetStatus,
    last_built_at: Option<String>,
    snapshot_path: Option<&'static str>,
    snapshot_generation: Option<u64>,
    memory_graph: GraphSnapshotMemoryGraphReport,
) -> GraphSnapshotArtifactReport {
    GraphSnapshotArtifactReport {
        status,
        last_built_at,
        snapshot_path,
        snapshot_generation,
        memory_graph,
        next_refresh_via: GRAPH_SNAPSHOT_REFRESH_COMMAND,
    }
}

fn graph_live_compute_availability() -> &'static str {
    #[cfg(feature = "graph")]
    {
        GRAPH_LIVE_COMPUTE_AVAILABLE
    }
    #[cfg(not(feature = "graph"))]
    {
        GRAPH_LIVE_COMPUTE_UNAVAILABLE
    }
}

fn memory_graph_generation(
    connection: &DbConnection,
) -> Result<(u64, u32, u32), crate::db::DbError> {
    let links = status_visible_memory_links(connection.list_all_memory_links(None)?);
    let mut nodes = BTreeSet::new();
    for link in &links {
        nodes.insert(link.src_memory_id.clone());
        nodes.insert(link.dst_memory_id.clone());
    }
    let generation = u64::try_from(links.len()).unwrap_or(u64::MAX);
    let node_count = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
    let edge_count = u32::try_from(links.len()).unwrap_or(u32::MAX);
    Ok((generation, node_count, edge_count))
}

fn status_visible_memory_links(links: Vec<StoredMemoryLink>) -> Vec<StoredMemoryLink> {
    links
        .into_iter()
        .filter(|link| {
            crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
        })
        .collect()
}

fn resolve_status_workspace_ids(connection: &DbConnection, workspace_path: &Path) -> Vec<String> {
    let mut candidates = Vec::new();
    let workspace_key = workspace_path.to_string_lossy().to_string();
    if let Some(workspace) = connection
        .get_workspace_by_path(&workspace_key)
        .ok()
        .flatten()
    {
        push_unique_workspace_id(&mut candidates, workspace.id);
    }
    push_unique_workspace_id(&mut candidates, stable_workspace_id(workspace_path));

    if let Ok(canonical) = workspace_path.canonicalize() {
        let canonical_key = canonical.to_string_lossy().to_string();
        if let Some(workspace) = connection
            .get_workspace_by_path(&canonical_key)
            .ok()
            .flatten()
        {
            push_unique_workspace_id(&mut candidates, workspace.id);
        }
        push_unique_workspace_id(&mut candidates, stable_workspace_id(&canonical));
    }

    candidates
}

fn push_unique_workspace_id(candidates: &mut Vec<String>, workspace_id: String) {
    if !candidates
        .iter()
        .any(|candidate| candidate == &workspace_id)
    {
        candidates.push(workspace_id);
    }
}

fn gather_memory_health(
    workspace_path: Option<&Path>,
) -> (MemoryHealthReport, Vec<DegradationReport>) {
    let Some(workspace_path) = workspace_path else {
        return (
            MemoryHealthReport::gather(),
            vec![DegradationReport {
                code: "memory_health_unavailable",
                severity: "low",
                message: "Memory health is unavailable without an explicit workspace.",
                repair: "Run `ee status --workspace . --json`.",
            }],
        );
    };

    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return (
            MemoryHealthReport::gather(),
            vec![DegradationReport {
                code: "memory_health_unavailable",
                severity: "low",
                message: "Memory health is unavailable because the workspace database is missing.",
                repair: "Run `ee init --workspace .` before inspecting memory health.",
            }],
        );
    }

    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(_) => {
            return (
                MemoryHealthReport::gather(),
                vec![DegradationReport {
                    code: "memory_health_unavailable",
                    severity: "medium",
                    message: "Memory health is unavailable because the database could not be opened.",
                    repair: "Run `ee doctor --json`.",
                }],
            );
        }
    };

    let memories = match connection.list_memories(&workspace_id, None, true) {
        Ok(memories) => memories,
        Err(_) => {
            return (
                MemoryHealthReport::gather(),
                vec![DegradationReport {
                    code: "memory_health_unavailable",
                    severity: "medium",
                    message: "Memory health is unavailable because memory rows could not be read.",
                    repair: "Run `ee db migrate --workspace .` and `ee doctor --json`.",
                }],
            );
        }
    };
    let access_times = connection
        .list_audit_entries(Some(&workspace_id), None)
        .map(|entries| memory_access_timestamp_map(&entries))
        .unwrap_or_default();

    (
        memory_health_from_rows_with_accesses(&memories, Utc::now(), &access_times),
        Vec::new(),
    )
}

#[cfg(test)]
fn memory_health_from_rows(memories: &[StoredMemory], now: DateTime<Utc>) -> MemoryHealthReport {
    memory_health_from_rows_with_accesses(memories, now, &BTreeMap::new())
}

fn memory_health_from_rows_with_accesses(
    memories: &[StoredMemory],
    now: DateTime<Utc>,
    access_times: &BTreeMap<String, DateTime<Utc>>,
) -> MemoryHealthReport {
    if memories.is_empty() {
        return MemoryHealthReport {
            status: MemoryHealthStatus::Empty,
            total_count: 0,
            active_count: 0,
            tombstoned_count: 0,
            stale_count: 0,
            average_confidence: None,
            provenance_coverage: None,
            health_score: None,
            score_components: None,
        };
    }

    let total_count = capped_u32(memories.len());
    let mut active_count = 0_u32;
    let mut tombstoned_count = 0_u32;
    let mut stale_count = 0_u32;
    let mut confidence_sum = 0.0_f32;
    let mut freshness_sum = 0.0_f32;
    let mut provenance_count = 0_u32;

    for memory in memories {
        if memory.tombstoned_at.is_some() {
            tombstoned_count = tombstoned_count.saturating_add(1);
            continue;
        }

        active_count = active_count.saturating_add(1);
        confidence_sum += memory.confidence;
        if memory
            .provenance_uri
            .as_deref()
            .is_some_and(|uri| !uri.trim().is_empty())
        {
            provenance_count = provenance_count.saturating_add(1);
        }
        let freshness = memory_row_freshness_score(memory, now, access_times.get(&memory.id));
        freshness_sum += freshness;
        if freshness < 0.5 {
            stale_count = stale_count.saturating_add(1);
        }
    }

    let average_confidence = if active_count == 0 {
        None
    } else {
        Some((confidence_sum / active_count as f32).clamp(0.0, 1.0))
    };
    let provenance_coverage = if active_count == 0 {
        None
    } else {
        Some(bounded_ratio(provenance_count, active_count))
    };
    let freshness_score = if active_count == 0 {
        0.0
    } else {
        (freshness_sum / active_count as f32).clamp(0.0, 1.0)
    };
    let active_ratio = bounded_ratio(active_count, total_count);
    let confidence_score = bounded_score(average_confidence);
    let provenance_score = bounded_score(provenance_coverage);
    let tombstone_penalty = bounded_ratio(tombstoned_count, total_count);
    let score_components = Some(MemoryHealthScoreComponents {
        active_ratio,
        freshness_score,
        freshness_sourced_from: MEMORY_DECAY_SOURCE,
        confidence_score,
        provenance_score,
        tombstone_penalty,
    });
    let health_score = score_components.map(MemoryHealthScoreComponents::health_score);

    let mut report = MemoryHealthReport {
        status: MemoryHealthStatus::Healthy,
        total_count,
        active_count,
        tombstoned_count,
        stale_count,
        average_confidence,
        provenance_coverage,
        health_score,
        score_components,
    };

    report.status = match report.health_score {
        _ if active_count == 0 => MemoryHealthStatus::Degraded,
        Some(score) if score >= 0.5 => MemoryHealthStatus::Healthy,
        _ => MemoryHealthStatus::Degraded,
    };

    report
}

fn memory_row_freshness_score(
    memory: &StoredMemory,
    now: DateTime<Utc>,
    last_accessed_at: Option<&DateTime<Utc>>,
) -> f32 {
    let Some(reference) = parse_memory_timestamp(&memory.updated_at)
        .or_else(|| parse_memory_timestamp(&memory.created_at))
        .into_iter()
        .chain(last_accessed_at.copied())
        .max()
    else {
        return 0.0;
    };
    let reference = reference.min(now);
    evaluate_memory_decay(memory, reference, now, MemoryDecayThresholds::default()).freshness
}

fn memory_access_timestamp_map(entries: &[StoredAuditEntry]) -> BTreeMap<String, DateTime<Utc>> {
    let mut timestamps = BTreeMap::new();
    for entry in entries {
        if !is_memory_access_audit_action(&entry.action) {
            continue;
        }
        if entry.target_type.as_deref() != Some("memory") {
            continue;
        }
        let Some(memory_id) = entry.target_id.as_deref() else {
            continue;
        };
        let Some(timestamp) = parse_memory_timestamp(&entry.timestamp) else {
            continue;
        };
        timestamps
            .entry(memory_id.to_owned())
            .and_modify(|existing| {
                if timestamp > *existing {
                    *existing = timestamp;
                }
            })
            .or_insert(timestamp);
    }
    timestamps
}

fn is_memory_access_audit_action(action: &str) -> bool {
    matches!(
        action,
        audit_actions::SEARCH_RETURNED_MEM
            | audit_actions::PACK_INCLUDED_MEM
            | audit_actions::MEMORY_SHOW
            | audit_actions::WHY_INSPECTED
    )
}

fn parse_memory_timestamp(timestamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn gather_curation_health(
    workspace_path: Option<&Path>,
) -> (CurationHealthReport, Vec<DegradationReport>) {
    let Some(workspace_path) = workspace_path else {
        return (CurationHealthReport::not_inspected(), Vec::new());
    };
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return (
            CurationHealthReport::unavailable(),
            vec![DegradationReport {
                code: "curation_health_unavailable",
                severity: "low",
                message: "Curation health is unavailable because the workspace database is missing.",
                repair: "Run `ee init --workspace .` before inspecting curation health.",
            }],
        );
    }

    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(_) => {
            return (
                CurationHealthReport::unavailable(),
                vec![DegradationReport {
                    code: "curation_health_unavailable",
                    severity: "medium",
                    message: "Curation health is unavailable because the database could not be opened.",
                    repair: "Run `ee doctor --json`.",
                }],
            );
        }
    };
    let candidates = match connection.list_curation_candidates(&workspace_id, None, None, None) {
        Ok(candidates) => candidates,
        Err(_) => {
            return (
                CurationHealthReport::unavailable(),
                vec![DegradationReport {
                    code: "curation_health_unavailable",
                    severity: "medium",
                    message: "Curation health is unavailable because candidate rows could not be read.",
                    repair: "Run `ee db migrate --workspace .` and `ee doctor --json`.",
                }],
            );
        }
    };
    let policies = match connection.list_curation_ttl_policies() {
        Ok(policies) => policies,
        Err(_) => {
            return (
                CurationHealthReport::unavailable(),
                vec![DegradationReport {
                    code: "curation_ttl_policy_unavailable",
                    severity: "medium",
                    message: "Curation TTL policy rows could not be read.",
                    repair: "Run `ee db migrate --workspace .`.",
                }],
            );
        }
    };

    let health = curation_health_from_rows(&candidates, &policies, Utc::now());
    let degradations = curation_health_degradations(&health);
    (health, degradations)
}

fn gather_feedback_health(
    workspace_path: Option<&Path>,
) -> (FeedbackHealthReport, Vec<DegradationReport>) {
    let Some(workspace_path) = workspace_path else {
        return (FeedbackHealthReport::not_inspected(), Vec::new());
    };
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return (
            FeedbackHealthReport::unavailable(),
            vec![DegradationReport {
                code: "feedback_health_unavailable",
                severity: "low",
                message: "Feedback health is unavailable because the workspace database is missing.",
                repair: "Run `ee init --workspace .` before inspecting feedback health.",
            }],
        );
    }

    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(_) => {
            return (
                FeedbackHealthReport::unavailable(),
                vec![DegradationReport {
                    code: "feedback_health_unavailable",
                    severity: "medium",
                    message: "Feedback health is unavailable because the database could not be opened.",
                    repair: "Run `ee doctor --json`.",
                }],
            );
        }
    };
    let since = Utc::now()
        .checked_sub_signed(ChronoDuration::seconds(i64::from(
            DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        )))
        .unwrap_or_else(Utc::now)
        .to_rfc3339();
    let per_source_harmful_counts = match connection
        .list_harmful_feedback_source_counts_since(&workspace_id, &since)
    {
        Ok(counts) => counts.into_iter().map(FeedbackSourceHealth::from).collect(),
        Err(_) => {
            return (
                FeedbackHealthReport::unavailable(),
                vec![DegradationReport {
                    code: "feedback_health_unavailable",
                    severity: "medium",
                    message: "Feedback health is unavailable because feedback rows could not be read.",
                    repair: "Run `ee db migrate --workspace .` and `ee doctor --json`.",
                }],
            );
        }
    };
    let quarantine_queue_depth =
        match connection.list_feedback_quarantine(&workspace_id, Some("pending")) {
            Ok(rows) => u32::try_from(rows.len()).unwrap_or(u32::MAX),
            Err(_) => {
                return (
                    FeedbackHealthReport::unavailable(),
                    vec![DegradationReport {
                        code: "feedback_quarantine_unavailable",
                        severity: "medium",
                        message: "Feedback quarantine rows could not be read.",
                        repair: "Run `ee db migrate --workspace .`.",
                    }],
                );
            }
        };
    let protected_rule_count = match connection.count_protected_procedural_rules(&workspace_id) {
        Ok(count) => count,
        Err(_) => {
            return (
                FeedbackHealthReport::unavailable(),
                vec![DegradationReport {
                    code: "feedback_protected_rules_unavailable",
                    severity: "medium",
                    message: "Protected procedural rule rows could not be read.",
                    repair: "Run `ee db migrate --workspace .`.",
                }],
            );
        }
    };
    let status = if quarantine_queue_depth > 0 {
        FeedbackHealthStatus::ReviewQueued
    } else {
        FeedbackHealthStatus::Healthy
    };
    let next_deterministic_action = if quarantine_queue_depth > 0 {
        "review quarantined feedback with ee outcome quarantine list --json".to_owned()
    } else {
        "monitor harmful feedback rates".to_owned()
    };

    (
        FeedbackHealthReport {
            status,
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
            per_source_harmful_counts,
            quarantine_queue_depth,
            protected_rule_count,
            last_inversion_event: None,
            next_deterministic_action,
        },
        Vec::new(),
    )
}

fn curation_health_from_rows(
    candidates: &[StoredCurationCandidate],
    policies: &[StoredCurationTtlPolicy],
    now: DateTime<Utc>,
) -> CurationHealthReport {
    if candidates.is_empty() {
        return CurationHealthReport {
            status: CurationHealthStatus::Empty,
            policy_count: capped_u32(policies.len()),
            auto_promote_enabled_count: capped_u32(
                policies
                    .iter()
                    .filter(|policy| policy.auto_promote_enabled)
                    .count(),
            ),
            ..CurationHealthReport::not_inspected()
        };
    }

    let policy_map = policies
        .iter()
        .map(|policy| (policy.id.as_str(), policy))
        .collect::<BTreeMap<_, _>>();
    let mut pending_count = 0_u32;
    let mut accepted_count = 0_u32;
    let mut snoozed_count = 0_u32;
    let mut rejected_count = 0_u32;
    let mut due_count = 0_u32;
    let mut prompt_count = 0_u32;
    let mut escalation_count = 0_u32;
    let mut blocked_count = 0_u32;
    let mut oldest_pending_age_days = None;
    let mut reviewed_latencies = Vec::new();
    let mut next_scheduled_at: Option<String> = None;

    for candidate in candidates {
        let review_state = normalized_review_state(candidate);
        match review_state.as_str() {
            "accepted" => accepted_count = accepted_count.saturating_add(1),
            "snoozed" => snoozed_count = snoozed_count.saturating_add(1),
            "rejected" => rejected_count = rejected_count.saturating_add(1),
            _ => pending_count = pending_count.saturating_add(1),
        }

        if let (Ok(created), Some(reviewed)) = (
            DateTime::parse_from_rfc3339(&candidate.created_at),
            candidate.reviewed_at.as_deref(),
        ) && let Ok(reviewed) = DateTime::parse_from_rfc3339(reviewed)
        {
            reviewed_latencies.push(reviewed.signed_duration_since(created).num_days().max(0));
        }

        if pending_count > 0
            && let Ok(created) = DateTime::parse_from_rfc3339(&candidate.created_at)
        {
            let age = now
                .signed_duration_since(created.with_timezone(&Utc))
                .num_days()
                .max(0);
            oldest_pending_age_days =
                Some(oldest_pending_age_days.map_or(age, |oldest| std::cmp::max(oldest, age)));
        }

        let policy_id = candidate
            .ttl_policy_id
            .as_deref()
            .unwrap_or_else(|| default_curation_ttl_policy_id_for_review_state(&review_state));
        let Some(policy) = policy_map.get(policy_id) else {
            blocked_count = blocked_count.saturating_add(1);
            continue;
        };
        let Some(state_entered) = candidate_state_entered_at(candidate) else {
            blocked_count = blocked_count.saturating_add(1);
            continue;
        };
        let threshold = match i64::try_from(policy.threshold_seconds) {
            Ok(value) => chrono::Duration::seconds(value),
            Err(_) => {
                blocked_count = blocked_count.saturating_add(1);
                continue;
            }
        };
        let due_at = state_entered + threshold;
        if due_at > now {
            let due_at_str = due_at.to_rfc3339();
            next_scheduled_at = match next_scheduled_at {
                Some(current) if current < due_at_str => Some(current),
                _ => Some(due_at_str),
            };
            continue;
        }

        due_count = due_count.saturating_add(1);
        match policy.action.as_str() {
            "prompt_promote" => prompt_count = prompt_count.saturating_add(1),
            "escalate" => escalation_count = escalation_count.saturating_add(1),
            "snooze" | "retire_with_audit" => {}
            _ => blocked_count = blocked_count.saturating_add(1),
        }
    }

    let mean_review_latency_days = if reviewed_latencies.is_empty() {
        None
    } else {
        Some(reviewed_latencies.iter().sum::<i64>() / reviewed_latencies.len() as i64)
    };
    let status = if escalation_count > 0 {
        CurationHealthStatus::Escalated
    } else if blocked_count > 0 {
        CurationHealthStatus::Degraded
    } else if due_count > 0 {
        CurationHealthStatus::Due
    } else {
        CurationHealthStatus::Healthy
    };

    CurationHealthReport {
        status,
        total_count: capped_u32(candidates.len()),
        pending_count,
        accepted_count,
        snoozed_count,
        rejected_count,
        due_count,
        prompt_count,
        escalation_count,
        blocked_count,
        policy_count: capped_u32(policies.len()),
        auto_promote_enabled_count: capped_u32(
            policies
                .iter()
                .filter(|policy| policy.auto_promote_enabled)
                .count(),
        ),
        oldest_pending_age_days,
        mean_review_latency_days,
        next_scheduled_at,
    }
}

fn curation_health_degradations(health: &CurationHealthReport) -> Vec<DegradationReport> {
    let mut degradations = Vec::new();
    if health.escalation_count > 0 {
        degradations.push(DegradationReport {
            code: "curation_harmful_candidate_escalated",
            severity: "high",
            message: "One or more rejected curation candidates reached their escalation TTL.",
            repair: "Run `ee curate disposition --json` and review escalated candidates.",
        });
    }
    if health.blocked_count > 0 {
        degradations.push(DegradationReport {
            code: "curation_ttl_blocked",
            severity: "medium",
            message: "One or more curation candidates could not be evaluated against TTL policy.",
            repair: "Run `ee curate disposition --json` for candidate-level errors.",
        });
    }
    degradations
}

fn normalized_review_state(candidate: &StoredCurationCandidate) -> String {
    if candidate.review_state.trim().is_empty() {
        "new".to_owned()
    } else {
        candidate.review_state.clone()
    }
}

fn candidate_state_entered_at(candidate: &StoredCurationCandidate) -> Option<DateTime<Utc>> {
    candidate
        .state_entered_at
        .as_deref()
        .or(candidate.reviewed_at.as_deref())
        .or(candidate.applied_at.as_deref())
        .unwrap_or(candidate.created_at.as_str())
        .parse::<DateTime<Utc>>()
        .ok()
}

fn capped_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Canonical Criterion group name for the status benchmark.
pub const STATUS_BENCH_GROUP_NAME: &str = "ee_status";
/// Hard p50 ceiling (ms) from plan section 28 for `ee status`.
pub const STATUS_BENCH_HARD_CEILING_MS: f64 = 100.0;
/// Quick benchmark iteration count used by regression tests.
pub const STATUS_BENCH_QUICK_ITERATIONS: u32 = 5;

/// Input scale for status benchmarking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusBenchScale {
    pub name: &'static str,
    pub memory_count: usize,
}

/// Required scale set for `ee status` benchmark runs.
pub const STATUS_BENCH_SCALES: [StatusBenchScale; 3] = [
    StatusBenchScale {
        name: "empty",
        memory_count: 0,
    },
    StatusBenchScale {
        name: "memory_100",
        memory_count: 100,
    },
    StatusBenchScale {
        name: "memory_5000",
        memory_count: 5_000,
    },
];

/// Prepared workspace fixture for one status benchmark scale.
#[derive(Clone, Debug)]
pub struct StatusBenchFixture {
    scale: StatusBenchScale,
    workspace_path: PathBuf,
}

impl StatusBenchFixture {
    /// Build a deterministic local workspace fixture and seed memory rows.
    pub fn prepare(scale: StatusBenchScale) -> Result<Self, String> {
        let workspace_path = status_bench_workspace_path(scale);
        let ee_dir = workspace_path.join(".ee");
        fs::create_dir_all(&ee_dir).map_err(|error| {
            format!(
                "failed to create benchmark workspace directory {}: {error}",
                ee_dir.display()
            )
        })?;

        let database_path = ee_dir.join("ee.db");
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("failed to open benchmark database: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("failed to migrate benchmark database: {error}"))?;

        let canonical_workspace = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.clone());
        let workspace_id = stable_workspace_id(&canonical_workspace);
        seed_status_bench_memories(
            &connection,
            &workspace_path,
            &workspace_id,
            scale.memory_count,
        )?;

        Ok(Self {
            scale,
            workspace_path,
        })
    }

    #[must_use]
    pub const fn scale(&self) -> StatusBenchScale {
        self.scale
    }

    #[must_use]
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    /// Measure one `ee status` report generation against this fixture.
    pub fn measure_once(&self) -> Result<Duration, String> {
        let started_at = Instant::now();
        let report = StatusReport::gather_for_workspace(&self.workspace_path);
        let elapsed = started_at.elapsed();

        if report.curation_health.total_count != 0 {
            return Err(format!(
                "status benchmark expected 0 curation candidates for scale `{}`, got {}",
                self.scale.name, report.curation_health.total_count
            ));
        }

        black_box(report);
        Ok(elapsed)
    }

    /// Run repeated measurements and return deterministic summary stats.
    pub fn run_iterations(&self, iterations: u32) -> Result<StatusBenchSample, String> {
        if iterations == 0 {
            return Err("status benchmark iterations must be greater than zero".to_owned());
        }

        let mut samples_ms = Vec::with_capacity(iterations as usize);
        for _ in 0..iterations {
            let elapsed = self.measure_once()?;
            samples_ms.push(duration_ms(elapsed));
        }

        let p50_ms = percentile_ms(&samples_ms, 0.50);
        let max_ms = samples_ms.iter().copied().fold(0.0_f64, f64::max);

        Ok(StatusBenchSample {
            scale_name: self.scale.name,
            memory_count: self.scale.memory_count,
            iterations,
            p50_ms,
            max_ms,
            hard_ceiling_ms: STATUS_BENCH_HARD_CEILING_MS,
            samples_ms,
        })
    }
}

/// Benchmark sample summary for one scale.
#[derive(Clone, Debug)]
pub struct StatusBenchSample {
    pub scale_name: &'static str,
    pub memory_count: usize,
    pub iterations: u32,
    pub p50_ms: f64,
    pub max_ms: f64,
    pub hard_ceiling_ms: f64,
    pub samples_ms: Vec<f64>,
}

/// Complete benchmark report for `ee status`.
#[derive(Clone, Debug)]
pub struct StatusBenchReport {
    pub operation: &'static str,
    pub iterations_per_scale: u32,
    pub aggregate_p50_ms: f64,
    pub hard_ceiling_ms: f64,
    pub scales: Vec<StatusBenchSample>,
}

/// Run the full status benchmark for the required scale set.
pub fn run_status_bench_report(iterations_per_scale: u32) -> Result<StatusBenchReport, String> {
    let mut scales = Vec::with_capacity(STATUS_BENCH_SCALES.len());
    for scale in STATUS_BENCH_SCALES {
        let fixture = StatusBenchFixture::prepare(scale)?;
        let sample = fixture.run_iterations(iterations_per_scale)?;
        scales.push(sample);
    }

    let mut aggregate_samples = Vec::new();
    for scale in &scales {
        aggregate_samples.extend(scale.samples_ms.iter().copied());
    }

    let aggregate_p50_ms = percentile_ms(&aggregate_samples, 0.50);

    Ok(StatusBenchReport {
        operation: STATUS_BENCH_GROUP_NAME,
        iterations_per_scale,
        aggregate_p50_ms,
        hard_ceiling_ms: STATUS_BENCH_HARD_CEILING_MS,
        scales,
    })
}

/// Run the quick-mode status benchmark.
pub fn run_status_bench_quick() -> Result<StatusBenchReport, String> {
    run_status_bench_report(STATUS_BENCH_QUICK_ITERATIONS)
}

/// Returns true when any measured p50 exceeds the configured hard ceiling.
#[must_use]
pub fn status_bench_exceeds_hard_ceiling(report: &StatusBenchReport) -> bool {
    if report.aggregate_p50_ms > STATUS_BENCH_HARD_CEILING_MS {
        return true;
    }

    report
        .scales
        .iter()
        .any(|sample| sample.p50_ms > sample.hard_ceiling_ms)
}

fn status_bench_workspace_path(scale: StatusBenchScale) -> PathBuf {
    let mut path = std::env::temp_dir();
    let unique_id = uuid::Uuid::now_v7();
    path.push(format!(
        "ee_status_bench_{}_{}_{}",
        std::process::id(),
        scale.memory_count,
        unique_id
    ));
    path
}

fn seed_status_bench_memories(
    connection: &DbConnection,
    workspace_path: &Path,
    workspace_id: &str,
    memory_count: usize,
) -> Result<(), String> {
    connection
        .begin()
        .map_err(|error| format!("failed to begin benchmark seed transaction: {error}"))?;
    let seed_result = (|| -> Result<(), String> {
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().to_string(),
                    name: Some("status benchmark workspace".to_owned()),
                },
            )
            .map_err(|error| format!("failed to insert benchmark workspace: {error}"))?;

        insert_status_bench_memory_rows(connection, workspace_id, memory_count)?;

        Ok(())
    })();

    match seed_result {
        Ok(()) => connection
            .commit()
            .map_err(|error| format!("failed to commit benchmark seed transaction: {error}")),
        Err(error) => {
            let _ = connection.rollback();
            Err(error)
        }
    }
}

fn insert_status_bench_memory_rows(
    connection: &DbConnection,
    workspace_id: &str,
    memory_count: usize,
) -> Result<(), String> {
    const INSERT_CHUNK_SIZE: usize = 250;

    let now = Utc::now().to_rfc3339();
    for chunk_start in (0..memory_count).step_by(INSERT_CHUNK_SIZE) {
        let chunk_end = chunk_start
            .saturating_add(INSERT_CHUNK_SIZE)
            .min(memory_count);
        let mut sql = String::from(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, provenance_uri, trust_class, trust_subclass, provenance_chain_hash, provenance_chain_hash_version, provenance_verification_status, created_at, updated_at, valid_from, valid_to) VALUES ",
        );
        for index in chunk_start..chunk_end {
            if index > chunk_start {
                sql.push_str(", ");
            }
            let memory_id = status_bench_memory_id(memory_count, index);
            let content = format!(
                "Status benchmark memory {index}: deterministic fixture for scale {memory_count}."
            );
            let provenance_chain_hash = format!(
                "blake3:{}",
                blake3::hash(format!("status-bench:{memory_id}:{content}").as_bytes()).to_hex()
            );

            sql.push('(');
            push_sql_text(&mut sql, &memory_id);
            sql.push_str(", ");
            push_sql_text(&mut sql, workspace_id);
            sql.push_str(", 'semantic', 'fact', ");
            push_sql_text(&mut sql, &content);
            sql.push_str(
                ", 0.60, 0.50, 0.50, 'bench://ee_status', 'agent_assertion', 'benchmark_fixture', ",
            );
            push_sql_text(&mut sql, &provenance_chain_hash);
            sql.push_str(", ");
            push_sql_text(&mut sql, PROVENANCE_CHAIN_HASH_VERSION);
            sql.push_str(", ");
            push_sql_text(&mut sql, PROVENANCE_STATUS_UNVERIFIED);
            sql.push_str(", ");
            push_sql_text(&mut sql, &now);
            sql.push_str(", ");
            push_sql_text(&mut sql, &now);
            sql.push_str(", NULL, NULL)");
        }
        connection
            .execute_raw(&sql)
            .map_err(|error| format!("failed to insert benchmark memory rows: {error}"))?;
    }

    Ok(())
}

fn push_sql_text(sql: &mut String, value: &str) {
    sql.push('\'');
    for character in value.chars() {
        if character == '\'' {
            sql.push_str("''");
        } else {
            sql.push(character);
        }
    }
    sql.push('\'');
}

fn status_bench_memory_id(memory_count: usize, index: usize) -> String {
    let seed = ((memory_count as u128) << 64) | (index as u128 + 1);
    MemoryId::from_uuid(uuid::Uuid::from_u128(seed)).to_string()
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn percentile_ms(samples: &[f64], percentile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    let mut sorted = samples.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let last_index = sorted.len() - 1;
    let rank = (percentile.clamp(0.0, 1.0) * last_index as f64).floor() as usize;
    sorted[rank]
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::models::CapabilityStatus;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn parse_ts(s: &str) -> Result<DateTime<Utc>, String> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| format!("invalid timestamp '{s}': {e}"))
    }

    fn audit_entry(action: &str, timestamp: &str, details: Option<String>) -> StoredAuditEntry {
        StoredAuditEntry {
            id: format!("audit_{action}_{timestamp}"),
            workspace_id: Some("wsp_test".to_string()),
            timestamp: timestamp.to_string(),
            actor: None,
            action: action.to_string(),
            target_type: Some("pack".to_string()),
            target_id: Some("pack_test".to_string()),
            details,
            surface: "pack".to_string(),
            mutation_kind: action.to_string(),
            before_hash: None,
            after_hash: None,
            prev_row_hash: None,
            this_row_hash: None,
        }
    }

    #[test]
    fn pack_budget_buckets_count_recent_pack_assembled_rows() -> TestResult {
        let now = parse_ts("2026-05-17T07:00:00Z")?;
        let entries = vec![
            audit_entry(
                audit_actions::PACK_ASSEMBLED,
                "2026-05-17T06:59:00Z",
                Some(r#"{"budget":999}"#.to_string()),
            ),
            audit_entry(
                audit_actions::PACK_ASSEMBLED,
                "2026-05-17T06:58:00Z",
                Some(r#"{"budget":1500,"adaptiveBudget":{"adaptive":true}}"#.to_string()),
            ),
            audit_entry(
                audit_actions::PACK_ASSEMBLED,
                "2026-05-17T06:57:00Z",
                Some(r#"{"budget":3000}"#.to_string()),
            ),
            audit_entry(
                audit_actions::PACK_ASSEMBLED,
                "2026-05-17T06:56:00Z",
                Some(r#"{"budget":4000,"adaptiveBudget":{"adaptive":true}}"#.to_string()),
            ),
            audit_entry(
                audit_actions::PACK_ASSEMBLED,
                "2026-05-17T06:55:00Z",
                Some(r#"{"budget":12000}"#.to_string()),
            ),
            audit_entry(
                audit_actions::PACK_ASSEMBLED,
                "2026-05-16T06:55:00Z",
                Some(r#"{"budget":12000,"adaptiveBudget":{"adaptive":true}}"#.to_string()),
            ),
            audit_entry(
                "memory.create",
                "2026-05-17T06:54:00Z",
                Some(r#"{"budget":1500}"#.to_string()),
            ),
        ];

        let report = pack_budget_buckets_from_audit_entries(&entries, now);

        ensure(report.total_invocations, 5, "total recent pack rows")?;
        ensure(report.adaptive_invocations, 2, "adaptive rows")?;
        ensure(report.non_adaptive_invocations, 3, "non-adaptive rows")?;
        ensure(report.below_one_k, 1, "below 1k")?;
        ensure(report.one_to_two_k, 1, "1-2k")?;
        ensure(report.two_to_four_k, 1, "2-4k")?;
        ensure(report.four_to_eight_k, 1, "4-8k")?;
        ensure(report.eight_k_plus, 1, "8k+")
    }

    #[test]
    fn tailscale_socket_override_replaces_default_candidates() -> TestResult {
        let mut config = TailscaleSocketProbeConfig::mesh_enabled();

        apply_tailscale_socket_override(
            &mut config,
            Some(" /tmp/ee-fake-tailscaled.sock ".to_owned()),
        );

        ensure(
            config.socket_candidates,
            vec![PathBuf::from("/tmp/ee-fake-tailscaled.sock")],
            "socket override should replace default socket candidates",
        )
    }

    #[test]
    fn tailscale_socket_override_ignores_blank_values() -> TestResult {
        let mut config = TailscaleSocketProbeConfig::mesh_enabled();
        let default_candidates = config.socket_candidates.clone();

        apply_tailscale_socket_override(&mut config, Some("   ".to_owned()));

        ensure(
            config.socket_candidates,
            default_candidates,
            "blank socket override should leave default candidates unchanged",
        )
    }

    fn stored_memory_fixture(
        id: &str,
        confidence: f32,
        provenance_uri: Option<&str>,
        updated_at: &str,
        tombstoned_at: Option<&str>,
    ) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "ws_test".to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: format!("test memory {id}"),
            workflow_id: None,
            confidence,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: provenance_uri.map(str::to_owned),
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: Some(format!("blake3:{id}")),
            provenance_chain_hash_version: PROVENANCE_CHAIN_HASH_VERSION.to_owned(),
            provenance_verification_status: PROVENANCE_STATUS_UNVERIFIED.to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: updated_at.to_owned(),
            updated_at: updated_at.to_owned(),
            tombstoned_at: tombstoned_at.map(str::to_owned),
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn status_report_gather_returns_valid_report() -> TestResult {
        let report = StatusReport::gather_with_options(&StatusOptions::default());

        ensure(
            report.capabilities.runtime,
            CapabilityStatus::Ready,
            "runtime should be ready",
        )?;
        ensure(
            report.capabilities.storage,
            CapabilityStatus::Pending,
            "storage not inspected without workspace",
        )?;
        ensure(
            report.capabilities.search,
            CapabilityStatus::Pending,
            "search not inspected without workspace",
        )?;
        ensure(
            report.capabilities.mesh,
            CapabilityStatus::Pending,
            "mesh should default to disabled/pending",
        )?;
        ensure(
            report.capabilities.agent_detection,
            CapabilityStatus::Ready,
            "agent detection should be ready",
        )?;
        ensure(report.runtime.engine, "asupersync", "runtime engine")?;
        ensure(report.runtime.profile, "current_thread", "runtime profile")?;
        ensure(
            report.derived_assets.len(),
            2,
            "derived assets should be reported",
        )?;
        Ok(())
    }

    #[test]
    fn cass_probe_maps_trusted_discovery_without_path_lookup_execution() -> TestResult {
        let ready = cass_discovery_to_capability(Ok(crate::cass::DiscoveredBinary::new(
            PathBuf::from("/usr/bin/cass"),
            crate::cass::DiscoverySource::Path,
        )));
        ensure(ready, CapabilityStatus::Ready, "trusted cass discovery")?;

        let pending = cass_discovery_to_capability(Err(crate::cass::CassError::BinaryNotFound {
            binary: PathBuf::from("cass"),
        }));
        ensure(pending, CapabilityStatus::Pending, "missing cass binary")?;

        let degraded = cass_discovery_to_capability(Err(crate::cass::CassError::InvalidBinary {
            binary: PathBuf::from("cass"),
            reason: "relative PATH lookup is not trusted".to_owned(),
        }));
        ensure(degraded, CapabilityStatus::Degraded, "invalid cass binary")
    }

    #[test]
    fn feedback_health_source_counts_redact_sensitive_source_ids() -> TestResult {
        let health = FeedbackSourceHealth::from(FeedbackSourceHarmfulCount {
            source_id: "file:///Users/alice/private/outcome.jsonl?api_key=redaction-fixture"
                .to_owned(),
            harmful_count: 3,
        });

        ensure(health.harmful_count, 3, "harmful count should be preserved")?;
        assert!(
            health.source_id.contains("[REDACTED_PATH]"),
            "source ID should redact path-like segments: {}",
            health.source_id
        );
        assert!(
            health.source_id.contains("[REDACTED:"),
            "source ID should redact secret-like segments: {}",
            health.source_id
        );
        assert!(
            !health.source_id.contains("/Users/alice")
                && !health.source_id.contains("redaction-fixture"),
            "source ID leaked sensitive material: {}",
            health.source_id
        );
        Ok(())
    }

    #[test]
    fn feedback_health_source_counts_preserve_safe_source_ids() -> TestResult {
        let health = FeedbackSourceHealth::from(FeedbackSourceHarmfulCount {
            source_id: "agent://run/public-feedback".to_owned(),
            harmful_count: 1,
        });

        ensure(
            health.source_id,
            "agent://run/public-feedback".to_owned(),
            "safe source IDs should remain readable",
        )
    }

    #[test]
    fn status_report_includes_deferred_agent_inventory() -> TestResult {
        let report = StatusReport::gather_with_options(&StatusOptions::default());

        ensure(
            report.agent_inventory.status.as_str(),
            "not_inspected",
            "agent inventory status",
        )?;
        ensure(
            report.agent_inventory.inspection_command,
            "ee agent status --json",
            "agent inventory inspection command",
        )?;
        ensure(
            report.agent_inventory.installed_agents.is_empty(),
            true,
            "status report should not expose machine-specific roots",
        )
    }

    #[test]
    fn status_report_includes_degradations_for_uninspected_subsystems() -> TestResult {
        let report = StatusReport::gather_with_options(&StatusOptions::default());

        ensure(report.degradations.len(), 3, "three degradations expected")?;

        let storage_deg = report
            .degradations
            .iter()
            .find(|d| d.code == "storage_not_inspected");
        ensure(storage_deg.is_some(), true, "storage degradation exists")?;

        let search_deg = report
            .degradations
            .iter()
            .find(|d| d.code == "search_not_inspected");
        ensure(search_deg.is_some(), true, "search degradation exists")?;

        let memory_health_deg = report
            .degradations
            .iter()
            .find(|d| d.code == "memory_health_unavailable");
        ensure(
            memory_health_deg.is_some(),
            true,
            "memory health degradation exists",
        )?;

        Ok(())
    }

    #[test]
    fn degraded_storage_capability_reports_specific_and_broad_codes() -> TestResult {
        let mut degradations = Vec::new();
        push_storage_capability_degradation(
            &mut degradations,
            CapabilityStatus::Degraded,
            Some(Path::new(".")),
        );

        ensure(
            degradations
                .iter()
                .map(|degradation| degradation.code)
                .collect(),
            vec!["storage_degraded", "storage_unavailable"],
            "storage degraded aliases",
        )?;
        ensure(
            degradations
                .iter()
                .map(|degradation| degradation.severity)
                .collect(),
            vec!["medium", "high"],
            "storage degraded severities",
        )
    }

    #[test]
    fn status_storage_posture_reports_uncommitted_write_replay_required() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let ee_dir = temp.path().join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let connection =
            DbConnection::open_file(ee_dir.join("ee.db")).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        super::super::write_owner::mark_write_replay_required(temp.path())
            .map_err(|error| error.to_string())?;

        let report = StatusReport::gather_for_workspace(temp.path());
        let storage = report
            .posture
            .subsystems
            .iter()
            .find(|subsystem| subsystem.id == "storage")
            .ok_or_else(|| "missing storage posture row".to_string())?;

        ensure(
            report.capabilities.storage,
            CapabilityStatus::Ready,
            "storage capability remains ready",
        )?;
        ensure(
            storage.status,
            SubsystemPostureStatus::DegradedRecoverable,
            "storage posture is recoverable",
        )?;
        ensure(
            storage.reason,
            Some("uncommitted_write_replay_required"),
            "storage replay reason",
        )
    }

    #[test]
    fn degraded_search_capability_reports_specific_and_broad_codes() -> TestResult {
        let mut degradations = Vec::new();
        push_search_capability_degradation(
            &mut degradations,
            CapabilityStatus::Degraded,
            Some(Path::new(".")),
        );

        ensure(
            degradations
                .iter()
                .map(|degradation| degradation.code)
                .collect(),
            vec!["search_index_degraded", "search_unavailable"],
            "search degraded aliases",
        )?;
        ensure(
            degradations
                .iter()
                .map(|degradation| degradation.severity)
                .collect(),
            vec!["medium", "medium"],
            "search degraded severities",
        )
    }

    #[test]
    fn degraded_toon_output_reports_unavailable_code() -> TestResult {
        let mut degradations = Vec::new();
        push_toon_output_capability_degradation(&mut degradations, CapabilityStatus::Degraded);

        ensure(
            degradations
                .iter()
                .map(|degradation| degradation.code)
                .collect(),
            vec!["toon_unavailable"],
            "toon degraded code",
        )?;
        ensure(
            degradations
                .iter()
                .map(|degradation| degradation.repair)
                .collect(),
            vec!["Unset `EE_DISABLE_TOON` or use `--format json`."],
            "toon degraded repair",
        )
    }

    #[test]
    fn status_skyline_reports_degenerate_communities() -> TestResult {
        let mut degradations = Vec::new();
        push_skyline_degenerate_communities_degradation(&mut degradations, Some(2));

        let skyline_deg = degradations
            .iter()
            .find(|degradation| degradation.code == GRAPH_SKYLINE_DEGENERATE_COMMUNITIES_CODE)
            .ok_or_else(|| "missing skyline degenerate communities degradation".to_string())?;

        ensure(skyline_deg.severity, "info", "skyline severity")?;
        ensure(
            skyline_deg.message.contains("degenerate"),
            true,
            "skyline message mentions degenerate",
        )?;
        ensure(
            skyline_deg.message.contains("communities"),
            true,
            "skyline message mentions communities",
        )?;

        let mut sufficient = Vec::new();
        push_skyline_degenerate_communities_degradation(&mut sufficient, Some(3));
        ensure(
            sufficient.is_empty(),
            true,
            "three communities is sufficient",
        )
    }

    #[test]
    fn status_skyline_respects_runtime_feature_flag() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let disabled_report = StatusReport::gather_for_workspace(temp.path());
        let disabled = disabled_report
            .degradations
            .iter()
            .find(|degradation| {
                degradation.code == "graph_feature_disabled"
                    && degradation.message.contains("Knowledge skyline status")
            })
            .ok_or_else(|| "status skyline should emit disabled degradation".to_string())?;
        ensure(disabled.severity, "medium", "disabled skyline severity")?;
        ensure(
            disabled.repair,
            "ee config set graph.feature.skyline.enabled true",
            "disabled skyline repair",
        )?;

        let config_dir = temp.path().join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            config_dir.join("config.toml"),
            "[graph.feature.skyline]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())?;

        let enabled_report = StatusReport::gather_for_workspace(temp.path());
        ensure(
            enabled_report.degradations.iter().all(|degradation| {
                degradation.code != "graph_feature_disabled"
                    || !degradation.message.contains("Knowledge skyline status")
            }),
            true,
            "enabled skyline should not emit disabled degradation",
        )
    }

    #[test]
    fn mesh_storage_status_report_counts_policy_failures_without_peer_labels() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-status-mesh-storage");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_mesh_import_ledger_event(&crate::db::InsertMeshImportLedgerEventInput {
                workspace_id,
                event_id: "mesh_evt_status_policy_denied".to_owned(),
                origin_node_id: "node_remote_status".to_owned(),
                origin_workspace_id: "wsp_remote_private".to_owned(),
                producer_peer_id: Some("peer_builder_one".to_owned()),
                seq: 1,
                prev_event_hash: None,
                event_hash: format!("blake3:{}", "a".repeat(64)),
                event_kind: "create".to_owned(),
                logical_memory_id: "mem_remote_status_rule".to_owned(),
                content_hash: format!("blake3:{}", "b".repeat(64)),
                material_lane: "body".to_owned(),
                redaction_class: "secretDenied".to_owned(),
                trust_lane: "peerAgent".to_owned(),
                import_decision: "deny".to_owned(),
                local_memory_id: None,
                body_cache_key: None,
                policy_failure_surface_json: Some(
                    r#"{"schema":"ee.mesh.policy_failure_surface.v1","code":"mesh_peer_policy_denied","action":"deny","reason":"peer_policy_redaction_denied","policyRef":"mesh_pol_status","materialLane":"body","redaction":"deny","trustLane":"peerAgent"}"#
                        .to_owned(),
                ),
                policy_decision_json: Some(
                    r#"{"schema":"ee.mesh.policy_decision.v1","direction":"inbound","action":"deny","reason":"peer_policy_redaction_denied","policyRef":"mesh_pol_status","materialLane":"body","redaction":"deny","trustLane":"peerAgent","importTrustClass":"agent_validated","bodyFetchAllowed":false,"localTruthSideEffectsAllowed":false,"searchOrGraphSideEffectsAllowed":false,"failure":{"schema":"ee.mesh.policy_failure_surface.v1","code":"mesh_peer_policy_denied","action":"deny","reason":"peer_policy_redaction_denied","policyRef":"mesh_pol_status","materialLane":"body","redaction":"deny","trustLane":"peerAgent"}}"#
                        .to_owned(),
                ),
                event_json: r#"{"schema":"ee.mesh.event.v1","eventKind":"create"}"#.to_owned(),
                imported_at: Some("2026-05-16T21:40:00Z".to_owned()),
            })
            .map_err(|error| error.to_string())?;

        let report = gather_mesh_storage_status_from_connection(&connection, workspace_path)
            .ok_or_else(|| "mesh storage status should be inspected".to_owned())?;

        ensure(report.imported_event_count, 1, "imported event count")?;
        ensure(
            report.policy_decision_event_count,
            1,
            "policy decision count",
        )?;
        ensure(report.policy_failure_event_count, 1, "policy failure count")?;
        ensure(report.has_rows(), true, "mesh storage has rows")?;
        ensure(report.peer_count, 0, "peer labels are not surfaced")
    }

    #[test]
    fn memory_health_score_components_are_conservative() -> TestResult {
        let report = MemoryHealthReport::healthy_fixture();
        let components = report
            .score_components
            .ok_or_else(|| "healthy fixture should have score components".to_string())?;

        ensure(components.active_ratio > 0.94, true, "active ratio")?;
        ensure(
            (0.89..0.90).contains(&components.freshness_score),
            true,
            "freshness score",
        )?;
        ensure(components.confidence_score, 0.85, "confidence component")?;
        ensure(components.provenance_score, 0.92, "provenance component")?;
        ensure(
            (0.80..0.81).contains(
                &report
                    .health_score
                    .ok_or_else(|| "healthy fixture should have health score".to_string())?,
            ),
            true,
            "aggregate health score",
        )
    }

    #[test]
    fn memory_health_score_treats_missing_evidence_as_zero() -> TestResult {
        let report = MemoryHealthReport {
            status: MemoryHealthStatus::Degraded,
            total_count: 12,
            active_count: 12,
            tombstoned_count: 0,
            stale_count: 0,
            average_confidence: None,
            provenance_coverage: None,
            health_score: None,
            score_components: None,
        }
        .with_conservative_score();

        ensure(report.health_score, Some(0.0), "missing evidence score")
    }

    #[test]
    fn memory_health_score_treats_invalid_evidence_as_zero() -> TestResult {
        let report = MemoryHealthReport {
            status: MemoryHealthStatus::Degraded,
            total_count: 12,
            active_count: 12,
            tombstoned_count: 0,
            stale_count: 0,
            average_confidence: Some(f32::NAN),
            provenance_coverage: Some(f32::INFINITY),
            health_score: None,
            score_components: None,
        }
        .with_conservative_score();

        let components = report
            .score_components
            .ok_or_else(|| "non-empty report should have score components".to_string())?;
        ensure(components.confidence_score, 0.0, "invalid confidence")?;
        ensure(components.provenance_score, 0.0, "invalid provenance")?;
        ensure(report.health_score, Some(0.0), "invalid evidence score")
    }

    #[test]
    fn memory_health_from_rows_counts_persisted_memory_metrics() -> TestResult {
        let now = parse_ts("2026-05-03T00:00:00Z")?;
        let rows = vec![
            stored_memory_fixture(
                "mem_fresh",
                0.8,
                Some("cass://session/1"),
                "2026-04-30T00:00:00Z",
                None,
            ),
            stored_memory_fixture("mem_stale", 0.4, None, "2026-03-01T00:00:00Z", None),
            stored_memory_fixture(
                "mem_tombstoned",
                0.9,
                Some("cass://session/2"),
                "2026-04-28T00:00:00Z",
                Some("2026-05-01T00:00:00Z"),
            ),
        ];

        let report = memory_health_from_rows(&rows, now);

        ensure(
            report.status,
            MemoryHealthStatus::Degraded,
            "mixed memory health status",
        )?;
        ensure(report.total_count, 3, "total count")?;
        ensure(report.active_count, 2, "active count")?;
        ensure(report.tombstoned_count, 1, "tombstoned count")?;
        ensure(report.stale_count, 0, "stale count")?;
        ensure(
            report.average_confidence,
            Some(0.6),
            "average active confidence",
        )?;
        ensure(
            report.provenance_coverage,
            Some(0.5),
            "active provenance coverage",
        )?;

        let components = report
            .score_components
            .ok_or_else(|| "non-empty memory health should include components".to_owned())?;
        ensure(components.active_ratio, 2.0 / 3.0, "active ratio")?;
        ensure(
            (0.88..0.89).contains(&components.freshness_score),
            true,
            "freshness",
        )?;
        ensure(
            components.freshness_sourced_from,
            MEMORY_DECAY_SOURCE,
            "freshness source",
        )?;
        ensure(components.confidence_score, 0.6, "confidence score")?;
        ensure(components.provenance_score, 0.5, "provenance score")?;
        ensure(components.tombstone_penalty, 1.0 / 3.0, "tombstone penalty")
    }

    #[test]
    fn gather_memory_health_reads_workspace_database_rows() -> TestResult {
        let fixture = StatusBenchFixture::prepare(StatusBenchScale {
            name: "memory_health_test",
            memory_count: 3,
        })?;

        let (report, degradations) = gather_memory_health(Some(fixture.workspace_path()));

        ensure(degradations.is_empty(), true, "no unavailable degradation")?;
        ensure(report.status, MemoryHealthStatus::Healthy, "status")?;
        ensure(report.total_count, 3, "total count")?;
        ensure(report.active_count, 3, "active count")?;
        ensure(report.tombstoned_count, 0, "tombstoned count")?;
        ensure(report.average_confidence, Some(0.6), "average confidence")?;
        ensure(report.provenance_coverage, Some(1.0), "provenance coverage")?;
        ensure(report.health_score, Some(0.6), "conservative score")
    }

    #[test]
    fn empty_memory_health_has_no_score() -> TestResult {
        let report = MemoryHealthReport {
            status: MemoryHealthStatus::Empty,
            total_count: 0,
            active_count: 0,
            tombstoned_count: 0,
            stale_count: 0,
            average_confidence: None,
            provenance_coverage: None,
            health_score: None,
            score_components: None,
        }
        .with_conservative_score();

        ensure(report.health_score, None, "empty health score")?;
        ensure(report.score_components, None, "empty score components")
    }

    #[test]
    fn status_report_version_matches_cargo_metadata() -> TestResult {
        let report = StatusReport::gather();
        ensure(
            report.version,
            env!("CARGO_PKG_VERSION"),
            "version from cargo",
        )
    }

    #[test]
    fn high_watermark_lag_handles_missing_and_current_values() -> TestResult {
        ensure(high_watermark_lag(Some(12), Some(9)), Some(3), "stale lag")?;
        ensure(
            high_watermark_lag(Some(9), Some(12)),
            Some(0),
            "ahead lag saturates",
        )?;
        ensure(
            high_watermark_lag(Some(12), None),
            Some(12),
            "missing asset lag",
        )?;
        ensure(high_watermark_lag(None, Some(9)), None, "unknown source")
    }

    #[test]
    fn derived_asset_from_index_status_reports_high_watermark_lag() -> TestResult {
        let report = super::super::index::IndexStatusReport {
            health: IndexHealth::Stale,
            index_dir: PathBuf::from("/tmp/index"),
            database_path: PathBuf::from("/tmp/ee.db"),
            index_exists: true,
            index_file_count: 2,
            index_size_bytes: 128,
            db_memory_count: 4,
            db_session_count: 1,
            db_generation: Some(12),
            index_generation: Some(9),
            last_rebuild_at: Some("2026-04-30T12:00:00Z".to_string()),
            last_check_error: None,
            repair_hint: Some("ee index rebuild --workspace ."),
            elapsed_ms: 1.0,
        };

        let asset = DerivedAssetReport::from_index_status(&report);

        ensure(asset.name, "search_index", "name")?;
        ensure(asset.status, DerivedAssetStatus::Stale, "status")?;
        ensure(
            asset.source_high_watermark,
            Some(12),
            "source high watermark",
        )?;
        ensure(asset.asset_high_watermark, Some(9), "asset high watermark")?;
        ensure(asset.high_watermark_lag, Some(3), "lag")?;
        ensure(
            asset.repair,
            Some("ee index rebuild --workspace ."),
            "repair",
        )
    }

    #[test]
    fn graph_compute_report_separates_live_algorithm_availability() -> TestResult {
        let report = gather_graph_compute(None);

        ensure(
            report.status,
            GraphComputeStatus::Available,
            "graph compute status",
        )?;
        ensure(
            report.live_compute_supported,
            true,
            "live compute supported",
        )?;
        ensure(
            report.available_algorithms.contains(&"pagerank"),
            true,
            "pagerank listed",
        )?;
        ensure(
            report.result_cache.status,
            "not_inspected",
            "cache not inspected without workspace",
        )
    }

    #[test]
    fn graph_snapshot_artifact_reports_empty_without_persisted_snapshot() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-status-graph-empty");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = gather_graph_snapshot_artifact_from_connection(&connection, workspace_path);
        let asset = DerivedAssetReport::from_graph_snapshot_artifact(&report);

        ensure(report.status, DerivedAssetStatus::Empty, "artifact status")?;
        ensure(report.memory_graph.node_count, 0, "node count")?;
        ensure(report.memory_graph.edge_count, 0, "edge count")?;
        ensure(
            report.memory_graph.availability,
            GRAPH_LIVE_COMPUTE_AVAILABLE,
            "live availability",
        )?;
        ensure(asset.name, GRAPH_SNAPSHOT_ASSET_NAME, "asset name")?;
        ensure(asset.kind, GRAPH_SNAPSHOT_ASSET_KIND, "asset kind")
    }

    #[test]
    fn memory_graph_generation_ignores_denied_mesh_links() -> TestResult {
        const MEMORY_A: &str = "mem_00000000000000000000000001";
        const MEMORY_B: &str = "mem_00000000000000000000000002";
        const MEMORY_C: &str = "mem_00000000000000000000000003";
        const LOCAL_LINK: &str = "link_00000000000000000000000001";
        const DENIED_LINK: &str = "link_00000000000000000000000002";

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-status-graph-mesh-filter");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        for (memory_id, content) in [
            (MEMORY_A, "Local graph source A"),
            (MEMORY_B, "Local graph source B"),
            (MEMORY_C, "Denied mesh graph source C"),
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "operational".to_owned(),
                        kind: "note".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.8,
                        utility: 0.5,
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
        connection
            .insert_memory_link(
                LOCAL_LINK,
                &crate::db::CreateMemoryLinkInput {
                    src_memory_id: MEMORY_A.to_owned(),
                    dst_memory_id: MEMORY_B.to_owned(),
                    relation: crate::db::MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: crate::db::MemoryLinkSource::Agent,
                    created_by: Some("status-mesh-test".to_owned()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_link(
                DENIED_LINK,
                &crate::db::CreateMemoryLinkInput {
                    src_memory_id: MEMORY_B.to_owned(),
                    dst_memory_id: MEMORY_C.to_owned(),
                    relation: crate::db::MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: crate::db::MemoryLinkSource::Agent,
                    created_by: Some("status-mesh-test".to_owned()),
                    metadata_json: Some(status_denied_mesh_link_metadata()),
                },
            )
            .map_err(|error| error.to_string())?;

        let (generation, node_count, edge_count) =
            memory_graph_generation(&connection).map_err(|error| error.to_string())?;

        ensure(generation, 1, "visible graph generation")?;
        ensure(node_count, 2, "visible node count")?;
        ensure(edge_count, 1, "visible edge count")
    }

    #[test]
    fn graph_snapshot_artifact_reports_current_persisted_snapshot() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-status-graph-current");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                "gsnap_0000000000000000000000001",
                &crate::db::CreateGraphSnapshotInput {
                    workspace_id,
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot_validation.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 0,
                    edge_count: 0,
                    metrics_json: "{}".to_owned(),
                    content_hash: "blake3:empty".to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = gather_graph_snapshot_artifact_from_connection(&connection, workspace_path);

        ensure(
            report.status,
            DerivedAssetStatus::Current,
            "artifact status",
        )?;
        ensure(
            report.memory_graph.matches_db_generation,
            true,
            "generation match",
        )?;
        ensure(report.snapshot_generation, Some(0), "snapshot generation")?;
        ensure(report.last_built_at.is_some(), true, "last built timestamp")
    }

    #[test]
    fn status_gather_inspects_current_workspace_by_default() -> TestResult {
        let report = StatusReport::gather();
        let search_index = report
            .derived_assets
            .iter()
            .find(|asset| asset.name == "search_index")
            .ok_or_else(|| "missing search_index asset".to_string())?;

        // After fix: gather() uses current directory as workspace, so status
        // should be something other than NotInspected (e.g., Missing, Ready, Degraded).
        ensure(
            search_index.status != DerivedAssetStatus::NotInspected,
            true,
            "search index should be inspected when current dir is workspace",
        )
    }

    fn make_ttl_policy(
        id: &str,
        review_state: &str,
        threshold_seconds: u64,
        action: &str,
        auto_promote_enabled: bool,
    ) -> StoredCurationTtlPolicy {
        StoredCurationTtlPolicy {
            id: id.to_owned(),
            review_state: review_state.to_owned(),
            threshold_seconds,
            action: action.to_owned(),
            requires_evidence_count: 0,
            requires_distinct_sessions: 0,
            requires_no_harmful_within_seconds: None,
            auto_promote_enabled,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    fn make_candidate(
        id: &str,
        review_state: &str,
        created_at: &str,
        state_entered_at: Option<&str>,
        ttl_policy_id: Option<&str>,
        reviewed_at: Option<&str>,
    ) -> StoredCurationCandidate {
        StoredCurationCandidate {
            id: id.to_owned(),
            workspace_id: "wsp_test".to_owned(),
            candidate_type: "promote".to_owned(),
            target_memory_id: "mem_test".to_owned(),
            proposed_content: None,
            proposed_confidence: Some(0.8),
            proposed_trust_class: None,
            source_type: "feedback_event".to_owned(),
            source_id: None,
            reason: "Test candidate".to_owned(),
            confidence: 0.7,
            status: "pending".to_owned(),
            created_at: created_at.to_owned(),
            reviewed_at: reviewed_at.map(|s| s.to_owned()),
            reviewed_by: None,
            applied_at: None,
            ttl_expires_at: None,
            review_state: review_state.to_owned(),
            snoozed_until: None,
            merged_into_candidate_id: None,
            state_entered_at: state_entered_at.map(|s| s.to_owned()),
            last_action_at: None,
            ttl_policy_id: ttl_policy_id.map(|s| s.to_owned()),
        }
    }

    fn status_denied_mesh_link_metadata() -> String {
        serde_json::json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_status_denied",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_status_decision_denied",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }

    #[test]
    fn curation_health_empty_candidates_returns_empty_status() -> TestResult {
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            1209600,
            "snooze",
            false,
        )];
        let now = chrono::Utc::now();

        let report = curation_health_from_rows(&[], &policies, now);

        ensure(report.status, CurationHealthStatus::Empty, "empty status")?;
        ensure(report.total_count, 0, "total count")?;
        ensure(report.policy_count, 1, "policy count")
    }

    #[test]
    fn curation_health_healthy_when_all_within_ttl() -> TestResult {
        let now = parse_ts("2026-05-01T12:00:00Z")?;
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            1209600,
            "snooze",
            false,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "new",
            "2026-04-25T12:00:00Z",
            Some("2026-04-25T12:00:00Z"),
            Some("policy_new"),
            None,
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(
            report.status,
            CurationHealthStatus::Healthy,
            "healthy status",
        )?;
        ensure(report.due_count, 0, "no due candidates")?;
        ensure(report.pending_count, 1, "one pending")?;
        ensure(
            report.next_scheduled_at.is_some(),
            true,
            "next scheduled should be set",
        )
    }

    #[test]
    fn curation_health_due_when_past_ttl_threshold() -> TestResult {
        let now = parse_ts("2026-05-20T12:00:00Z")?;
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            1209600,
            "snooze",
            false,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "new",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_new"),
            None,
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(report.status, CurationHealthStatus::Due, "due status")?;
        ensure(report.due_count, 1, "one due candidate")
    }

    #[test]
    fn curation_health_boundary_ttl_minus_one_second_not_due() -> TestResult {
        let state_entered = parse_ts("2026-04-01T12:00:00Z")?;
        let ttl_seconds = 1209600_u64;
        let now = state_entered
            + chrono::Duration::seconds(i64::try_from(ttl_seconds).map_err(|e| e.to_string())? - 1);
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            ttl_seconds,
            "snooze",
            false,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "new",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_new"),
            None,
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(report.status, CurationHealthStatus::Healthy, "not due yet")?;
        ensure(report.due_count, 0, "zero due at TTL-1s")
    }

    #[test]
    fn curation_health_boundary_ttl_exactly_is_due() -> TestResult {
        let state_entered = parse_ts("2026-04-01T12:00:00Z")?;
        let ttl_seconds = 1209600_u64;
        let now = state_entered
            + chrono::Duration::seconds(i64::try_from(ttl_seconds).map_err(|e| e.to_string())?);
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            ttl_seconds,
            "snooze",
            false,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "new",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_new"),
            None,
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(report.status, CurationHealthStatus::Due, "due at exact TTL")?;
        ensure(report.due_count, 1, "one due at TTL exactly")
    }

    #[test]
    fn curation_health_boundary_ttl_plus_one_second_is_due() -> TestResult {
        let state_entered = parse_ts("2026-04-01T12:00:00Z")?;
        let ttl_seconds = 1209600_u64;
        let now = state_entered
            + chrono::Duration::seconds(i64::try_from(ttl_seconds).map_err(|e| e.to_string())? + 1);
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            ttl_seconds,
            "snooze",
            false,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "new",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_new"),
            None,
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(report.status, CurationHealthStatus::Due, "due past TTL")?;
        ensure(report.due_count, 1, "one due at TTL+1s")
    }

    #[test]
    fn curation_health_legacy_candidate_without_state_entered_at_falls_back() -> TestResult {
        let now = parse_ts("2026-05-20T12:00:00Z")?;
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            1209600,
            "snooze",
            false,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "new",
            "2026-04-01T12:00:00Z",
            None,
            Some("policy_new"),
            None,
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(
            report.status,
            CurationHealthStatus::Due,
            "due using created_at fallback",
        )?;
        ensure(report.due_count, 1, "one due from fallback")
    }

    #[test]
    fn curation_health_escalated_when_escalate_action_fires() -> TestResult {
        let now = parse_ts("2026-05-20T12:00:00Z")?;
        let policies = vec![make_ttl_policy(
            "policy_harmful",
            "rejected",
            604800,
            "escalate",
            false,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "rejected",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_harmful"),
            Some("2026-04-01T12:00:00Z"),
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(report.status, CurationHealthStatus::Escalated, "escalated")?;
        ensure(report.escalation_count, 1, "one escalation")
    }

    #[test]
    fn curation_health_blocked_when_policy_missing() -> TestResult {
        let now = parse_ts("2026-05-01T12:00:00Z")?;
        let policies = vec![];
        let candidates = vec![make_candidate(
            "curate_1",
            "new",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("nonexistent_policy"),
            None,
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(report.status, CurationHealthStatus::Degraded, "degraded")?;
        ensure(report.blocked_count, 1, "one blocked")
    }

    #[test]
    fn curation_health_counts_auto_promote_enabled_policies() -> TestResult {
        let now = chrono::Utc::now();
        let policies = vec![
            make_ttl_policy("p1", "new", 1209600, "snooze", false),
            make_ttl_policy("p2", "validated", 2592000, "prompt_promote", true),
            make_ttl_policy("p3", "snoozed", 7776000, "retire_with_audit", false),
        ];

        let report = curation_health_from_rows(&[], &policies, now);

        ensure(report.policy_count, 3, "three policies")?;
        ensure(
            report.auto_promote_enabled_count,
            1,
            "one auto-promote enabled",
        )
    }

    #[test]
    fn curation_health_computes_mean_review_latency() -> TestResult {
        let now = chrono::Utc::now();
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            1209600,
            "snooze",
            false,
        )];
        let mut c1 = make_candidate(
            "curate_1",
            "accepted",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_new"),
            Some("2026-04-05T12:00:00Z"),
        );
        c1.review_state = "accepted".to_owned();
        let mut c2 = make_candidate(
            "curate_2",
            "accepted",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_new"),
            Some("2026-04-09T12:00:00Z"),
        );
        c2.review_state = "accepted".to_owned();

        let report = curation_health_from_rows(&[c1, c2], &policies, now);

        ensure(report.accepted_count, 2, "two accepted")?;
        ensure(
            report.mean_review_latency_days,
            Some(6),
            "mean latency 6 days",
        )
    }

    #[test]
    fn curation_health_tracks_oldest_pending_age() -> TestResult {
        let now = parse_ts("2026-05-01T12:00:00Z")?;
        let policies = vec![make_ttl_policy(
            "policy_new",
            "new",
            9999999,
            "snooze",
            false,
        )];
        let candidates = vec![
            make_candidate(
                "curate_1",
                "new",
                "2026-04-20T12:00:00Z",
                Some("2026-04-20T12:00:00Z"),
                Some("policy_new"),
                None,
            ),
            make_candidate(
                "curate_2",
                "new",
                "2026-04-01T12:00:00Z",
                Some("2026-04-01T12:00:00Z"),
                Some("policy_new"),
                None,
            ),
        ];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(
            report.oldest_pending_age_days,
            Some(30),
            "oldest is 30 days",
        )
    }

    #[test]
    fn curation_health_degradations_reports_escalations() -> TestResult {
        let health = CurationHealthReport {
            status: CurationHealthStatus::Escalated,
            total_count: 1,
            pending_count: 0,
            accepted_count: 0,
            snoozed_count: 0,
            rejected_count: 1,
            due_count: 1,
            prompt_count: 0,
            escalation_count: 1,
            blocked_count: 0,
            policy_count: 1,
            auto_promote_enabled_count: 0,
            oldest_pending_age_days: None,
            mean_review_latency_days: None,
            next_scheduled_at: None,
        };

        let degradations = curation_health_degradations(&health);

        ensure(degradations.len(), 1, "one degradation")?;
        ensure(
            degradations[0].code,
            "curation_harmful_candidate_escalated",
            "escalation code",
        )
    }

    #[test]
    fn curation_health_prompt_promote_counted_separately() -> TestResult {
        let now = parse_ts("2026-06-01T12:00:00Z")?;
        let policies = vec![make_ttl_policy(
            "policy_validated",
            "validated",
            2592000,
            "prompt_promote",
            true,
        )];
        let candidates = vec![make_candidate(
            "curate_1",
            "validated",
            "2026-04-01T12:00:00Z",
            Some("2026-04-01T12:00:00Z"),
            Some("policy_validated"),
            Some("2026-04-02T12:00:00Z"),
        )];

        let report = curation_health_from_rows(&candidates, &policies, now);

        ensure(report.due_count, 1, "one due")?;
        ensure(report.prompt_count, 1, "one prompt_promote")
    }
}
