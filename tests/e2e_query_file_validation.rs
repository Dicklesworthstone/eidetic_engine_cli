//! EE-xufd: query-file validation errors produce JSON error envelope
//!
//! Validates that `ee pack --query-file` accepts supported tag/temporal filters
//! and rejects unsupported fields with a proper ee.error.v1 envelope.
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

fn assert_response_envelope(json: &serde_json::Value, context: &str) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure_equal(&schema, &"ee.response.v1", &format!("{context} schema"))?;

    let success = json
        .get("success")
        .and_then(|s| s.as_bool())
        .ok_or_else(|| format!("{context}: missing success field"))?;
    ensure(success, format!("{context}: expected success=true"))
}

fn assert_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

fn degraded_codes(json: &serde_json::Value) -> Vec<&str> {
    json["data"]["degraded"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["code"].as_str())
        .collect()
}

#[test]
fn query_file_with_valid_tags_object_succeeds() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    for (tags, content) in [
        ("important", "test query important result"),
        ("important,draft", "test query draft result"),
    ] {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            tags,
            content,
        ])?;
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            "remember exit code",
        )?;
        assert_stderr_empty(&remember, "remember")?;
    }

    // Create query file with valid tags object
    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{
        "version": "ee.query.v1",
        "query": {"text": "test query"},
        "tags": {
            "require": ["important"],
            "exclude": ["draft"]
        }
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

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "valid tags object should succeed",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "valid tags")?;
    let items = json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "valid tags: missing pack items array".to_string())?;
    ensure(!items.is_empty(), "valid tags: expected selected memory")?;
    ensure(
        items.iter().all(|item| {
            item["content"].as_str().is_some_and(|content| {
                content.contains("important result") && !content.contains("draft result")
            })
        }),
        "valid tags: selected memories must satisfy require and exclude filters",
    )?;
    assert_stderr_empty(&output, "valid tags")
}

#[test]
fn query_file_with_invalid_tags_array_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    // Create query file with invalid tags format (array instead of object)
    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{
        "version": "ee.query.v1",
        "query": {"text": "test query"},
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
        "invalid tags array should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_MALFORMED_JSON", "invalid tags format")?;
    assert_stderr_empty(&output, "invalid tags format")
}

#[test]
fn query_file_with_future_time_window_excludes_current_memories() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "test query current temporal result",
    ])?;
    ensure_equal(
        &remember.status.code(),
        &Some(EXIT_SUCCESS),
        "remember exit code",
    )?;
    assert_stderr_empty(&remember, "remember")?;

    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{
        "version": "ee.query.v1",
        "query": {"text": "test query"},
        "time": {"after": "2099-01-01T00:00:00Z"}
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

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "future time window should succeed",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "future time window")?;
    let items = json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "future time window: missing pack items array".to_string())?;
    ensure(
        items.is_empty(),
        "future time window: current memory should be excluded",
    )?;
    ensure(
        degraded_codes(&json).contains(&"context_temporal_filtered_results"),
        "future time window: temporal filter degradation should be reported",
    )?;
    assert_stderr_empty(&output, "future time window")
}

#[test]
fn query_file_with_strict_temporal_validity_filters_future_and_expired_memories() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    for args in [
        vec![
            "--workspace",
            &workspace,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--valid-from",
            "2026-04-01T00:00:00Z",
            "--valid-to",
            "2026-05-01T00:00:00Z",
            "test query current validity result",
        ],
        vec![
            "--workspace",
            &workspace,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--valid-from",
            "2099-01-01T00:00:00Z",
            "test query future validity result",
        ],
        vec![
            "--workspace",
            &workspace,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--valid-to",
            "2026-04-30T23:59:59Z",
            "test query expired validity result",
        ],
    ] {
        let remember = run_ee(&args)?;
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            "remember exit code",
        )?;
        assert_stderr_empty(&remember, "remember")?;
    }

    let query_file = tempdir.path().join("query.json");
    let query_content = r#"{
        "version": "ee.query.v1",
        "query": {"text": "test query"},
        "temporalValidity": {
            "posture": "strict",
            "referenceTime": "2026-05-01T00:00:00Z"
        }
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

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "strict temporal validity should succeed",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "strict temporal validity")?;
    let items = json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "strict temporal validity: missing pack items array".to_string())?;
    ensure(
        items.iter().any(|item| {
            item["content"]
                .as_str()
                .is_some_and(|content| content.contains("current validity result"))
        }),
        "strict temporal validity: current boundary memory should be selected",
    )?;
    ensure(
        items.iter().all(|item| {
            item["content"].as_str().is_some_and(|content| {
                !content.contains("future validity result")
                    && !content.contains("expired validity result")
            })
        }),
        "strict temporal validity: future and expired memories should be excluded",
    )?;
    ensure(
        degraded_codes(&json).contains(&"context_temporal_filtered_results"),
        "strict temporal validity: temporal filter degradation should be reported",
    )?;
    assert_stderr_empty(&output, "strict temporal validity")
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
