//! EE-mlfy: Migration boundary end-to-end test
//!
//! Validates schema upgrade behavior and data preservation across init cycles.
//! Since FrankenSQLite uses a custom format, we test migration semantics rather
//! than raw V001→current upgrade.
//!
//! NO MOCKS. Real ee binary, real FrankenSQLite.

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

fn stdout_string(output: &Output) -> Result<String, String> {
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = stdout_string(output)?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
}

#[test]
fn init_is_idempotent() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // First init creates the workspace
    let init1 = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(
        init1.status.code() == Some(EXIT_SUCCESS),
        format!("first init failed: {:?}", init1.status.code()),
    )?;

    let init1_json = stdout_json(&init1)?;
    let status1 = init1_json
        .pointer("/data/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    ensure(
        status1 == "created",
        format!("first init should create workspace, got: {status1}"),
    )?;

    // Second init is idempotent
    let init2 = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(
        init2.status.code() == Some(EXIT_SUCCESS),
        format!("second init failed: {:?}", init2.status.code()),
    )?;

    let init2_json = stdout_json(&init2)?;
    let status2 = init2_json
        .pointer("/data/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    ensure(
        status2 == "exists" || status2 == "already_exists",
        format!("second init should report exists/already_exists, got: {status2}"),
    )?;

    // Third init with --force is also idempotent
    let init3 = run_ee(&["--workspace", &workspace, "init", "--force", "--json"])?;
    ensure(
        init3.status.code() == Some(EXIT_SUCCESS),
        format!("force init failed: {:?}", init3.status.code()),
    )?;

    Ok(())
}

#[test]
fn data_survives_across_sessions() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Initialize workspace
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(
        init.status.code() == Some(EXIT_SUCCESS),
        "init failed",
    )?;

    // Add memories
    let content1 = "Migration test memory one: always verify data persistence";
    let content2 = "Migration test memory two: schema changes must preserve rows";
    let content3 = "Migration test memory three: idempotent operations are safe";

    for content in [content1, content2, content3] {
        let remember = run_ee(&[
            "--workspace", &workspace,
            "remember", content,
            "--kind", "rule",
            "--json",
        ])?;
        ensure(
            remember.status.code() == Some(EXIT_SUCCESS),
            format!("remember failed for '{content}'"),
        )?;
    }

    // Verify memories exist via search
    let search = run_ee(&[
        "--workspace", &workspace,
        "search", "migration test memory",
        "--json",
    ])?;
    ensure(
        search.status.code() == Some(EXIT_SUCCESS),
        "search failed after adding memories",
    )?;

    let search_json = stdout_json(&search)?;
    let result_count = search_json
        .pointer("/data/results")
        .or_else(|| search_json.pointer("/data/hits"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    ensure(
        result_count >= 3,
        format!("search should find at least 3 memories, found: {result_count}"),
    )?;

    // "Restart" by running another command after init (simulating new session)
    let init2 = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(
        init2.status.code() == Some(EXIT_SUCCESS),
        "re-init failed",
    )?;

    // Verify memories still exist
    let search2 = run_ee(&[
        "--workspace", &workspace,
        "search", "migration test memory",
        "--json",
    ])?;
    ensure(
        search2.status.code() == Some(EXIT_SUCCESS),
        "search failed after re-init",
    )?;

    let search2_json = stdout_json(&search2)?;
    let result_count2 = search2_json
        .pointer("/data/results")
        .or_else(|| search2_json.pointer("/data/hits"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    ensure(
        result_count2 >= 3,
        format!("memories should survive re-init, found: {result_count2}"),
    )?;

    Ok(())
}

#[test]
fn status_reports_storage_ready_after_init() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Initialize
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(
        init.status.code() == Some(EXIT_SUCCESS),
        "init failed",
    )?;

    // Check status
    let status = run_ee(&["--workspace", &workspace, "status", "--json"])?;
    ensure(
        status.status.code() == Some(EXIT_SUCCESS),
        "status failed",
    )?;

    let status_json = stdout_json(&status)?;

    // Storage should be ready
    let storage = status_json
        .pointer("/data/capabilities/storage")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    ensure(
        storage == "ready",
        format!("storage capability should be ready, got: {storage}"),
    )?;

    // Runtime should be ready
    let runtime = status_json
        .pointer("/data/capabilities/runtime")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    ensure(
        runtime == "ready",
        format!("runtime capability should be ready, got: {runtime}"),
    )?;

    Ok(())
}

#[test]
fn why_works_for_remembered_memory() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Initialize
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure(init.status.code() == Some(EXIT_SUCCESS), "init failed")?;

    // Remember something
    let remember = run_ee(&[
        "--workspace", &workspace,
        "remember", "Test memory for why command verification",
        "--kind", "fact",
        "--json",
    ])?;
    ensure(
        remember.status.code() == Some(EXIT_SUCCESS),
        "remember failed",
    )?;

    // Extract memory ID
    let remember_json = stdout_json(&remember)?;
    let memory_id = remember_json
        .pointer("/data/memory_id")
        .and_then(|v| v.as_str())
        .ok_or("failed to extract memory_id from remember response")?;

    // Verify why command works for this memory
    let why = run_ee(&["--workspace", &workspace, "why", memory_id, "--json"])?;
    ensure(
        why.status.code() == Some(EXIT_SUCCESS),
        format!("why failed for memory {memory_id}"),
    )?;

    let why_json = stdout_json(&why)?;
    let has_data = why_json.get("data").is_some();

    ensure(has_data, "why should return data for remembered memory")?;

    Ok(())
}
