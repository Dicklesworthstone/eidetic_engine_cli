//! Status command handler (EE-024).
//!
//! Gathers subsystem status data and returns a structured report that
//! the output layer renders as JSON or human-readable text.

use std::path::{Path, PathBuf};

use crate::models::CapabilityStatus;

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
        }
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

/// Describes the readiness of each ee subsystem.
#[derive(Clone, Debug)]
pub struct CapabilityReport {
    pub runtime: CapabilityStatus,
    pub storage: CapabilityStatus,
    pub search: CapabilityStatus,
}

impl CapabilityReport {
    #[must_use]
    pub fn gather() -> Self {
        Self {
            runtime: CapabilityStatus::Ready,
            storage: CapabilityStatus::Unimplemented,
            search: CapabilityStatus::Unimplemented,
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
    pub capabilities: CapabilityReport,
    pub runtime: RuntimeReport,
    pub memory_health: MemoryHealthReport,
    pub derived_assets: Vec<DerivedAssetReport>,
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
        let derived_assets = gather_derived_assets(options.workspace_path.as_deref());

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

        Self {
            version: build_info().version,
            capabilities,
            runtime,
            memory_health,
            derived_assets,
            degradations,
        }
    }
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
