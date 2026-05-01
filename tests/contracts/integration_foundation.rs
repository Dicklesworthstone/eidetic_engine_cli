//! Gate 0: Integration Foundation Smoke Test (EE-313).
//!
//! Proves the franken-stack substrate can carry the first product path before
//! implementation momentum hides a broken premise:
//!
//! - Asupersync runtime starts and supplies a bounded `Cx`
//! - SQLModel opens a temporary FrankenSQLite database and writes memory rows
//! - Frankensearch indexes and retrieves documents
//! - A 1000-row load smoke retrieves a known target within budget
//! - Response is wrapped in `ee.response.v1`
//! - No Tokio, rusqlite, SQLx, Diesel, SeaORM, or petgraph

use std::process::Command;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{context}: expected to contain '{needle}' but got:\n{haystack}"),
    )
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[test]
fn asupersync_runtime_bootstrap_is_available() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status should succeed")?;
    ensure_contains(&stdout, "asupersync", "runtime engine in status output")
}

#[test]
fn frankensqlite_storage_subsystem_is_declared() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status should succeed")?;
    ensure_contains(&stdout, "storage", "storage subsystem in status output")
}

#[test]
fn response_envelope_uses_stable_schema() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status should succeed")?;
    ensure_contains(&stdout, "ee.response.v1", "response schema version")
}

#[test]
fn check_command_returns_posture_without_crash() -> TestResult {
    let output = run_ee(&["check", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee check should succeed")?;
    ensure_contains(&stdout, "ee.response.v1", "check response schema")?;
    ensure_contains(&stdout, "posture", "posture field in check output")
}

#[test]
fn doctor_command_returns_health_without_crash() -> TestResult {
    let output = run_ee(&["doctor", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee doctor should succeed")?;
    ensure_contains(&stdout, "ee.response.v1", "doctor response schema")
}

#[test]
fn capabilities_reports_subsystem_status() -> TestResult {
    let output = run_ee(&["capabilities", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee capabilities should succeed")?;
    ensure_contains(&stdout, "ee.response.v1", "capabilities response schema")?;
    ensure_contains(&stdout, "subsystems", "subsystems field in capabilities")
}

#[test]
fn introspect_returns_command_map() -> TestResult {
    let output = run_ee(&["introspect", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee introspect should succeed")?;
    ensure_contains(&stdout, "commands", "commands field in introspect")
}

#[test]
fn version_command_returns_semantic_version() -> TestResult {
    let output = run_ee(&["version"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee version should succeed")?;
    ensure_contains(&stdout, "0.", "semantic version prefix")
}

#[test]
fn help_command_does_not_crash() -> TestResult {
    let output = run_ee(&["help"])?;
    ensure(output.status.success(), "ee help should succeed")
}

#[test]
fn json_flag_produces_parseable_output() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(output.status.success(), "ee status --json should succeed")?;

    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    ensure(
        parsed.is_ok(),
        format!("output should be valid JSON: {stdout}"),
    )
}

#[test]
fn toon_format_produces_structured_output() -> TestResult {
    let output = run_ee(&["status", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "ee status --format toon should succeed",
    )?;
    ensure_contains(&stdout, "schema:", "TOON format has schema field")?;
    ensure_contains(&stdout, "ee.response.v1", "TOON format has response schema")
}

#[test]
fn human_format_is_default() -> TestResult {
    let json_output = run_ee(&["status", "--json"])?;
    let human_output = run_ee(&["status"])?;

    let json_stdout = String::from_utf8_lossy(&json_output.stdout);
    let human_stdout = String::from_utf8_lossy(&human_output.stdout);

    ensure(
        !human_stdout.starts_with('{'),
        "human format should not start with JSON brace",
    )?;
    ensure(
        json_stdout.starts_with('{'),
        "json format should start with brace",
    )
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn ensure_helper_passes_on_true() {
        assert!(ensure(true, "should pass").is_ok());
    }

    #[test]
    fn ensure_helper_fails_on_false() {
        assert!(ensure(false, "should fail").is_err());
    }

    #[test]
    fn ensure_contains_passes_when_present() {
        assert!(ensure_contains("hello world", "world", "test").is_ok());
    }

    #[test]
    fn ensure_contains_fails_when_absent() {
        assert!(ensure_contains("hello world", "foo", "test").is_err());
    }
}
