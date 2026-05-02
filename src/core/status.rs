//! Status command handler (EE-024).
//!
//! Gathers subsystem status data and returns a structured report that
//! the output layer renders as JSON or human-readable text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::config::{
    WorkspaceDiagnostic, WorkspaceDiagnosticSeverity, WorkspaceResolution, WorkspaceResolutionMode,
    WorkspaceResolutionRequest, WorkspaceResolutionSource, diagnose_workspace_resolution,
    resolve_workspace,
};
use crate::db::{
    DbConnection, StoredCurationCandidate, StoredCurationTtlPolicy,
    default_curation_ttl_policy_id_for_review_state,
};
use crate::models::CapabilityStatus;

use super::agent_detect::AgentInventoryReport;
use super::curate::stable_workspace_id;
use super::index::{IndexHealth, IndexStatusOptions, get_index_status};
use super::{build_info, runtime_status};

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
    /// Gather memory health (stub until storage is wired).
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
        Self {
            runtime: CapabilityStatus::Ready,
            storage: CapabilityStatus::Unimplemented,
            search: CapabilityStatus::Unimplemented,
            agent_detection: CapabilityStatus::Ready,
        }
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
        let capabilities = CapabilityReport::gather();
        let runtime = RuntimeReport::gather();
        let memory_health = MemoryHealthReport::gather();
        let workspace = gather_workspace_status(options.workspace_path.as_deref());
        let derived_assets = gather_derived_assets(options.workspace_path.as_deref());
        let (curation_health, curation_degradations) =
            gather_curation_health(options.workspace_path.as_deref());
        let agent_inventory = AgentInventoryReport::not_inspected();

        let mut degradations = Vec::new();

        if capabilities.storage == CapabilityStatus::Unimplemented {
            degradations.push(DegradationReport {
                code: "storage_not_implemented",
                severity: "medium",
                message: "Storage subsystem is not wired yet.",
                repair: "Implement EE-040 through EE-044.",
            });
        }

        if capabilities.search == CapabilityStatus::Unimplemented {
            degradations.push(DegradationReport {
                code: "search_not_implemented",
                severity: "medium",
                message: "Search subsystem is not wired yet.",
                repair: "Implement EE-120 and dependent search beads.",
            });
        }

        if memory_health.status == MemoryHealthStatus::Unavailable {
            degradations.push(DegradationReport {
                code: "memory_health_unavailable",
                severity: "low",
                message: "Memory health metrics unavailable until storage is wired.",
                repair: "Implement storage subsystem.",
            });
        }

        degradations.extend(curation_degradations);

        Self {
            version: build_info().version,
            workspace,
            capabilities,
            runtime,
            memory_health,
            curation_health,
            derived_assets,
            agent_inventory,
            degradations,
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
            reviewed_latencies
                .push(reviewed.signed_duration_since(created).num_days().max(0));
        }

        if pending_count > 0
            && let Ok(created) = DateTime::parse_from_rfc3339(&candidate.created_at)
        {
            let age = now
                .signed_duration_since(created.with_timezone(&Utc))
                .num_days()
                .max(0);
            oldest_pending_age_days = Some(oldest_pending_age_days.map_or(age, |oldest| {
                std::cmp::max(oldest, age)
            }));
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

    #[test]
    fn status_report_gather_returns_valid_report() -> TestResult {
        let report = StatusReport::gather();

        ensure(
            report.capabilities.runtime,
            CapabilityStatus::Ready,
            "runtime should be ready",
        )?;
        ensure(
            report.capabilities.storage,
            CapabilityStatus::Unimplemented,
            "storage not yet implemented",
        )?;
        ensure(
            report.capabilities.search,
            CapabilityStatus::Unimplemented,
            "search not yet implemented",
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
        let report = StatusReport::gather();

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
    fn status_report_includes_degradations_for_unimplemented_subsystems() -> TestResult {
        let report = StatusReport::gather();

        ensure(report.degradations.len(), 3, "three degradations expected")?;

        let storage_deg = report
            .degradations
            .iter()
            .find(|d| d.code == "storage_not_implemented");
        ensure(storage_deg.is_some(), true, "storage degradation exists")?;

        let search_deg = report
            .degradations
            .iter()
            .find(|d| d.code == "search_not_implemented");
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
}
