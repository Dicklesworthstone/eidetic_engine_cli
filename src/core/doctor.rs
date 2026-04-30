//! Doctor command handler (EE-025, EE-241).
//!
//! Performs health checks on workspace subsystems and returns a structured
//! report with issues and repair suggestions.
//!
//! The `--fix-plan` flag (EE-241) outputs a structured repair plan that
//! agents can execute step-by-step.

use crate::models::error_codes::{self, ErrorCode};

/// Severity of a doctor check issue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckSeverity {
    Ok,
    Warning,
    Error,
}

impl CheckSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Result of a single health check.
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub name: &'static str,
    pub severity: CheckSeverity,
    pub message: String,
    pub error_code: Option<ErrorCode>,
    pub repair: Option<&'static str>,
}

impl CheckResult {
    #[must_use]
    pub fn ok(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            severity: CheckSeverity::Ok,
            message: message.into(),
            error_code: None,
            repair: None,
        }
    }

    #[must_use]
    pub fn warning(name: &'static str, message: impl Into<String>, error_code: ErrorCode) -> Self {
        Self {
            name,
            severity: CheckSeverity::Warning,
            message: message.into(),
            error_code: Some(error_code),
            repair: error_code.default_repair,
        }
    }

    #[must_use]
    pub fn error(name: &'static str, message: impl Into<String>, error_code: ErrorCode) -> Self {
        Self {
            name,
            severity: CheckSeverity::Error,
            message: message.into(),
            error_code: Some(error_code),
            repair: error_code.default_repair,
        }
    }
}

/// Full doctor report.
#[derive(Clone, Debug)]
pub struct DoctorReport {
    pub version: &'static str,
    pub overall_healthy: bool,
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    /// Run all health checks and return a report.
    #[must_use]
    pub fn gather() -> Self {
        let checks = vec![
            check_runtime(),
            check_workspace(),
            check_database(),
            check_search_index(),
            check_cass(),
        ];

        let overall_healthy = checks.iter().all(|c| c.severity.is_healthy());

        Self {
            version: env!("CARGO_PKG_VERSION"),
            overall_healthy,
            checks,
        }
    }

    /// Convert the doctor report into a structured fix plan.
    #[must_use]
    pub fn to_fix_plan(&self) -> FixPlan {
        let steps: Vec<FixStep> = self
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy() && c.repair.is_some())
            .enumerate()
            .map(|(idx, check)| FixStep {
                order: idx + 1,
                subsystem: check.name,
                severity: check.severity,
                issue: check.message.clone(),
                error_code: check.error_code,
                command: check.repair.unwrap_or_default(),
            })
            .collect();

        let total_issues = self
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy())
            .count();
        let fixable_issues = steps.len();

        FixPlan {
            version: self.version,
            total_issues,
            fixable_issues,
            steps,
        }
    }
}

/// A structured repair plan generated from doctor checks.
#[derive(Clone, Debug)]
pub struct FixPlan {
    pub version: &'static str,
    pub total_issues: usize,
    pub fixable_issues: usize,
    pub steps: Vec<FixStep>,
}

impl FixPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// A single repair step in a fix plan.
#[derive(Clone, Debug)]
pub struct FixStep {
    pub order: usize,
    pub subsystem: &'static str,
    pub severity: CheckSeverity,
    pub issue: String,
    pub error_code: Option<ErrorCode>,
    pub command: &'static str,
}

fn check_runtime() -> CheckResult {
    CheckResult::ok("runtime", "Asupersync runtime is available.")
}

fn check_workspace() -> CheckResult {
    CheckResult::warning(
        "workspace",
        "No workspace specified. Use --workspace or run from a workspace directory.",
        error_codes::WORKSPACE_NOT_SPECIFIED,
    )
}

fn check_database() -> CheckResult {
    CheckResult::warning(
        "database",
        "Database subsystem is not yet implemented.",
        error_codes::DATABASE_NOT_FOUND,
    )
}

fn check_search_index() -> CheckResult {
    CheckResult::warning(
        "search_index",
        "Search index subsystem is not yet implemented.",
        error_codes::INDEX_NOT_FOUND,
    )
}

fn check_cass() -> CheckResult {
    CheckResult::ok("cass", "CASS binary discovery is available.")
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
    fn doctor_report_gather_returns_checks() -> TestResult {
        let report = DoctorReport::gather();

        ensure(
            report.checks.len() >= 5,
            true,
            "should have at least 5 checks",
        )?;

        let runtime = report.checks.iter().find(|c| c.name == "runtime");
        ensure(runtime.is_some(), true, "runtime check exists")?;
        ensure(
            runtime.map(|c| c.severity),
            Some(CheckSeverity::Ok),
            "runtime is ok",
        )?;

        Ok(())
    }

    #[test]
    fn doctor_report_overall_healthy_reflects_all_checks() -> TestResult {
        let report = DoctorReport::gather();

        let has_issues = report.checks.iter().any(|c| !c.severity.is_healthy());

        ensure(
            report.overall_healthy,
            !has_issues,
            "overall_healthy matches check status",
        )
    }

    #[test]
    fn check_result_ok_has_no_error_code() -> TestResult {
        let check = CheckResult::ok("test", "All good");
        ensure(check.error_code.is_none(), true, "ok has no error code")?;
        ensure(check.repair.is_none(), true, "ok has no repair")
    }

    #[test]
    fn check_result_warning_has_error_code_and_repair() -> TestResult {
        let check = CheckResult::warning("test", "Issue found", error_codes::DATABASE_NOT_FOUND);
        ensure(check.error_code.is_some(), true, "warning has error code")?;
        ensure(check.repair.is_some(), true, "warning has repair from code")
    }

    #[test]
    fn check_severity_strings_are_stable() -> TestResult {
        ensure(CheckSeverity::Ok.as_str(), "ok", "ok")?;
        ensure(CheckSeverity::Warning.as_str(), "warning", "warning")?;
        ensure(CheckSeverity::Error.as_str(), "error", "error")
    }

    #[test]
    fn fix_plan_contains_only_fixable_issues() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        for step in &plan.steps {
            ensure(!step.command.is_empty(), true, "step has a command")?;
            ensure(
                step.severity != CheckSeverity::Ok,
                true,
                "step is not an ok check",
            )?;
        }

        Ok(())
    }

    #[test]
    fn fix_plan_steps_are_ordered() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        for (idx, step) in plan.steps.iter().enumerate() {
            ensure(step.order, idx + 1, "step order is sequential")?;
        }

        Ok(())
    }

    #[test]
    fn fix_plan_counts_match() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        let unhealthy_count = report
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy())
            .count();
        ensure(plan.total_issues, unhealthy_count, "total_issues matches")?;

        let fixable_count = report
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy() && c.repair.is_some())
            .count();
        ensure(plan.fixable_issues, fixable_count, "fixable_issues matches")?;
        ensure(plan.steps.len(), fixable_count, "steps count matches")?;

        Ok(())
    }

    #[test]
    fn fix_plan_is_empty_when_all_healthy() -> TestResult {
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: true,
            checks: vec![
                CheckResult::ok("test1", "All good"),
                CheckResult::ok("test2", "Also good"),
            ],
        };
        let plan = report.to_fix_plan();

        ensure(plan.is_empty(), true, "plan is empty when all healthy")?;
        ensure(plan.total_issues, 0, "no total issues")?;
        ensure(plan.fixable_issues, 0, "no fixable issues")
    }
}
