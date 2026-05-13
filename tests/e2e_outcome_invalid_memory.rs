//! EE-jp7a: outcome command with invalid memory ID returns error envelope
//!
//! Validates that `ee outcome <nonexistent-id> --signal helpful` produces proper
//! ee.error.v2 envelope with NOT_FOUND error code rather than silent success.
//!
//! NO MOCKS. Real ee binary, temp workspace.

use std::fmt::Debug;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn assert_error_envelope(json: &serde_json::Value, context: &str) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure_equal(&schema, &"ee.error.v2", &format!("{context} schema"))
}

fn assert_not_found_error(json: &serde_json::Value, context: &str) -> TestResult {
    let error = json
        .get("error")
        .ok_or_else(|| format!("{context}: missing error field"))?;

    let code = error
        .get("code")
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("{context}: missing error.code"))?;
    ensure(
        code.contains("not_found") || code == "not_found",
        format!("{context}: error.code should contain 'not_found', got {code}"),
    )?;

    let repair = error.get("repair").and_then(|r| r.as_str());
    ensure(
        repair.is_some(),
        format!("{context}: error.repair should be present"),
    )
}

#[test]
fn outcome_nonexistent_memory_id_returns_not_found_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // First init the workspace so the database exists
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Try to record outcome for a nonexistent memory ID
    let output = run_ee(&[
        "--workspace",
        &workspace,
        "outcome",
        "mem_nonexistent_00000000000000",
        "--signal",
        "helpful",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "outcome with nonexistent memory should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "nonexistent memory")?;
    assert_not_found_error(&json, "nonexistent memory")?;

    let message = json
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    ensure(
        message.contains("memory") || message.contains("Memory"),
        format!("error message should mention memory: {message}"),
    )
}

#[test]
fn outcome_malformed_memory_id_returns_not_found_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // First init the workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Try malformed ID (too short, wrong format)
    let output = run_ee(&[
        "--workspace",
        &workspace,
        "outcome",
        "not-a-valid-memory-id",
        "--signal",
        "helpful",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "outcome with malformed memory ID should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "malformed memory ID")
}

#[test]
fn outcome_empty_memory_id_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // First init the workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Try empty target ID
    let output = run_ee(&[
        "--workspace",
        &workspace,
        "outcome",
        "",
        "--signal",
        "helpful",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "outcome with empty memory ID should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "empty memory ID")
}
