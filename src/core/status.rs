//! Status command handler (EE-024).
//!
//! Gathers subsystem status data and returns a structured report that
//! the output layer renders as JSON or human-readable text.

use std::collections::BTreeMap;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use crate::config::{
    WorkspaceDiagnostic, WorkspaceDiagnosticSeverity, WorkspaceResolution, WorkspaceResolutionMode,
    WorkspaceResolutionRequest, WorkspaceResolutionSource, diagnose_workspace_resolution,
    resolve_workspace,
};
use crate::db::{
    CreateWorkspaceInput, DbConnection, FeedbackSourceHarmfulCount, PROVENANCE_CHAIN_HASH_VERSION,
    PROVENANCE_STATUS_UNVERIFIED, StoredCurationCandidate, StoredCurationTtlPolicy, StoredMemory,
    default_curation_ttl_policy_id_for_review_state,
};
use crate::models::{CapabilityStatus, MemoryId};

use super::agent_detect::AgentInventoryReport;
use super::curate::stable_workspace_id;
use super::index::{IndexHealth, IndexStatusOptions, get_index_status};
use super::outcome::{DEFAULT_HARMFUL_BURST_WINDOW_SECONDS, DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR};
use super::{build_info, runtime_status};

const MEMORY_STALE_AFTER_DAYS: i64 = 30;

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
    pub status: DerivedAssetStatus,
    pub source_high_watermark: Option<u64>,
    pub asset_high_watermark: Option<u64>,
    pub high_watermark_lag: Option<u64>,
    pub path: &'static str,
    pub repair: Option<&'static str>,
}

impl DerivedAssetReport {
    #[must_use]
    pub const fn not_inspected(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            status: DerivedAssetStatus::NotInspected,
            source_high_watermark: None,
            asset_high_watermark: None,
            high_watermark_lag: None,
            path,
            repair: Some("Run `ee status --workspace . --json` to inspect this asset."),
        }
    }

    #[must_use]
    pub const fn unimplemented(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            status: DerivedAssetStatus::Unimplemented,
            source_high_watermark: None,
            asset_high_watermark: None,
            high_watermark_lag: None,
            path,
            repair: Some("Implement the persistent derived asset before reporting a watermark."),
        }
    }

    #[must_use]
    pub const fn unavailable(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            status: DerivedAssetStatus::Unavailable,
            source_high_watermark: None,
            asset_high_watermark: None,
            high_watermark_lag: None,
            path,
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
            status,
            source_high_watermark: report.db_generation,
            asset_high_watermark: report.index_generation,
            high_watermark_lag: high_watermark_lag(report.db_generation, report.index_generation),
            path: ".ee/index",
            repair: report.repair_hint,
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
            runtime: CapabilityStatus::Ready,
            storage: probe_storage_capability(workspace_path),
            search: probe_search_capability(workspace_path),
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

fn workspace_database_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("ee.db")
}

#[must_use]
pub fn probe_cass_capability() -> CapabilityStatus {
    use std::process::Command;
    match Command::new("cass").arg("--version").output() {
        Ok(output) if output.status.success() => CapabilityStatus::Ready,
        Ok(_) => CapabilityStatus::Degraded,
        Err(_) => CapabilityStatus::Pending,
    }
}

#[must_use]
pub fn probe_runtime_capability() -> CapabilityStatus {
    match super::build_cli_runtime() {
        Ok(_) => CapabilityStatus::Ready,
        Err(_) => CapabilityStatus::Degraded,
    }
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
            source_id: value.source_id,
            harmful_count: value.harmful_count,
        }
    }
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
#[derive(Clone, Debug)]
pub struct DegradationReport {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

/// Full status report returned by the status command.
#[derive(Clone, Debug)]
pub struct StatusReport {
    pub version: &'static str,
    pub workspace: Option<WorkspaceStatusReport>,
    pub capabilities: CapabilityReport,
    pub runtime: RuntimeReport,
    pub memory_health: MemoryHealthReport,
    pub curation_health: CurationHealthReport,
    pub feedback_health: FeedbackHealthReport,
    pub derived_assets: Vec<DerivedAssetReport>,
    pub agent_inventory: AgentInventoryReport,
    pub degradations: Vec<DegradationReport>,
}

impl StatusReport {
    /// Gather current subsystem status.
    #[must_use]
    pub fn gather() -> Self {
        Self::gather_with_options(&StatusOptions::default())
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
        let (memory_health, memory_health_degradations) =
            gather_memory_health(options.workspace_path.as_deref());
        let workspace = gather_workspace_status(options.workspace_path.as_deref());
        let derived_assets = gather_derived_assets(options.workspace_path.as_deref());
        let (curation_health, curation_degradations) =
            gather_curation_health(options.workspace_path.as_deref());
        let (feedback_health, feedback_degradations) =
            gather_feedback_health(options.workspace_path.as_deref());
        let agent_inventory = AgentInventoryReport::not_inspected();

        let mut degradations = Vec::new();

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

        degradations.extend(memory_health_degradations);
        degradations.extend(curation_degradations);
        degradations.extend(feedback_degradations);

        Self {
            version: build_info().version,
            workspace,
            capabilities,
            runtime,
            memory_health,
            curation_health,
            feedback_health,
            derived_assets,
            agent_inventory,
            degradations,
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

fn gather_derived_assets(workspace_path: Option<&Path>) -> Vec<DerivedAssetReport> {
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

    let graph_snapshot = DerivedAssetReport::unimplemented("graph_snapshot", ".ee/graph");

    vec![search_index, graph_snapshot]
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

    (memory_health_from_rows(&memories, Utc::now()), Vec::new())
}

fn memory_health_from_rows(memories: &[StoredMemory], now: DateTime<Utc>) -> MemoryHealthReport {
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
        if memory_row_is_stale(memory, now) {
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

    let mut report = MemoryHealthReport {
        status: MemoryHealthStatus::Healthy,
        total_count,
        active_count,
        tombstoned_count,
        stale_count,
        average_confidence,
        provenance_coverage,
        health_score: None,
        score_components: None,
    }
    .with_conservative_score();

    report.status = match report.health_score {
        _ if active_count == 0 => MemoryHealthStatus::Degraded,
        Some(score) if score >= 0.5 => MemoryHealthStatus::Healthy,
        _ => MemoryHealthStatus::Degraded,
    };

    report
}

fn memory_row_is_stale(memory: &StoredMemory, now: DateTime<Utc>) -> bool {
    let Some(reference) = parse_memory_timestamp(&memory.updated_at)
        .or_else(|| parse_memory_timestamp(&memory.created_at))
    else {
        return true;
    };
    now.signed_duration_since(reference).num_days() >= MEMORY_STALE_AFTER_DAYS
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
    use std::path::PathBuf;

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
        ensure(report.stale_count, 1, "stale count")?;
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
        ensure(components.freshness_score, 0.5, "freshness")?;
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
    fn status_without_workspace_reports_not_inspected_asset() -> TestResult {
        let report = StatusReport::gather();
        let search_index = report
            .derived_assets
            .iter()
            .find(|asset| asset.name == "search_index")
            .ok_or_else(|| "missing search_index asset".to_string())?;

        ensure(
            search_index.status,
            DerivedAssetStatus::NotInspected,
            "search index status",
        )?;
        ensure(
            search_index.asset_high_watermark,
            None,
            "no asset watermark without workspace",
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
