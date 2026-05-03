//! Degraded-mode honesty checks (EE-253).
//!
//! Validates that degraded responses are honest and actionable:
//! - Severity levels accurately reflect the impact
//! - Repair fields contain valid, executable commands
//! - Messages are clear and informative
//!
//! These checks prevent the system from understating problems or providing
//! unhelpful repair suggestions.

use crate::models::degradation::{
    ALL_DEGRADATION_CODES, ActiveDegradation, DegradationCode, DegradationSeverity,
};

/// Result of a single honesty check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HonestyCheckResult {
    /// What was checked.
    pub check_name: &'static str,
    /// Whether the check passed.
    pub passed: bool,
    /// Issue description if failed.
    pub issue: Option<String>,
    /// Degradation code being validated.
    pub code_id: Option<String>,
}

impl HonestyCheckResult {
    /// Create a passing check.
    #[must_use]
    pub const fn pass(check_name: &'static str) -> Self {
        Self {
            check_name,
            passed: true,
            issue: None,
            code_id: None,
        }
    }

    /// Create a passing check for a specific code.
    #[must_use]
    pub fn pass_for(check_name: &'static str, code_id: &str) -> Self {
        Self {
            check_name,
            passed: true,
            issue: None,
            code_id: Some(code_id.to_owned()),
        }
    }

    /// Create a failing check.
    #[must_use]
    pub fn fail(check_name: &'static str, issue: impl Into<String>) -> Self {
        Self {
            check_name,
            passed: false,
            issue: Some(issue.into()),
            code_id: None,
        }
    }

    /// Create a failing check for a specific code.
    #[must_use]
    pub fn fail_for(check_name: &'static str, code_id: &str, issue: impl Into<String>) -> Self {
        Self {
            check_name,
            passed: false,
            issue: Some(issue.into()),
            code_id: Some(code_id.to_owned()),
        }
    }
}

/// Summary of all honesty checks.
#[derive(Clone, Debug)]
pub struct HonestyReport {
    /// Individual check results.
    pub checks: Vec<HonestyCheckResult>,
    /// Overall pass/fail.
    pub passed: bool,
    /// Number of issues found.
    pub issue_count: u32,
}

impl HonestyReport {
    /// Create a report from check results.
    #[must_use]
    pub fn from_checks(checks: Vec<HonestyCheckResult>) -> Self {
        let failed_checks = checks.iter().filter(|c| !c.passed).count();
        let issue_count = u32::try_from(failed_checks).unwrap_or(u32::MAX);
        let passed = issue_count == 0;
        Self {
            checks,
            passed,
            issue_count,
        }
    }
}

/// Validate that a repair command looks actionable.
///
/// Repair commands should:
/// - Start with a known command prefix (ee, cargo, cass, etc.)
/// - Not be empty
/// - Not contain placeholder text
#[must_use]
pub fn validate_repair_command(repair: &str) -> HonestyCheckResult {
    if repair.is_empty() {
        return HonestyCheckResult::fail("repair_not_empty", "Repair command is empty");
    }

    let known_prefixes = ["ee ", "cargo ", "cass ", "chmod ", "rm ", "sqlite3 "];
    let starts_with_known = known_prefixes.iter().any(|p| repair.starts_with(p));

    if !starts_with_known {
        return HonestyCheckResult::fail(
            "repair_known_command",
            format!(
                "Repair command '{}' doesn't start with known prefix",
                repair
            ),
        );
    }

    let placeholder_patterns = ["TODO", "FIXME", "<placeholder>", "xxx", "???"];
    for pattern in placeholder_patterns {
        if repair.to_lowercase().contains(&pattern.to_lowercase()) {
            return HonestyCheckResult::fail(
                "repair_no_placeholders",
                format!("Repair command contains placeholder pattern: {}", pattern),
            );
        }
    }

    HonestyCheckResult::pass("repair_valid")
}

/// Validate that severity accurately reflects impact.
///
/// Critical should only be used when core functionality is impaired.
/// Advisory is for minor inconveniences.
/// Warning is for significant but non-blocking issues.
#[must_use]
pub fn validate_severity_honesty(code: &DegradationCode) -> HonestyCheckResult {
    match code.severity {
        DegradationSeverity::Critical => {
            if code.auto_recoverable {
                return HonestyCheckResult::fail_for(
                    "severity_critical_not_auto_recoverable",
                    code.id,
                    "Critical degradations should not be auto-recoverable",
                );
            }
        }
        DegradationSeverity::Advisory => {
            if !code.auto_recoverable && code.repair.is_none() {
                return HonestyCheckResult::fail_for(
                    "severity_advisory_has_path_forward",
                    code.id,
                    "Advisory degradations should be auto-recoverable or have repair",
                );
            }
        }
        DegradationSeverity::Warning => {}
    }

    HonestyCheckResult::pass_for("severity_honest", code.id)
}

/// Validate that message is informative.
///
/// Messages should:
/// - Not be empty
/// - Not be generic
/// - Describe what's wrong
#[must_use]
pub fn validate_message_quality(code: &DegradationCode) -> HonestyCheckResult {
    if code.description.is_empty() {
        return HonestyCheckResult::fail_for("message_not_empty", code.id, "Description is empty");
    }

    if code.description.len() < 10 {
        return HonestyCheckResult::fail_for(
            "message_informative",
            code.id,
            format!(
                "Description '{}' is too short to be informative",
                code.description
            ),
        );
    }

    let generic_patterns = [
        "error occurred",
        "something went wrong",
        "unknown error",
        "failed",
    ];
    for pattern in generic_patterns {
        if code.description.to_lowercase() == pattern {
            return HonestyCheckResult::fail_for(
                "message_not_generic",
                code.id,
                format!("Description '{}' is too generic", code.description),
            );
        }
    }

    if code.behavior_change.is_empty() {
        return HonestyCheckResult::fail_for(
            "behavior_change_documented",
            code.id,
            "Behavior change is not documented",
        );
    }

    HonestyCheckResult::pass_for("message_quality", code.id)
}

/// Validate all registered degradation codes for honesty.
#[must_use]
pub fn validate_all_codes() -> HonestyReport {
    let mut checks = Vec::new();

    for code in ALL_DEGRADATION_CODES {
        checks.push(validate_severity_honesty(code));
        checks.push(validate_message_quality(code));

        if let Some(repair) = code.repair {
            checks.push(validate_repair_command(repair));
        }
    }

    HonestyReport::from_checks(checks)
}

/// Validate a specific active degradation.
#[must_use]
pub fn validate_active_degradation(active: &ActiveDegradation) -> HonestyReport {
    let mut checks = Vec::new();

    checks.push(validate_severity_honesty(&active.code));
    checks.push(validate_message_quality(&active.code));

    if let Some(repair) = active.code.repair {
        checks.push(validate_repair_command(repair));
    }

    HonestyReport::from_checks(checks)
}

/// Check that degraded array in a response is honest.
///
/// This validates the runtime degraded state, not just code definitions.
#[must_use]
pub fn validate_degraded_response(
    degraded: &[(String, DegradationSeverity, String, Option<String>)],
) -> HonestyReport {
    let mut checks = Vec::new();

    for (code, _severity, message, repair) in degraded {
        if message.is_empty() {
            checks.push(HonestyCheckResult::fail_for(
                "response_message_not_empty",
                code,
                "Response message is empty",
            ));
        } else if message.len() < 10 {
            checks.push(HonestyCheckResult::fail_for(
                "response_message_informative",
                code,
                format!("Response message '{}' is too short", message),
            ));
        } else {
            checks.push(HonestyCheckResult::pass_for("response_message_ok", code));
        }

        if let Some(r) = repair {
            checks.push(validate_repair_command(r));
        }
    }

    HonestyReport::from_checks(checks)
}

/// Markers that should never appear in normal successful production output.
///
/// Fixture and eval commands may intentionally mention fixture identifiers, but
/// ordinary successful command output must not look like it came from a sample,
/// mock, or stub path. The list is intentionally small and literal so it is
/// explainable when a contract fails.
pub const FORBIDDEN_SUCCESS_MARKERS: &[&str] = &[
    "[sample]",
    "example_",
    "mock_",
    "sample_",
    "stub_",
    "stubbed",
    "stub success",
    "fixture_",
    "tests/fixtures/",
];

/// Validate that a successful production command did not return fake data.
///
/// Failed/degraded commands are allowed to explain that behavior is unavailable.
/// Fixture-mode commands are allowed to identify fixtures explicitly. Everything
/// else that reports success must not include sample/mock/stub markers.
#[must_use]
pub fn validate_no_fake_success_output(
    command_path: &str,
    success: bool,
    fixture_mode: bool,
    output: &str,
) -> HonestyReport {
    if !success {
        return HonestyReport::from_checks(vec![HonestyCheckResult::pass_for(
            "fake_success_not_applicable_for_failure",
            command_path,
        )]);
    }

    if fixture_mode {
        return HonestyReport::from_checks(vec![HonestyCheckResult::pass_for(
            "fake_success_allowed_in_fixture_mode",
            command_path,
        )]);
    }

    let lower_output = output.to_ascii_lowercase();
    let checks = FORBIDDEN_SUCCESS_MARKERS
        .iter()
        .map(|marker| {
            if lower_output.contains(marker) {
                HonestyCheckResult::fail_for(
                    "no_fake_success_output",
                    command_path,
                    format!("Successful production output contains fake-data marker `{marker}`"),
                )
            } else {
                HonestyCheckResult::pass_for("no_fake_success_output", command_path)
            }
        })
        .collect();

    HonestyReport::from_checks(checks)
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
    fn validate_repair_rejects_empty() -> TestResult {
        let result = validate_repair_command("");
        ensure(result.passed, false, "empty repair fails")
    }

    #[test]
    fn validate_repair_accepts_ee_command() -> TestResult {
        let result = validate_repair_command("ee index rebuild");
        ensure(result.passed, true, "ee command passes")
    }

    #[test]
    fn validate_repair_accepts_cass_command() -> TestResult {
        let result = validate_repair_command("cass index --full");
        ensure(result.passed, true, "cass command passes")
    }

    #[test]
    fn validate_repair_accepts_chmod_command() -> TestResult {
        let result = validate_repair_command("chmod 600 /path/to/file");
        ensure(result.passed, true, "chmod command passes")
    }

    #[test]
    fn validate_repair_rejects_placeholder() -> TestResult {
        let result = validate_repair_command("ee TODO fix this");
        ensure(result.passed, false, "TODO placeholder fails")
    }

    #[test]
    fn validate_repair_rejects_unknown_prefix() -> TestResult {
        let result = validate_repair_command("unknown command");
        ensure(result.passed, false, "unknown prefix fails")
    }

    #[test]
    fn all_registered_codes_are_honest() {
        let report = validate_all_codes();
        assert!(
            report.passed,
            "All codes should pass honesty checks. Failures: {:?}",
            report
                .checks
                .iter()
                .filter(|c| !c.passed)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn honesty_report_counts_issues() {
        let checks = vec![
            HonestyCheckResult::pass("check1"),
            HonestyCheckResult::fail("check2", "issue"),
            HonestyCheckResult::pass("check3"),
        ];
        let report = HonestyReport::from_checks(checks);

        assert!(!report.passed);
        assert_eq!(report.issue_count, 1);
    }

    #[test]
    fn honesty_report_passes_with_no_issues() {
        let checks = vec![
            HonestyCheckResult::pass("check1"),
            HonestyCheckResult::pass("check2"),
        ];
        let report = HonestyReport::from_checks(checks);

        assert!(report.passed);
        assert_eq!(report.issue_count, 0);
    }

    #[test]
    fn validate_degraded_response_checks_messages() {
        let degraded = vec![
            (
                "D001".to_string(),
                DegradationSeverity::Warning,
                "Semantic search unavailable".to_string(),
                Some("ee index rebuild".to_string()),
            ),
            (
                "D002".to_string(),
                DegradationSeverity::Warning,
                "".to_string(),
                None,
            ),
        ];

        let report = validate_degraded_response(&degraded);
        assert!(!report.passed);
        assert!(report.issue_count >= 1);
    }

    #[test]
    fn severity_critical_with_auto_recover_fails() {
        use crate::models::degradation::{DegradationCode, DegradedSubsystem};

        let code = DegradationCode {
            id: "TEST",
            subsystem: DegradedSubsystem::Search,
            severity: DegradationSeverity::Critical,
            description: "Test critical issue",
            behavior_change: "Test behavior",
            auto_recoverable: true,
            repair: None,
        };

        let result = validate_severity_honesty(&code);
        assert!(!result.passed);
    }

    #[test]
    fn message_quality_rejects_empty_description() {
        use crate::models::degradation::{DegradationCode, DegradedSubsystem};

        let code = DegradationCode {
            id: "TEST",
            subsystem: DegradedSubsystem::Search,
            severity: DegradationSeverity::Advisory,
            description: "",
            behavior_change: "Test behavior",
            auto_recoverable: true,
            repair: None,
        };

        let result = validate_message_quality(&code);
        assert!(!result.passed);
    }

    #[test]
    fn message_quality_rejects_empty_behavior_change() {
        use crate::models::degradation::{DegradationCode, DegradedSubsystem};

        let code = DegradationCode {
            id: "TEST",
            subsystem: DegradedSubsystem::Search,
            severity: DegradationSeverity::Advisory,
            description: "Test description here",
            behavior_change: "",
            auto_recoverable: true,
            repair: None,
        };

        let result = validate_message_quality(&code);
        assert!(!result.passed);
    }

    #[test]
    fn fake_success_output_rejects_stub_marker() {
        let report = validate_no_fake_success_output(
            "preflight show",
            true,
            false,
            r#"{"schema":"ee.response.v1","success":true,"data":{"status":"stubbed"}}"#,
        );

        assert!(!report.passed);
        assert_eq!(report.issue_count, 1);
    }

    #[test]
    fn fake_success_output_allows_fixture_mode() {
        let report = validate_no_fake_success_output(
            "eval run",
            true,
            true,
            r#"{"schema":"ee.response.v1","success":true,"data":{"fixtureId":"fixture_release"}}"#,
        );

        assert!(report.passed);
    }

    #[test]
    fn fake_success_output_ignores_degraded_failure() {
        let report = validate_no_fake_success_output(
            "context",
            false,
            false,
            r#"{"schema":"ee.error.v1","error":{"message":"stub store unavailable"}}"#,
        );

        assert!(report.passed);
    }
}
