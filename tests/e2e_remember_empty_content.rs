//! EE-90xb: remember rejects empty/whitespace content with error envelope
//!
//! Validates that `ee remember ''` and `ee remember '   '` produce proper
//! ee.error.v2 envelope with validation error rather than storing empty memory.
//!
//! NO MOCKS. Real ee binary, temp workspace.

use std::fmt::Debug;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_USAGE: i32 = 1;

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
    ensure_equal(&schema, &"ee.error.v2", &format!("{context} schema"))?;

    let error = json
        .get("error")
        .ok_or_else(|| format!("{context}: missing error field"))?;
    let code = error
        .get("code")
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("{context}: missing error.code"))?;
    ensure(
        code == "usage" || code == "validation",
        format!("{context}: error.code should be usage or validation, got {code}"),
    )?;

    ensure(
        json.get("memory_id").is_none(),
        format!("{context}: should not have memory_id on error"),
    )
}

#[test]
fn remember_empty_string_returns_error_envelope() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let output = run_ee(&["--workspace", &workspace, "remember", "", "--json"])?;

    ensure(
        output.status.code() != Some(0),
        "empty content should produce non-zero exit code",
    )?;
    ensure_equal(
        &output.status.code(),
        &Some(EXIT_USAGE),
        "empty content exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "empty string")?;

    let message = json
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    ensure(
        message.contains("empty") || message.contains("blank") || message.contains("content"),
        format!("error message should mention empty content: {message}"),
    )
}

#[test]
fn remember_whitespace_only_returns_error_envelope() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let output = run_ee(&["--workspace", &workspace, "remember", "   ", "--json"])?;

    ensure(
        output.status.code() != Some(0),
        "whitespace-only content should produce non-zero exit code",
    )?;
    ensure_equal(
        &output.status.code(),
        &Some(EXIT_USAGE),
        "whitespace-only content exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "whitespace only")?;

    let message = json
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    ensure(
        message.contains("empty") || message.contains("trim") || message.contains("content"),
        format!("error message should mention empty/trim: {message}"),
    )
}

#[test]
fn remember_tabs_and_newlines_only_returns_error_envelope() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "\t\n  \n\t",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(0),
        "tabs/newlines-only content should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "tabs and newlines only")
}
