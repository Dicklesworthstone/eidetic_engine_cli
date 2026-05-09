//! ee.query.v1 conformance matrix
//!
//! Comprehensive coverage of all query-file filter combinations, error paths,
//! and feature interactions documented in docs/query-schema.md.
//!
//! NO MOCKS. Real ee binary, temp workspace. Deterministic across runs.

use std::fmt::Debug;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
    MemoryLinkRelation, MemoryLinkSource,
};

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

fn run_ee_pack_query_file(workspace: &str, query_file: &str) -> Result<Output, String> {
    let build_output = run_ee(&[
        "--workspace",
        workspace,
        "pack",
        "build",
        "--query-file",
        query_file,
        "--json",
    ])?;
    if build_output.status.code() == Some(EXIT_SUCCESS) {
        return Ok(build_output);
    }

    run_ee(&[
        "--workspace",
        workspace,
        "pack",
        "--query-file",
        query_file,
        "--json",
    ])
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

fn remember_graph_memory(workspace: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        &format!("remember graph memory '{content}'"),
    )?;
    assert_stderr_empty(&output, "remember graph memory")?;
    let json = stdout_json(&output)?;
    json["data"]["public_id"]
        .as_str()
        .or_else(|| json["data"]["memory_id"].as_str())
        .or_else(|| json["data"]["id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("remember response missing memory id: {json}"))
}

fn setup_workspace_with_graph()
-> Result<(tempfile::TempDir, String, String, String, String, String), String> {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "graph init exit code",
    )?;
    assert_stderr_empty(&init_output, "graph init")?;

    let seed = remember_graph_memory(&workspace, "graph traversal anchor release root")?;
    let orphan = remember_graph_memory(&workspace, "graph traversal orphan memory unrelated")?;

    let database_path = Path::new(&workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let workspace_row = connection
        .get_workspace_by_path(&workspace)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "graph workspace row missing".to_string())?;
    let outbound = "mem_00000000000000000000000201".to_string();
    let inbound = "mem_00000000000000000000000202".to_string();
    connection
        .insert_memory(
            &outbound,
            &CreateMemoryInput {
                workspace_id: workspace_row.id.clone(),
                level: "semantic".to_string(),
                kind: "fact".to_string(),
                content: "outbound expansion only".to_string(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("file://docs/query-schema.md#L221".to_string()),
                trust_class: "agent_validated".to_string(),
                trust_subclass: Some("query-v1-matrix".to_string()),
                valid_from: None,
                valid_to: None,
                tags: vec!["graph".to_string()],
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            &inbound,
            &CreateMemoryInput {
                workspace_id: workspace_row.id,
                level: "semantic".to_string(),
                kind: "fact".to_string(),
                content: "inbound expansion only".to_string(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("file://docs/query-schema.md#L221".to_string()),
                trust_class: "agent_validated".to_string(),
                trust_subclass: Some("query-v1-matrix".to_string()),
                valid_from: None,
                valid_to: None,
                tags: vec!["graph".to_string()],
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000201",
            &CreateMemoryLinkInput {
                src_memory_id: seed.clone(),
                dst_memory_id: outbound.clone(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.91,
                confidence: 0.88,
                directed: true,
                evidence_count: 2,
                last_reinforced_at: Some("2026-05-08T00:00:00Z".to_string()),
                source: MemoryLinkSource::Agent,
                created_by: Some("query-v1-matrix".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000202",
            &CreateMemoryLinkInput {
                src_memory_id: inbound.clone(),
                dst_memory_id: seed.clone(),
                relation: MemoryLinkRelation::Related,
                weight: 0.82,
                confidence: 0.79,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-08T00:00:00Z".to_string()),
                source: MemoryLinkSource::Agent,
                created_by: Some("query-v1-matrix".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    Ok((tempdir, workspace, seed, outbound, inbound, orphan))
}

fn pack_item_contents(json: &serde_json::Value, context: &str) -> Result<Vec<String>, String> {
    let items = json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| format!("{context}: missing pack items"))?;
    Ok(items
        .iter()
        .filter_map(|item| item["content"].as_str().map(str::to_string))
        .collect())
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

#[test]
fn matrix_redaction_allow_categories_filters_secret_reasons() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "redaction allow init",
    )?;
    assert_stderr_empty(&init_output, "redaction allow init")?;

    let database_path = Path::new(&workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let workspace_row = connection
        .get_workspace_by_path(&workspace)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "redaction allow workspace row missing".to_string())?;
    let memory_input = |content: &str| CreateMemoryInput {
        workspace_id: workspace_row.id.clone(),
        level: "semantic".to_string(),
        kind: "fact".to_string(),
        content: content.to_string(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.8,
        importance: 0.7,
        provenance_uri: Some("file://docs/query-schema.md#L190".to_string()),
        trust_class: "agent_validated".to_string(),
        trust_subclass: Some("query-v1-matrix".to_string()),
        valid_from: None,
        valid_to: None,
        tags: vec!["redaction".to_string()],
    };
    let blocked_secret = "sk-ant-api03-redactionfuzztokenredactionfuzztokenredactionfuzztoken";
    let blocked_content = format!("redaction allowcategory blocked candidate {blocked_secret}");
    connection
        .insert_memory(
            "mem_00000000000000000000000401",
            &memory_input("redaction allowcategory safe candidate"),
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            "mem_00000000000000000000000402",
            &memory_input(&blocked_content),
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(
        &rebuild.status.code(),
        &Some(EXIT_SUCCESS),
        "redaction allow index rebuild",
    )?;
    assert_stderr_empty(&rebuild, "redaction allow index rebuild")?;

    let excluded_query = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "redaction allowcategory"},
            "redaction": {"allowCategories": ["email_address"]}
        }"#,
    )?;
    let excluded_output = run_ee_pack_query_file(&workspace, &excluded_query)?;
    ensure_equal(
        &excluded_output.status.code(),
        &Some(EXIT_SUCCESS),
        "redaction allow excluded",
    )?;
    let excluded_json = stdout_json(&excluded_output)?;
    assert_response_envelope(&excluded_json, "redaction allow excluded")?;
    let excluded_contents = pack_item_contents(&excluded_json, "redaction allow excluded")?;
    ensure(
        excluded_contents
            .iter()
            .any(|content| content.contains("safe candidate")),
        "redaction allow excluded: safe candidate should remain",
    )?;
    ensure(
        excluded_contents
            .iter()
            .all(|content| !content.contains("blocked candidate")),
        "redaction allow excluded: secret candidate should be filtered",
    )?;
    ensure(
        degraded_codes(&excluded_json).contains(&"context_redaction_filtered_results"),
        "redaction allow excluded: filtering should be reported",
    )?;
    assert_stderr_empty(&excluded_output, "redaction allow excluded")?;

    let included_query = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "redaction allowcategory"},
            "redaction": {"allowCategories": ["anthropic_api_key"]}
        }"#,
    )?;
    let included_output = run_ee_pack_query_file(&workspace, &included_query)?;
    ensure_equal(
        &included_output.status.code(),
        &Some(EXIT_SUCCESS),
        "redaction allow included",
    )?;
    let included_json = stdout_json(&included_output)?;
    assert_response_envelope(&included_json, "redaction allow included")?;
    let included_contents = pack_item_contents(&included_json, "redaction allow included")?;
    ensure(
        included_contents
            .iter()
            .any(|content| content.contains("blocked candidate")),
        "redaction allow included: allowed secret category should be retained",
    )?;
    ensure(
        included_contents
            .iter()
            .all(|content| !content.contains(blocked_secret)),
        "redaction allow included: raw secret must still be redacted",
    )?;
    assert_stderr_empty(&included_output, "redaction allow included")
}

#[test]
fn matrix_graph_traversal_hints_expand_seed_neighborhood() -> TestResult {
    let (tempdir, workspace, seed, _outbound, _inbound, _orphan) = setup_workspace_with_graph()?;
    let query_file = write_query_file(
        &tempdir,
        &format!(
            r#"{{
                "version": "ee.query.v1",
                "query": {{"text": "graph traversal anchor"}},
                "graph": {{
                    "seedMemories": ["{seed}"],
                    "traversal": "outbound",
                    "maxHops": 1,
                    "linkTypes": ["supports"],
                    "includeOrphans": false
                }}
            }}"#
        ),
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &query_file,
        "--json",
    ])?;

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "graph outbound")?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "graph outbound")?;
    let contents = pack_item_contents(&json, "graph outbound")?;
    ensure(
        contents
            .iter()
            .any(|content| content.contains("graph traversal anchor")),
        "graph outbound: seed memory should be retained",
    )?;
    ensure(
        contents
            .iter()
            .any(|content| content.contains("outbound expansion only")),
        "graph outbound: outbound neighbor should be expanded",
    )?;
    ensure(
        contents
            .iter()
            .all(|content| !content.contains("inbound expansion only")),
        "graph outbound: inbound neighbor should be excluded",
    )?;
    ensure(
        contents
            .iter()
            .all(|content| !content.contains("orphan memory unrelated")),
        "graph outbound: orphan memory should be excluded",
    )?;
    ensure(
        degraded_codes(&json).contains(&"context_graph_snapshot_missing"),
        "graph outbound: missing graph snapshot should be reported",
    )?;
    assert_stderr_empty(&output, "graph outbound")
}

#[test]
fn matrix_graph_traversal_direction_and_orphan_handling() -> TestResult {
    let (tempdir, workspace, seed, _outbound, _inbound, _orphan) = setup_workspace_with_graph()?;
    let inbound_query = write_query_file(
        &tempdir,
        &format!(
            r#"{{
                "version": "ee.query.v1",
                "query": {{"text": "graph traversal anchor"}},
                "graph": {{
                    "seedMemories": ["{seed}"],
                    "traversal": "inbound",
                    "maxHops": 1,
                    "linkTypes": ["related"],
                    "includeOrphans": false
                }}
            }}"#
        ),
    )?;

    let inbound_output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &inbound_query,
        "--json",
    ])?;
    ensure_equal(
        &inbound_output.status.code(),
        &Some(EXIT_SUCCESS),
        "graph inbound",
    )?;
    let inbound_json = stdout_json(&inbound_output)?;
    assert_response_envelope(&inbound_json, "graph inbound")?;
    let inbound_contents = pack_item_contents(&inbound_json, "graph inbound")?;
    ensure(
        inbound_contents
            .iter()
            .any(|content| content.contains("inbound expansion only")),
        "graph inbound: inbound neighbor should be expanded",
    )?;
    ensure(
        inbound_contents
            .iter()
            .all(|content| !content.contains("outbound expansion only")),
        "graph inbound: outbound neighbor should be excluded by traversal",
    )?;
    assert_stderr_empty(&inbound_output, "graph inbound")?;

    let bidirectional_query = write_query_file(
        &tempdir,
        &format!(
            r#"{{
                "version": "ee.query.v1",
                "query": {{"text": "graph traversal anchor"}},
                "graph": {{
                    "seedMemories": ["{seed}"],
                    "traversal": "bidirectional",
                    "maxHops": 1,
                    "linkTypes": ["supports", "related"],
                    "includeOrphans": false
                }}
            }}"#
        ),
    )?;

    let bidirectional_output = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "--query-file",
        &bidirectional_query,
        "--json",
    ])?;
    ensure_equal(
        &bidirectional_output.status.code(),
        &Some(EXIT_SUCCESS),
        "graph bidirectional",
    )?;
    let bidirectional_json = stdout_json(&bidirectional_output)?;
    assert_response_envelope(&bidirectional_json, "graph bidirectional")?;
    let bidirectional_contents = pack_item_contents(&bidirectional_json, "graph bidirectional")?;
    ensure(
        bidirectional_contents
            .iter()
            .any(|content| content.contains("outbound expansion only")),
        "graph bidirectional: outbound neighbor should be expanded",
    )?;
    ensure(
        bidirectional_contents
            .iter()
            .any(|content| content.contains("inbound expansion only")),
        "graph bidirectional: inbound neighbor should be expanded",
    )?;
    ensure(
        bidirectional_contents
            .iter()
            .all(|content| !content.contains("orphan memory unrelated")),
        "graph bidirectional: orphan memory should be excluded",
    )?;
    ensure(
        degraded_codes(&bidirectional_json).contains(&"context_graph_orphans_filtered"),
        "graph bidirectional: orphan filtering should be reported",
    )?;
    assert_stderr_empty(&bidirectional_output, "graph bidirectional")
}

#[test]
fn matrix_graph_hints_do_not_expand_cross_workspace_links() -> TestResult {
    let (tempdir, workspace, seed, _outbound, _inbound, _orphan) = setup_workspace_with_graph()?;

    let database_path = Path::new(&workspace).join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let other_workspace_id = "wsp_11111111111111111111111111";
    let cross_memory = "mem_00000000000000000000000203";
    connection
        .insert_workspace(
            other_workspace_id,
            &CreateWorkspaceInput {
                path: tempdir
                    .path()
                    .join("other-workspace")
                    .to_string_lossy()
                    .to_string(),
                name: Some("other-workspace".to_string()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            cross_memory,
            &CreateMemoryInput {
                workspace_id: other_workspace_id.to_string(),
                level: "semantic".to_string(),
                kind: "fact".to_string(),
                content: "cross workspace neighbor must not leak".to_string(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("file://docs/query-schema.md#L221".to_string()),
                trust_class: "agent_validated".to_string(),
                trust_subclass: Some("query-v1-matrix".to_string()),
                valid_from: None,
                valid_to: None,
                tags: vec!["graph".to_string()],
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000203",
            &CreateMemoryLinkInput {
                src_memory_id: seed.clone(),
                dst_memory_id: cross_memory.to_string(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.93,
                confidence: 0.9,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-08T00:00:00Z".to_string()),
                source: MemoryLinkSource::Agent,
                created_by: Some("query-v1-matrix".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let query_file = write_query_file(
        &tempdir,
        &format!(
            r#"{{
                "version": "ee.query.v1",
                "query": {{"text": "graph traversal anchor"}},
                "graph": {{
                    "seedMemories": ["{seed}"],
                    "traversal": "outbound",
                    "maxHops": 1,
                    "linkTypes": ["supports"],
                    "includeOrphans": false
                }}
            }}"#
        ),
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
        "graph cross-workspace",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "graph cross-workspace")?;
    let contents = pack_item_contents(&json, "graph cross-workspace")?;
    ensure(
        contents
            .iter()
            .any(|content| content.contains("outbound expansion only")),
        "graph cross-workspace: active-workspace neighbor should still be expanded",
    )?;
    ensure(
        contents
            .iter()
            .all(|content| !content.contains("cross workspace neighbor")),
        "graph cross-workspace: cross-workspace neighbor must not be emitted",
    )?;
    ensure(
        degraded_codes(&json).contains(&"context_graph_workspace_filtered"),
        "graph cross-workspace: scope filtering should be reported",
    )?;
    assert_stderr_empty(&output, "graph cross-workspace")
}

// ============================================================================
// SECTION 3.1: Pagination Features (Implemented via eidetic_engine_cli-4x80)
// ============================================================================

#[test]
fn matrix_pagination_limit() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "pagination": {"limit": 2}
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
        "pagination limit",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "pagination limit")?;
    assert_stderr_empty(&output, "pagination limit")
}

#[test]
fn matrix_pagination_cursor_first_page() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "pagination": {"limit": 1}
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
        "pagination first page",
    )?;
    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "pagination first page")?;
    assert_stderr_empty(&output, "pagination first page")
}

#[test]
fn matrix_pagination_invalid_cursor() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "pagination": {"limit": 10, "cursor": "not-valid-base64-cursor"}
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
        "invalid cursor should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_INVALID_CURSOR", "invalid cursor")?;
    assert_stderr_empty(&output, "invalid cursor")
}

#[test]
fn matrix_pagination_zero_limit() -> TestResult {
    let (tempdir, workspace) = setup_workspace_with_memories()?;
    let query_file = write_query_file(
        &tempdir,
        r#"{
            "version": "ee.query.v1",
            "query": {"text": "release"},
            "pagination": {"limit": 0}
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
        "zero limit should fail",
    )?;
    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "ERR_INVALID_PAGINATION", "zero limit")?;
    assert_stderr_empty(&output, "zero limit")
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
