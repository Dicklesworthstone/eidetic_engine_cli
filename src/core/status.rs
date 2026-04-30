//! Status command handler (EE-024).
//!
//! Gathers subsystem status data and returns a structured report that
//! the output layer renders as JSON or human-readable text.

use crate::models::CapabilityStatus;

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
    pub degradations: Vec<DegradationReport>,
}

impl StatusReport {
    /// Gather current subsystem status.
    #[must_use]
    pub fn gather() -> Self {
        let capabilities = CapabilityReport::gather();
        let runtime = RuntimeReport::gather();
        let memory_health = MemoryHealthReport::gather();

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
            degradations,
        }
    }
}

#[cfg(test)]
mod tests {
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
        Ok(())
    }

    #[test]
    fn status_report_includes_degradations_for_unimplemented_subsystems() -> TestResult {
        let report = StatusReport::gather();

        ensure(report.degradations.len(), 2, "two degradations expected")?;

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
}
