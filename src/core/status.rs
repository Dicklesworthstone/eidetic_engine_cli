//! Status command handler (EE-024).
//!
//! Gathers subsystem status data and returns a structured report that
//! the output layer renders as JSON or human-readable text.

use crate::models::CapabilityStatus;

use super::{build_info, runtime_status};

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
    pub degradations: Vec<DegradationReport>,
}

impl StatusReport {
    /// Gather current subsystem status.
    #[must_use]
    pub fn gather() -> Self {
        let capabilities = CapabilityReport::gather();
        let runtime = RuntimeReport::gather();

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

        Self {
            version: build_info().version,
            capabilities,
            runtime,
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
