//! Health command handler (EE-026).
//!
//! Provides a quick health check summary with an overall healthy/unhealthy
//! verdict and counts of issues by severity. Simpler than doctor (no fix plans),
//! more binary than status (which reports detailed readiness states).

use std::path::Path;

use crate::models::CapabilityStatus;

use super::build_info;
use super::status::{
    default_workspace_path, probe_runtime_capability, probe_search_capability,
    probe_storage_capability,
};

/// Overall health verdict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthVerdict {
    Healthy,
    Degraded,
    Unhealthy,
}

impl HealthVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }

    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy)
    }
}

/// A single health issue.
#[derive(Clone, Debug)]
pub struct HealthIssue {
    pub subsystem: &'static str,
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
}

/// Health report returned by the health command.
#[derive(Clone, Debug)]
pub struct HealthReport {
    pub version: &'static str,
    pub verdict: HealthVerdict,
    pub runtime_ok: bool,
    pub storage_ok: bool,
    pub search_ok: bool,
    pub issues: Vec<HealthIssue>,
}

impl HealthReport {
    /// Gather current health status.
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
        let version = build_info().version;
        let runtime_status = probe_runtime_capability();
        let runtime_ok = runtime_status == CapabilityStatus::Ready;
        let storage_status = probe_storage_capability(workspace_path);
        let search_status = probe_search_capability(workspace_path);
        let storage_ok = storage_status == CapabilityStatus::Ready;
        let search_ok = search_status == CapabilityStatus::Ready;

        let mut issues = Vec::new();

        if !runtime_ok {
            issues.push(HealthIssue {
                subsystem: "runtime",
                code: "runtime_unavailable",
                severity: "high",
                message: "Asupersync runtime failed to initialize.",
            });
        }

        if storage_status == CapabilityStatus::Pending {
            issues.push(HealthIssue {
                subsystem: "storage",
                code: "storage_not_ready",
                severity: "medium",
                message: "Workspace storage has not been initialized.",
            });
        } else if !storage_ok {
            issues.push(HealthIssue {
                subsystem: "storage",
                code: "storage_degraded",
                severity: "high",
                message: "Workspace storage exists but failed readiness checks.",
            });
        }

        if search_status == CapabilityStatus::Pending {
            issues.push(HealthIssue {
                subsystem: "search",
                code: "search_not_ready",
                severity: "medium",
                message: "Search is waiting for workspace storage before it can be inspected.",
            });
        } else if !search_ok {
            issues.push(HealthIssue {
                subsystem: "search",
                code: "search_index_degraded",
                severity: "medium",
                message: "Search is compiled but the selected workspace index is not ready.",
            });
        }

        let verdict = if issues.is_empty() {
            HealthVerdict::Healthy
        } else if issues.iter().any(|i| i.severity == "high") {
            HealthVerdict::Unhealthy
        } else {
            HealthVerdict::Degraded
        };

        Self {
            version,
            verdict,
            runtime_ok,
            storage_ok,
            search_ok,
            issues,
        }
    }

    /// Count of issues by severity.
    #[must_use]
    pub fn issue_count(&self) -> usize {
        self.issues.len()
    }

    /// Count of high-severity issues.
    #[must_use]
    pub fn high_severity_count(&self) -> usize {
        self.issues.iter().filter(|i| i.severity == "high").count()
    }

    /// Count of medium-severity issues.
    #[must_use]
    pub fn medium_severity_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == "medium")
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn health_report_gather_returns_valid_report() -> TestResult {
        let report = HealthReport::gather_with_workspace(None);

        ensure(
            report.version,
            env!("CARGO_PKG_VERSION"),
            "version from cargo",
        )?;
        ensure(report.runtime_ok, true, "runtime is ok")?;
        ensure(report.storage_ok, false, "storage not yet ok")?;
        ensure(report.search_ok, false, "search not yet ok")
    }

    #[test]
    fn health_report_verdict_is_degraded_when_medium_issues() -> TestResult {
        let report = HealthReport::gather_with_workspace(None);

        ensure(
            report.verdict,
            HealthVerdict::Degraded,
            "verdict is degraded with medium issues",
        )?;
        ensure(
            report.verdict.as_str(),
            "degraded",
            "verdict string is degraded",
        )
    }

    #[test]
    fn health_report_issue_counts_are_correct() -> TestResult {
        let report = HealthReport::gather_with_workspace(None);

        ensure(report.issue_count(), 2, "two issues total")?;
        ensure(report.high_severity_count(), 0, "no high severity issues")?;
        ensure(
            report.medium_severity_count(),
            2,
            "two medium severity issues",
        )
    }

    #[test]
    fn health_verdict_strings_are_stable() -> TestResult {
        ensure(HealthVerdict::Healthy.as_str(), "healthy", "healthy")?;
        ensure(HealthVerdict::Degraded.as_str(), "degraded", "degraded")?;
        ensure(HealthVerdict::Unhealthy.as_str(), "unhealthy", "unhealthy")
    }

    #[test]
    fn health_verdict_is_healthy_predicate() -> TestResult {
        ensure(
            HealthVerdict::Healthy.is_healthy(),
            true,
            "healthy is healthy",
        )?;
        ensure(
            HealthVerdict::Degraded.is_healthy(),
            false,
            "degraded is not healthy",
        )?;
        ensure(
            HealthVerdict::Unhealthy.is_healthy(),
            false,
            "unhealthy is not healthy",
        )
    }
}
