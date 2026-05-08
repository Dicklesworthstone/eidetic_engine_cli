//! EE-xufd: query-file validation errors produce JSON error envelope
//!
//! Validates that `ee pack --query-file` with unsupported fields (tags, time,
//! graph, etc.) produces proper ee.error.v1 envelope with ERR_UNSUPPORTED_FEATURE.
//!
//! NO MOCKS. Real ee binary, temp workspace.

use std::fmt::Debug;
use std::fs;
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

fn assert_error_envelope(
    json: &serde_json::Value,
    expected_code: &str,
    context: &str,
) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure_equal(&schema, &"ee.error.v1", &format!("{context} schema"))?;

    let error = json
        .get("error")
        .ok_or_else(|| format!("{context}: missing error field"))?;

    let code = error
        .get("code")
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("{context}: missing error.code"))?;
    ensure_equal(&code, &expected_code, &format!("{context} error.code"))
}

fn assert_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

#[test]
fn query_file_with_unsupported_tags_field_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Create query file with unsupported 'tags' field
    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{
        "version": "ee.query.v1",
        "query": "test query",
        "tags": ["important"]
    }"#;
    fs::write(&query_file, query_content).map_err(|e| e.to_string())?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file.to_string_lossy(),
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "unsupported tags field should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_UNSUPPORTED_FEATURE", "tags field")?;
    assert_stderr_empty(&output, "tags field")
}

#[test]
fn query_file_with_unsupported_time_field_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Create query file with unsupported 'time' field (using valid RFC3339 timestamps)
    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{
        "version": "ee.query.v1",
        "query": "test query",
        "time": {"after": "2024-01-01T00:00:00Z"}
    }"#;
    fs::write(&query_file, query_content).map_err(|e| e.to_string())?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file.to_string_lossy(),
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "unsupported time field should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_UNSUPPORTED_FEATURE", "time field")?;
    assert_stderr_empty(&output, "time field")
}

#[test]
fn query_file_with_unsupported_graph_field_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Create query file with unsupported 'graph' field
    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{
        "version": "ee.query.v1",
        "query": "test query",
        "graph": {"depth": 2}
    }"#;
    fs::write(&query_file, query_content).map_err(|e| e.to_string())?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file.to_string_lossy(),
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "unsupported graph field should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_UNSUPPORTED_FEATURE", "graph field")?;
    assert_stderr_empty(&output, "graph field")
}

#[test]
fn query_file_with_malformed_json_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Create malformed query file
    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{ "query": "test", invalid json }"#;
    fs::write(&query_file, query_content).map_err(|e| e.to_string())?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file.to_string_lossy(),
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "malformed JSON should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_MALFORMED_JSON", "malformed JSON")?;
    assert_stderr_empty(&output, "malformed JSON")
}

#[test]
fn query_file_missing_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        "/nonexistent/query.json",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "missing file should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_QUERY_FILE_NOT_FOUND", "missing file")?;
    assert_stderr_empty(&output, "missing file")
}
