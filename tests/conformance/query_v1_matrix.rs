//! ee.query.v1 conformance matrix
//!
//! Comprehensive coverage of all query-file filter combinations, error paths,
//! and feature interactions documented in docs/query-schema.md.
//!
//! NO MOCKS. Real ee binary, temp workspace. Deterministic across runs.

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

fn setup_workspace_with_memories() -> Result<(tempfile::TempDir, String), String> {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;

    let memories = [
        (
            "release,important",
            "procedural",
            "rule",
            "always run tests before release",
        ),
        (
            "release,draft",
            "procedural",
            "rule",
            "draft release notes template",
        ),
        (
            "security,important",
            "procedural",
            "warning",
            "review access logs before release",
        ),
        (
            "debug",
            "episodic",
            "observation",
            "debug session observation",
        ),
    ];

    for (tags, level, kind, content) in memories {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "--json",
            "remember",
            "--level",
            level,
            "--kind",
            kind,
            "--tags",
            tags,
            content,
        ])?;
        ensure_equal(
            &remember.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("remember '{content}' exit code"),
        )?;
    }

    Ok((tempdir, workspace))
}

fn write_query_file(tempdir: &tempfile::TempDir, content: &str) -> Result<String, String> {
    let query_file = tempdir.path().join("query.json");
    fs::write(&query_file, content).map_err(|e| e.to_string())?;
    Ok(query_file.to_string_lossy().to_string())
}

// ============================================================================
// SECTION 1: Implemented Features (Should Succeed)
// ============================================================================

#[test]
fn matrix_simple_text_query() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release tests"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "simple query")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "simple query")?;
    assert_stderr_empty(&output, "simple query")
}

#[test]
fn matrix_tags_require_only() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {"require": ["release", "important"]}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "tags require")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "tags require")?;

    let items = json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "tags require: missing pack items".to_string())?;
    ensure(
        items.iter().all(|item| {
            item["content"]
                .as_str()
                .is_some_and(|c| c.contains("tests before release"))
        }),
        "tags require: all items must have both 'release' AND 'important' tags",
    )?;
    assert_stderr_empty(&output, "tags require")
}

#[test]
fn matrix_tags_require_any() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "important"},
            "tags": {"requireAny": ["release", "security"]}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "tags requireAny",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "tags requireAny")?;
    assert_stderr_empty(&output, "tags requireAny")
}

#[test]
fn matrix_tags_exclude() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {"exclude": ["draft"]}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "tags exclude")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "tags exclude")?;

    let items = json["data"]["pack"]["items"].as_array();
    if let Some(items) = items {
        ensure(
            items.iter().all(|item| {
                !item["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("draft"))
            }),
            "tags exclude: no items should contain 'draft' content",
        )?;
    }
    assert_stderr_empty(&output, "tags exclude")
}

#[test]
fn matrix_tags_combined_filters() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {
                "require": ["important"],
                "requireAny": ["release", "security"],
                "exclude": ["draft", "debug"]
            }
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "tags combined")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "tags combined")?;
    assert_stderr_empty(&output, "tags combined")
}

#[test]
fn matrix_output_profile_balanced() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "output": {"profile": "balanced"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "output profile")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "output profile")?;
    assert_stderr_empty(&output, "output profile")
}

#[test]
fn matrix_output_explain_true() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "output": {"explain": true}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "output explain")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "output explain")?;
    assert_stderr_empty(&output, "output explain")
}

#[test]
fn matrix_budget_max_tokens() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "budget": {"maxTokens": 2000}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "budget maxTokens",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "budget maxTokens")?;
    assert_stderr_empty(&output, "budget maxTokens")
}

#[test]
fn matrix_budget_max_results() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "budget": {"maxResults": 2}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "budget maxResults",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "budget maxResults")?;
    assert_stderr_empty(&output, "budget maxResults")
}

#[test]
fn matrix_query_mode_hybrid() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release", "mode": "hybrid"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "query mode hybrid",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "query mode hybrid")?;
    assert_stderr_empty(&output, "query mode hybrid")
}

// ============================================================================
// SECTION 2: Temporal Features (Should Succeed)
// ============================================================================

#[test]
fn matrix_time_after_filters_created_at() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "time": {"after": "2099-01-01T00:00:00Z"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "time.after")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "time.after")?;
    let items = json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "time.after: missing pack items".to_string())?;
    ensure(
        items.is_empty(),
        "time.after: future lower bound should exclude current memories",
    )?;
    ensure(
        degraded_codes(&json).contains(&"context_temporal_filtered_results"),
        "time.after: temporal filter should be explainable",
    )?;
    assert_stderr_empty(&output, "time.after")
}

#[test]
fn matrix_time_before_accepts_open_ended_future_window() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "time": {"before": "2026-12-31T23:59:59Z"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "time.before")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "time.before")?;
    assert_stderr_empty(&output, "time.before")
}

#[test]
fn matrix_as_of_future_snapshot_succeeds() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "asOf": "2099-04-15T12:00:00Z"
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "asOf")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "asOf")?;
    assert_stderr_empty(&output, "asOf")
}

#[test]
fn matrix_temporal_validity_strict_succeeds() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "temporalValidity": {
                "posture": "strict",
                "referenceTime": "2099-01-01T00:00:00Z"
            }
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "temporalValidity",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "temporalValidity")?;
    assert_stderr_empty(&output, "temporalValidity")
}

// ============================================================================
// SECTION 3: Trust and Redaction Features (Should Succeed)
// ============================================================================

#[test]
fn matrix_trust_min_class_succeeds() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "trust": {"minClass": "human_explicit"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "trust")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "trust")?;
    assert_stderr_empty(&output, "trust")
}

#[test]
fn matrix_redaction_respect_succeeds() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "redaction": {"policy": "respect"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "redaction")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "redaction")?;
    assert_stderr_empty(&output, "redaction")
}

// ============================================================================
// SECTION 4: Unimplemented Features (ERR_UNSUPPORTED_FEATURE)
// ============================================================================

#[test]
fn matrix_unsupported_graph() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "graph": {"seedMemories": ["mem_abc"], "maxHops": 2}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "graph should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_UNSUPPORTED_FEATURE", "graph")?;
    assert_stderr_empty(&output, "graph")
}

#[test]
fn matrix_unsupported_pagination() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "pagination": {"limit": 10}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "pagination should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_UNSUPPORTED_FEATURE", "pagination")?;
    assert_stderr_empty(&output, "pagination")
}

// ============================================================================
// SECTION 3: Error Cases
// ============================================================================

#[test]
fn matrix_error_malformed_json() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(&tempdir, r#"{ "query": "test", invalid json }"#)?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "malformed JSON should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_MALFORMED_JSON", "malformed JSON")?;
    assert_stderr_empty(&output, "malformed JSON")
}

#[test]
fn matrix_error_unknown_version() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v99",
            "query": {"text": "test"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "unknown version should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_UNKNOWN_VERSION", "unknown version")?;
    assert_stderr_empty(&output, "unknown version")
}

#[test]
fn matrix_error_empty_query_text() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": ""}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "empty query text should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_EMPTY_QUERY", "empty query")?;
    assert_stderr_empty(&output, "empty query")
}

#[test]
fn matrix_error_whitespace_query_text() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "   "}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "whitespace query text should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_EMPTY_QUERY", "whitespace query")?;
    assert_stderr_empty(&output, "whitespace query")
}

#[test]
fn matrix_error_invalid_timestamp_format() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "test"},
            "time": {"after": "not-a-timestamp"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "invalid timestamp should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_INVALID_TIMESTAMP", "invalid timestamp")?;
    assert_stderr_empty(&output, "invalid timestamp")
}

#[test]
fn matrix_error_zero_budget_max_tokens() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "test"},
            "budget": {"maxTokens": 0}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "zero maxTokens should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_ZERO_BUDGET", "zero budget")?;
    assert_stderr_empty(&output, "zero budget")
}

#[test]
fn matrix_error_query_file_not_found() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let nonexistent = tempdir.path().join("nonexistent.json");

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &nonexistent.to_string_lossy(),
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "missing file should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_QUERY_FILE_NOT_FOUND", "file not found")?;
    assert_stderr_empty(&output, "file not found")
}

#[test]
fn matrix_error_tags_wrong_type_array() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "test"},
            "tags": ["important"]
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "tags as array should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_MALFORMED_JSON", "tags wrong type")?;
    assert_stderr_empty(&output, "tags wrong type")
}

#[test]
fn matrix_error_tags_wrong_type_string() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "test"},
            "tags": "important"
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "tags as string should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_MALFORMED_JSON", "tags as string")?;
    assert_stderr_empty(&output, "tags as string")
}

// ============================================================================
// SECTION 4: Combination Tests (Multiple Features Together)
// ============================================================================

#[test]
fn matrix_combo_tags_and_budget() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {"require": ["important"], "exclude": ["draft"]},
            "budget": {"maxTokens": 4000, "maxResults": 10}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "tags + budget")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "tags + budget")?;
    assert_stderr_empty(&output, "tags + budget")
}

#[test]
fn matrix_combo_tags_and_output() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {"require": ["important"]},
            "output": {"profile": "balanced", "explain": true}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "tags + output")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "tags + output")?;
    assert_stderr_empty(&output, "tags + output")
}

#[test]
fn matrix_combo_all_implemented_features() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release", "mode": "hybrid"},
            "tags": {
                "require": ["important"],
                "requireAny": ["release", "security"],
                "exclude": ["draft"]
            },
            "output": {
                "profile": "balanced",
                "format": "json",
                "fields": "standard",
                "explain": true
            },
            "budget": {
                "maxTokens": 4000,
                "maxResults": 25,
                "candidatePool": 100
            }
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "all implemented features",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "all implemented features")?;
    assert_stderr_empty(&output, "all implemented features")
}

// ============================================================================
// SECTION 5: Determinism Tests
// ============================================================================

#[test]
fn matrix_deterministic_output() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {"require": ["important"]}
        }"#,
    )?;

    let output1 = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    let output2 = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output1.status.code(),
        &Some(EXIT_SUCCESS),
        "run 1 exit code",
    )?;
    ensure_equal(
        &output2.status.code(),
        &Some(EXIT_SUCCESS),
        "run 2 exit code",
    )?;

    let json1 = stdout_json(&output1)?;
    let json2 = stdout_json(&output2)?;

    let items1 = json1["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "run 1 missing pack items".to_string())?;
    let items2 = json2["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "run 2 missing pack items".to_string())?;

    let ids1: Vec<_> = items1.iter().filter_map(|i| i["id"].as_str()).collect();
    let ids2: Vec<_> = items2.iter().filter_map(|i| i["id"].as_str()).collect();

    ensure_equal(&ids1, &ids2, "item IDs should be deterministic across runs")
}

// ============================================================================
// SECTION 6: Edge Cases
// ============================================================================

#[test]
fn matrix_edge_empty_tags_object() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "empty tags object",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "empty tags object")?;
    assert_stderr_empty(&output, "empty tags object")
}

#[test]
fn matrix_edge_empty_require_array() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "tags": {"require": []}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "empty require array",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "empty require array")?;
    assert_stderr_empty(&output, "empty require array")
}

#[test]
fn matrix_edge_large_budget() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "budget": {"maxTokens": 1000000, "maxResults": 10000}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "large budget")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "large budget")?;
    assert_stderr_empty(&output, "large budget")
}

#[test]
fn matrix_edge_unicode_query_text() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release 发布 リリース 🚀"}
        }"#,
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "unicode query")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "unicode query")?;
    assert_stderr_empty(&output, "unicode query")
}

#[test]
fn matrix_edge_unicode_tags_are_rejected_by_remember_validation() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init_output.status.code(), &Some(EXIT_SUCCESS), "init")?;

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
        "发布,重要",
        "release with unicode tags",
    ])?;
    ensure(
        remember.status.code() != Some(EXIT_SUCCESS),
        "unicode tags should be rejected by remember validation",
    )?;
    let json = stdout_json(&remember)?;
    assert_error_envelope(&json, "usage", "unicode tags")?;
    assert_stderr_empty(&remember, "unicode tags")
}
