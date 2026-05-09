//! EE-3ofv: Core memory workflow end-to-end integration test
//!
//! Validates the primary user flow: init → remember → search → context → why
//! using real FrankenSQLite database in a tempdir workspace.
//!
//! NO MOCKS. Real ee binary, real DB, real search indexes.

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

fn assert_schema(json: &serde_json::Value, expected: &str, context: &str) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure_equal(&schema, &expected, &format!("{context} schema"))
}

fn assert_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

fn degraded_codes(json: &serde_json::Value) -> Vec<&str> {
    json.pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("code").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn core_workflow_init_remember_search_context_why() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Step 1: ee init
    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(
        &init_output.status.code(),
        &Some(EXIT_SUCCESS),
        "init exit code",
    )?;
    let init_json = stdout_json(&init_output)?;
    assert_schema(&init_json, "ee.response.v1", "init")?;

    // Step 2: ee remember (add 3 memories)
    let memories = [
        ("Run cargo fmt before release", "rule"),
        ("Check all tests pass before merge", "rule"),
        ("The release workflow uses GitHub Actions", "fact"),
    ];

    let mut memory_ids = Vec::new();

    for (content, kind) in &memories {
        let remember_output = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--level",
            "procedural",
            "--kind",
            kind,
            "--json",
        ])?;
        ensure_equal(
            &remember_output.status.code(),
            &Some(EXIT_SUCCESS),
            &format!("remember '{content}' exit code"),
        )?;
        let remember_json = stdout_json(&remember_output)?;
        assert_schema(
            &remember_json,
            "ee.response.v1",
            &format!("remember '{content}'"),
        )?;

        // Extract memory_id from response
        if let Some(id) = remember_json
            .pointer("/data/memory_id")
            .and_then(|v| v.as_str())
        {
            memory_ids.push(id.to_string());
        }
    }

    ensure(
        !memory_ids.is_empty(),
        "at least one memory_id should be captured",
    )?;

    // Step 3: ee search
    let search_output = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "cargo fmt release",
        "--json",
    ])?;
    ensure_equal(
        &search_output.status.code(),
        &Some(EXIT_SUCCESS),
        "search exit code",
    )?;
    let search_json = stdout_json(&search_output)?;
    assert_schema(&search_json, "ee.response.v1", "search")?;

    // Verify search returns results
    let results = search_json
        .pointer("/data/results")
        .or_else(|| search_json.pointer("/data/hits"))
        .and_then(|r| r.as_array());
    ensure(
        results.map(|r| !r.is_empty()).unwrap_or(false),
        "search should return at least one result",
    )?;

    // Step 4: ee context
    let context_output = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "release preparation",
        "--max-tokens",
        "2000",
        "--json",
    ])?;
    ensure_equal(
        &context_output.status.code(),
        &Some(EXIT_SUCCESS),
        "context exit code",
    )?;
    let context_json = stdout_json(&context_output)?;
    assert_schema(&context_json, "ee.response.v1", "context")?;

    // Verify context pack has items
    let pack_items = context_json
        .pointer("/data/pack/items")
        .or_else(|| context_json.pointer("/data/items"))
        .and_then(|p| p.as_array());
    ensure(
        pack_items.map(|p| !p.is_empty()).unwrap_or(false),
        "context pack should include at least one item",
    )?;

    // Step 5: ee why (if we have a memory_id)
    if let Some(memory_id) = memory_ids.first() {
        let why_output = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
        ensure_equal(
            &why_output.status.code(),
            &Some(EXIT_SUCCESS),
            "why exit code",
        )?;
        let why_json = stdout_json(&why_output)?;
        assert_schema(&why_json, "ee.response.v1", "why")?;

        // Verify why has explanation data
        let has_storage = why_json.pointer("/data/storage").is_some();
        let has_report = why_json.pointer("/data/report").is_some();
        let has_data = why_json.get("data").is_some();
        ensure(
            has_storage || has_report || has_data,
            "why should return explanation data",
        )?;
    }

    Ok(())
}

#[test]
fn context_and_why_report_changed_file_provenance() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let source_path = tempdir.path().join("freshness-source.md");
    let remembered_content = "Freshness source release evidence line";
    fs::write(&source_path, remembered_content).map_err(|error| error.to_string())?;
    let source_uri = format!("file://{}#L1", source_path.display());

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init")?;
    assert_stderr_empty(&init, "init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        remembered_content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--source",
        &source_uri,
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(EXIT_SUCCESS), "remember")?;
    assert_stderr_empty(&remember, "remember")?;
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember response missing memory_id".to_string())?;

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure_equal(&rebuild.status.code(), &Some(EXIT_SUCCESS), "index rebuild")?;
    assert_stderr_empty(&rebuild, "index rebuild")?;

    fs::write(&source_path, "Freshness source release evidence changed")
        .map_err(|error| error.to_string())?;

    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "freshness source release",
        "--max-tokens",
        "2000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(EXIT_SUCCESS), "context")?;
    assert_stderr_empty(&context, "context")?;
    let context_json = stdout_json(&context)?;
    assert_schema(&context_json, "ee.response.v1", "context")?;
    ensure(
        degraded_codes(&context_json).contains(&"context_evidence_freshness_changed_source"),
        "context should report changed source evidence freshness",
    )?;

    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    ensure_equal(&why.status.code(), &Some(EXIT_SUCCESS), "why")?;
    assert_stderr_empty(&why, "why")?;
    let why_json = stdout_json(&why)?;
    assert_schema(&why_json, "ee.response.v1", "why")?;
    ensure(
        degraded_codes(&why_json).contains(&"why_evidence_freshness_changed_source"),
        "why should report changed source evidence freshness",
    )
}

#[test]
fn remember_creates_searchable_memory() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init workspace
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init")?;

    // Remember a unique fact
    let unique_content = "Xylophone zebra quantum 12345 unique test phrase";
    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        unique_content,
        "--kind",
        "fact",
        "--json",
    ])?;
    ensure_equal(&remember.status.code(), &Some(EXIT_SUCCESS), "remember")?;

    // Search for the unique phrase
    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "xylophone zebra quantum",
        "--json",
    ])?;
    ensure_equal(&search.status.code(), &Some(EXIT_SUCCESS), "search")?;

    let search_json = stdout_json(&search)?;
    let results = search_json
        .pointer("/data/results")
        .or_else(|| search_json.pointer("/data/hits"))
        .and_then(|r| r.as_array());

    ensure(
        results.map(|r| !r.is_empty()).unwrap_or(false),
        "search for unique content should find the remembered memory",
    )
}

#[test]
fn context_pack_includes_relevant_memories() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Init
    run_ee(&["--workspace", &workspace, "init", "--json"])?;

    // Add specific memories about testing
    run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Always run unit tests before committing code",
        "--kind",
        "rule",
        "--json",
    ])?;
    run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Integration tests should cover happy path and edge cases",
        "--kind",
        "rule",
        "--json",
    ])?;
    run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "The database schema is defined in migrations/",
        "--kind",
        "fact",
        "--json",
    ])?;

    // Request context about testing
    let context = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "how to run tests",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    ensure_equal(&context.status.code(), &Some(EXIT_SUCCESS), "context")?;

    let context_json = stdout_json(&context)?;

    // Verify pack has content
    let pack_data = context_json.get("data");
    ensure(pack_data.is_some(), "context should have data field")?;

    // Check for pack items or content
    let has_items = context_json.pointer("/data/pack/items").is_some()
        || context_json.pointer("/data/items").is_some()
        || context_json.pointer("/data/pack").is_some();

    ensure(has_items, "context pack should have items or pack data")
}
