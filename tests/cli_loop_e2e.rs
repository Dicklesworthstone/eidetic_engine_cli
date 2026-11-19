//! EE-lp4p.3: Real-binary CLI loop e2e test
//!
//! Tests the full ee CLI loop (init, remember, search, context, why) against a
//! temporary workspace with no mocks. Asserts JSON output schema, stable hashes,
//! and stdout/stderr isolation. Persists artifacts for first-failure diagnosis.

use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    let temp = tempfile::tempdir().map_err(|error| format!("isolated tempdir: {error}"))?;
    let isolated_root = args
        .windows(2)
        .find_map(|window| {
            (window[0] == "--workspace").then(|| {
                let ws = Path::new(window[1]);
                let name = ws.file_name().and_then(|n| n.to_str()).unwrap_or("ws");
                ws.parent()
                    .unwrap_or(ws)
                    .join(format!(".ee-isolated-{name}"))
            })
        })
        .unwrap_or_else(|| temp.path().to_path_buf());
    crate::isolated_ee::isolated_ee_command(&isolated_root)?
        .args(args)
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
    serde_json::from_str(&stdout).map_err(|error| format!("stdout was not JSON: {error}\n{stdout}"))
}

fn stdout_is_json(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout).is_ok()
}

fn stdout_is_clean(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[INFO]")
            || trimmed.starts_with("[WARN]")
            || trimmed.starts_with("[ERROR]")
            || trimmed.starts_with("warning:")
            || trimmed.starts_with("error:")
        {
            return false;
        }
    }
    true
}

fn artifact_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_loop_e2e_artifacts");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn persist_artifact(name: &str, output: &Output) {
    let dir = artifact_dir();
    let stdout_path = dir.join(format!("{name}.stdout"));
    let stderr_path = dir.join(format!("{name}.stderr"));
    let _ = fs::write(&stdout_path, &output.stdout);
    let _ = fs::write(&stderr_path, &output.stderr);
}

fn persist_json_artifact(name: &str, value: &serde_json::Value) -> TestResult {
    let dir = artifact_dir();
    let path = dir.join(format!("{name}.json"));
    let serialized = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    fs::write(&path, serialized).map_err(|error| error.to_string())
}

// ============================================================================
// CLI Loop E2E Test (init -> remember -> search -> context -> why)
// ============================================================================

#[test]
fn cli_loop_init_remember_search_context_why_full_cycle() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // Step 1: ee init
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("01_init", &init);
    ensure_equal(&init.status.code(), &Some(0), "init exit code")?;
    ensure(init.stderr.is_empty(), "init stderr must be empty")?;
    ensure(stdout_is_json(&init), "init stdout must be valid JSON")?;
    ensure(stdout_is_clean(&init), "init stdout must be clean")?;
    let init_json = stdout_json(&init)?;
    persist_json_artifact("01_init", &init_json)?;
    ensure_equal(
        &init_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "init response schema",
    )?;
    ensure_equal(
        &init_json["success"],
        &serde_json::json!(true),
        "init success",
    )?;

    // Step 2: ee remember (procedural rule)
    let remember_rule = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Always run cargo test before pushing to main.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        "testing,ci,workflow",
        "--json",
    ])?;
    persist_artifact("02_remember_rule", &remember_rule);
    ensure_equal(
        &remember_rule.status.code(),
        &Some(0),
        "remember rule exit code",
    )?;
    ensure(
        remember_rule.stderr.is_empty(),
        "remember rule stderr must be empty",
    )?;
    ensure(
        stdout_is_json(&remember_rule),
        "remember rule stdout must be valid JSON",
    )?;
    ensure(
        stdout_is_clean(&remember_rule),
        "remember rule stdout must be clean",
    )?;
    let remember_rule_json = stdout_json(&remember_rule)?;
    persist_json_artifact("02_remember_rule", &remember_rule_json)?;
    ensure_equal(
        &remember_rule_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "remember rule response schema",
    )?;
    let rule_memory_id = remember_rule_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember rule memory_id must be a string".to_string())?;
    ensure(
        rule_memory_id.starts_with("mem_"),
        "rule memory_id must have mem_ prefix",
    )?;

    // Step 3: ee remember (episodic fact)
    let remember_fact = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "A test failure on 2026-05-01 was caused by missing cargo test before push.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--tags",
        "testing,incident",
        "--json",
    ])?;
    persist_artifact("03_remember_fact", &remember_fact);
    ensure_equal(
        &remember_fact.status.code(),
        &Some(0),
        "remember fact exit code",
    )?;
    ensure(
        remember_fact.stderr.is_empty(),
        "remember fact stderr must be empty",
    )?;
    ensure(
        stdout_is_json(&remember_fact),
        "remember fact stdout must be valid JSON",
    )?;
    let remember_fact_json = stdout_json(&remember_fact)?;
    persist_json_artifact("03_remember_fact", &remember_fact_json)?;
    let fact_memory_id = remember_fact_json["data"]["memory_id"]
        .as_str()
        .ok_or_else(|| "remember fact memory_id must be a string".to_string())?;
    ensure(
        fact_memory_id.starts_with("mem_"),
        "fact memory_id must have mem_ prefix",
    )?;

    // Step 4: ee index rebuild (ensure search index is current)
    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    persist_artifact("04_index_rebuild", &rebuild);
    ensure_equal(&rebuild.status.code(), &Some(0), "index rebuild exit code")?;
    ensure(
        rebuild.stderr.is_empty(),
        "index rebuild stderr must be empty",
    )?;
    ensure(
        stdout_is_json(&rebuild),
        "index rebuild stdout must be valid JSON",
    )?;
    let rebuild_json = stdout_json(&rebuild)?;
    persist_json_artifact("04_index_rebuild", &rebuild_json)?;
    ensure_equal(
        &rebuild_json["data"]["memories_indexed"],
        &serde_json::json!(2),
        "index rebuild indexed count",
    )?;

    // Step 5: ee search
    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "cargo test before push",
        "--explain",
        "--json",
    ])?;
    persist_artifact("05_search", &search);
    ensure_equal(&search.status.code(), &Some(0), "search exit code")?;
    ensure(search.stderr.is_empty(), "search stderr must be empty")?;
    ensure(stdout_is_json(&search), "search stdout must be valid JSON")?;
    ensure(stdout_is_clean(&search), "search stdout must be clean")?;
    let search_json = stdout_json(&search)?;
    persist_json_artifact("05_search", &search_json)?;
    ensure_equal(
        &search_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "search response schema",
    )?;
    let search_results = search_json["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results must be an array".to_string())?;
    ensure(
        search_results.len() >= 2,
        "search must return at least 2 results",
    )?;
    ensure(
        search_results
            .iter()
            .any(|hit| hit["docId"].as_str() == Some(rule_memory_id)),
        "search results must include the rule memory",
    )?;
    ensure(
        search_results
            .iter()
            .any(|hit| hit["docId"].as_str() == Some(fact_memory_id)),
        "search results must include the fact memory",
    )?;
    for hit in search_results {
        ensure(
            hit["score"].as_f64().is_some(),
            "search hit must have numeric score",
        )?;
        ensure(
            hit["explanation"]["factors"]
                .as_array()
                .is_some_and(|factors| !factors.is_empty()),
            "search --explain must expose score factors",
        )?;
    }

    // Step 6: ee pack
    let context = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "prepare for push to main",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    persist_artifact("06_context", &context);
    ensure_equal(&context.status.code(), &Some(0), "context exit code")?;
    ensure(context.stderr.is_empty(), "context stderr must be empty")?;
    ensure(
        stdout_is_json(&context),
        "context stdout must be valid JSON",
    )?;
    ensure(stdout_is_clean(&context), "context stdout must be clean")?;
    let context_json = stdout_json(&context)?;
    persist_json_artifact("06_context", &context_json)?;
    ensure_equal(
        &context_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "context response schema",
    )?;
    let pack_items = context_json["data"]["pack"]["items"]
        .as_array()
        .ok_or_else(|| "context pack items must be an array".to_string())?;
    ensure(
        !pack_items.is_empty(),
        "context pack must include at least 1 item",
    )?;
    ensure(
        pack_items
            .iter()
            .any(|item| item["memoryId"].as_str() == Some(rule_memory_id)),
        "context pack must include the rule memory",
    )?;
    let pack_hash = context_json["data"]["pack"]["hash"]
        .as_str()
        .ok_or_else(|| "context pack hash must be a string".to_string())?;
    ensure(
        pack_hash.starts_with("blake3:"),
        "context pack hash must be blake3 prefixed",
    )?;
    for item in pack_items {
        ensure(
            item["scores"]["relevance"].as_f64().is_some()
                && item["scores"]["utility"].as_f64().is_some(),
            "context pack item must expose numeric score components",
        )?;
        ensure(
            item["provenance"]
                .as_array()
                .is_some_and(|provenance| !provenance.is_empty()),
            "context pack item must carry provenance",
        )?;
        ensure(
            item["why"].as_str().is_some_and(|why| !why.is_empty()),
            "context pack item must include a selection explanation",
        )?;
    }
    ensure(
        context_json["data"]["pack"]["meta"]["algorithm"]["algorithmId"]
            .as_str()
            .is_some_and(|algorithm| !algorithm.is_empty()),
        "context pack must include deterministic algorithm name",
    )?;

    // Step 7: ee why (for the rule memory)
    let why = run_ee(&["--workspace", &workspace, "why", rule_memory_id, "--json"])?;
    persist_artifact("07_why", &why);
    let why_stderr = String::from_utf8_lossy(&why.stderr);
    ensure_equal(
        &why.status.code(),
        &Some(0),
        &format!("why exit code (stderr: {why_stderr})"),
    )?;
    ensure(
        why.stderr.is_empty(),
        format!("why stderr must be empty: {why_stderr}"),
    )?;
    ensure(stdout_is_json(&why), "why stdout must be valid JSON")?;
    ensure(stdout_is_clean(&why), "why stdout must be clean")?;
    let why_json = stdout_json(&why)?;
    persist_json_artifact("07_why", &why_json)?;
    ensure_equal(
        &why_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "why response schema",
    )?;
    ensure(
        why_json["data"]["retrieval"]["confidence"]
            .as_f64()
            .is_some()
            && why_json["data"]["retrieval"]["utility"].as_f64().is_some()
            && why_json["data"]["retrieval"]["importance"]
                .as_f64()
                .is_some(),
        "why retrieval must expose numeric score inputs",
    )?;
    ensure(
        why_json["data"]["selection"]["latestPackSelection"]["packHash"]
            .as_str()
            .is_some_and(|hash| hash == pack_hash),
        "why must reference the persisted context pack hash",
    )?;
    ensure(
        why_json["data"]["selection"]["scoreBreakdown"]
            .as_str()
            .is_some_and(|breakdown| breakdown.contains("selection_score")),
        "why must expose the deterministic score formula",
    )?;

    // Verify determinism: run context again and check hash stability
    let context_replay = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "prepare for push to main",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    persist_artifact("08_context_replay", &context_replay);
    ensure_equal(
        &context_replay.status.code(),
        &Some(0),
        "context replay exit code",
    )?;
    let context_replay_json = stdout_json(&context_replay)?;
    persist_json_artifact("08_context_replay", &context_replay_json)?;
    let replay_pack_hash = context_replay_json["data"]["pack"]["hash"]
        .as_str()
        .ok_or_else(|| "context replay pack hash must be a string".to_string())?;
    ensure_equal(
        &replay_pack_hash,
        &pack_hash,
        "context pack hash must be stable across runs",
    )?;

    Ok(())
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn cli_loop_search_empty_workspace_returns_empty_results() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("empty_ws_init", &init);
    ensure_equal(&init.status.code(), &Some(0), "init exit code")?;

    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "nonexistent query",
        "--json",
    ])?;
    persist_artifact("empty_ws_search", &search);
    ensure_equal(&search.status.code(), &Some(0), "search exit code")?;
    ensure(search.stderr.is_empty(), "search stderr must be empty")?;
    let search_json = stdout_json(&search)?;
    persist_json_artifact("empty_ws_search", &search_json)?;
    let results = search_json["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results must be an array".to_string())?;
    ensure_equal(
        &results.len(),
        &0,
        "empty workspace search returns 0 results",
    )
}

#[test]
fn cli_loop_context_empty_workspace_degrades_gracefully() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("empty_ctx_init", &init);
    ensure_equal(&init.status.code(), &Some(0), "init exit code")?;

    // Context on an empty workspace should degrade gracefully (exit code 4 = search/index error)
    // or return an empty pack. Either behavior is acceptable for an empty workspace.
    let context = run_ee(&[
        "--workspace",
        &workspace,
        "pack",
        "anything",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    persist_artifact("empty_ctx_context", &context);

    // Empty workspace may return exit code 4 (search/index error) or 0 with empty pack
    let exit_code = context.status.code().unwrap_or(-1);
    ensure(
        exit_code == 0 || exit_code == 4,
        format!("context exit code must be 0 or 4 for empty workspace, got {exit_code}"),
    )?;
    ensure(
        stdout_is_json(&context),
        "context stdout must be valid JSON",
    )?;

    let context_json = stdout_json(&context)?;
    persist_json_artifact("empty_ctx_context", &context_json)?;

    if exit_code == 0 {
        // If successful, pack should be empty
        let pack_items = context_json["data"]["pack"]["items"]
            .as_array()
            .ok_or_else(|| "context pack items must be an array".to_string())?;
        ensure_equal(
            &pack_items.len(),
            &0,
            "empty workspace context returns 0 items",
        )?;
        ensure(
            context_json["data"]["pack"]["hash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "empty pack still has a deterministic hash",
        )?;
    } else {
        // Exit code 4 is acceptable for empty workspace - schema should indicate error or response
        ensure(
            context_json["schema"].as_str().is_some(),
            "degraded context must still have schema",
        )?;
    }

    Ok(())
}

#[test]
fn cli_loop_why_unknown_memory_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("why_unknown_init", &init);
    ensure_equal(&init.status.code(), &Some(0), "init exit code")?;

    let why = run_ee(&[
        "--workspace",
        &workspace,
        "why",
        "mem_nonexistent_00000000",
        "--json",
    ])?;
    persist_artifact("why_unknown", &why);
    ensure(stdout_is_json(&why), "why stdout must be valid JSON")?;
    let why_json = stdout_json(&why)?;
    persist_json_artifact("why_unknown", &why_json)?;
    ensure(
        why_json["schema"].as_str() == Some("ee.error.v2")
            || why_json["success"].as_bool() == Some(false),
        "why unknown memory must return error or unsuccessful response",
    )
}

#[test]
fn cli_loop_multiple_memories_with_same_tags_are_searchable() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("multi_tag_init", &init);
    ensure_equal(&init.status.code(), &Some(0), "init exit code")?;

    let mut memory_ids = Vec::new();
    for i in 0..3 {
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            &format!("Memory number {i} about database migrations."),
            "--level",
            "episodic",
            "--kind",
            "fact",
            "--tags",
            "database,migrations",
            "--json",
        ])?;
        persist_artifact(&format!("multi_tag_remember_{i}"), &remember);
        ensure_equal(&remember.status.code(), &Some(0), "remember exit code")?;
        let remember_json = stdout_json(&remember)?;
        let memory_id = remember_json["data"]["memory_id"]
            .as_str()
            .ok_or_else(|| "memory_id must be a string".to_string())?
            .to_string();
        memory_ids.push(memory_id);
    }

    let rebuild = run_ee(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    persist_artifact("multi_tag_rebuild", &rebuild);
    ensure_equal(&rebuild.status.code(), &Some(0), "rebuild exit code")?;

    let search = run_ee(&[
        "--workspace",
        &workspace,
        "search",
        "database migrations",
        "--json",
    ])?;
    persist_artifact("multi_tag_search", &search);
    ensure_equal(&search.status.code(), &Some(0), "search exit code")?;
    let search_json = stdout_json(&search)?;
    persist_json_artifact("multi_tag_search", &search_json)?;
    let results = search_json["data"]["results"]
        .as_array()
        .ok_or_else(|| "search results must be an array".to_string())?;
    ensure(
        results.len() >= 3,
        "search must return all 3 tagged memories",
    )?;
    for memory_id in &memory_ids {
        ensure(
            results
                .iter()
                .any(|hit| hit["docId"].as_str() == Some(memory_id)),
            format!("search results must include memory {memory_id}"),
        )?;
    }

    Ok(())
}

// ============================================================================
// Status Command Integration
// ============================================================================

#[test]
fn cli_loop_status_reports_workspace_health() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("status_init", &init);
    ensure_equal(&init.status.code(), &Some(0), "init exit code")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Test memory for status check.",
        "--level",
        "episodic",
        "--kind",
        "fact",
        "--json",
    ])?;
    persist_artifact("status_remember", &remember);
    ensure_equal(&remember.status.code(), &Some(0), "remember exit code")?;

    let status = run_ee(&[
        "--workspace",
        &workspace,
        "--fields",
        "standard",
        "status",
        "--json",
    ])?;
    persist_artifact("status_check", &status);
    let status_stderr = String::from_utf8_lossy(&status.stderr);
    ensure_equal(
        &status.status.code(),
        &Some(0),
        &format!("status exit code (stderr: {status_stderr})"),
    )?;
    ensure(
        status.stderr.is_empty(),
        format!("status stderr must be empty: {status_stderr}"),
    )?;
    ensure(stdout_is_json(&status), "status stdout must be valid JSON")?;
    ensure(stdout_is_clean(&status), "status stdout must be clean")?;
    let status_json = stdout_json(&status)?;
    persist_json_artifact("status_check", &status_json)?;
    ensure_equal(
        &status_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "status response schema",
    )?;
    ensure_equal(
        &status_json["success"],
        &serde_json::json!(true),
        "status success flag",
    )?;
    let stdout = String::from_utf8_lossy(&status.stdout);
    ensure(
        stdout.contains("\"runtime\":\"ready\""),
        "status must report runtime as ready",
    )?;
    ensure(
        stdout.contains("\"engine\":\"asupersync\""),
        "status must report asupersync engine",
    )
}

// ============================================================================
// Behavioral E2E coverage for the remaining documented CLI surfaces:
// focus show / focus explain / focus remove, workflow create, diag contention,
// and import eidetic-legacy.
// ============================================================================

#[test]
fn cli_loop_focus_show_explain_remove_lifecycle() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "focus init exit code")?;

    let mem1_out = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Always check release artifacts before publication.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ])?;
    ensure_equal(&mem1_out.status.code(), &Some(0), "remember mem1 exit code")?;
    let mem1_json = stdout_json(&mem1_out)?;
    let mem1_id = mem1_json["data"]["memoryId"]
        .as_str()
        .ok_or("missing mem1 memoryId")?
        .to_owned();

    let mem2_out = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Verify minisign signatures and SHA256SUMS for every target.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ])?;
    ensure_equal(&mem2_out.status.code(), &Some(0), "remember mem2 exit code")?;
    let mem2_json = stdout_json(&mem2_out)?;
    let mem2_id = mem2_json["data"]["memoryId"]
        .as_str()
        .ok_or("missing mem2 memoryId")?
        .to_owned();

    // Empty workspace: focus show and focus explain are read-only and report empty state.
    let show_empty = run_ee(&["--workspace", &workspace, "focus", "show", "--json"])?;
    ensure_equal(
        &show_empty.status.code(),
        &Some(0),
        "focus show empty exit code",
    )?;
    let show_empty_json = stdout_json(&show_empty)?;
    ensure_equal(
        &show_empty_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "focus show empty schema",
    )?;
    ensure_equal(
        &show_empty_json["data"]["command"],
        &serde_json::json!("focus show"),
        "focus show command",
    )?;
    ensure_equal(
        &show_empty_json["data"]["mutated"],
        &serde_json::json!(false),
        "focus show read-only",
    )?;
    ensure_equal(
        &show_empty_json["data"]["activeMemoryIds"],
        &serde_json::json!([]),
        "focus show initially empty",
    )?;

    let explain_empty = run_ee(&["--workspace", &workspace, "focus", "explain", "--json"])?;
    ensure_equal(
        &explain_empty.status.code(),
        &Some(0),
        "focus explain empty exit code",
    )?;
    let explain_empty_json = stdout_json(&explain_empty)?;
    ensure_equal(
        &explain_empty_json["data"]["command"],
        &serde_json::json!("focus explain"),
        "focus explain command",
    )?;
    let empty_explanations = explain_empty_json["data"]["explanations"]
        .as_array()
        .ok_or("focus explain empty missing explanations array")?;
    ensure(
        empty_explanations
            .iter()
            .any(|item| item["code"] == "focus_empty"),
        "focus explain empty must include focus_empty explanation",
    )?;

    // Set focus with a pinned focal item and a secondary item.
    let set = run_ee(&[
        "--workspace",
        &workspace,
        "focus",
        "set",
        &mem1_id,
        &mem2_id,
        "--focal",
        &mem1_id,
        "--pin",
        &mem1_id,
        "--reason",
        "release staging gate",
        "--json",
    ])?;
    ensure_equal(&set.status.code(), &Some(0), "focus set exit code")?;

    let show_active = run_ee(&["--workspace", &workspace, "focus", "show", "--json"])?;
    ensure_equal(
        &show_active.status.code(),
        &Some(0),
        "focus show active exit code",
    )?;
    let show_active_json = stdout_json(&show_active)?;
    let active_ids = show_active_json["data"]["activeMemoryIds"]
        .as_array()
        .ok_or("focus show active missing activeMemoryIds")?;
    ensure_equal(&active_ids.len(), &2, "focus show active count")?;
    ensure(
        active_ids.iter().any(|id| id == &mem1_id) && active_ids.iter().any(|id| id == &mem2_id),
        "focus show active must include both focused memories",
    )?;
    ensure_equal(
        &show_active_json["data"]["focusState"]["focalMemoryId"],
        &serde_json::json!(mem1_id),
        "focus show focal memory id",
    )?;

    let explain_active = run_ee(&["--workspace", &workspace, "focus", "explain", "--json"])?;
    ensure_equal(
        &explain_active.status.code(),
        &Some(0),
        "focus explain active exit code",
    )?;
    let explain_active_json = stdout_json(&explain_active)?;
    let active_explanations = explain_active_json["data"]["explanations"]
        .as_array()
        .ok_or("focus explain active missing explanations")?;
    ensure(
        active_explanations.iter().any(|item| {
            item["code"] == "focus_item_included"
                && item["memoryId"] == mem1_id
                && item["message"].as_str().is_some_and(|msg| {
                    msg.contains("pinned focal") && msg.contains("release staging gate")
                })
        }),
        "focus explain must report pinned focal reason for mem1",
    )?;

    // Remove the focal memory: clears focalMemoryId and retains mem2.
    let remove_focal = run_ee(&[
        "--workspace",
        &workspace,
        "focus",
        "remove",
        &mem1_id,
        "--json",
    ])?;
    ensure_equal(
        &remove_focal.status.code(),
        &Some(0),
        "focus remove focal exit code",
    )?;
    let remove_focal_json = stdout_json(&remove_focal)?;
    ensure_equal(
        &remove_focal_json["data"]["command"],
        &serde_json::json!("focus remove"),
        "focus remove command",
    )?;
    ensure_equal(
        &remove_focal_json["data"]["mutated"],
        &serde_json::json!(true),
        "focus remove mutated",
    )?;
    ensure_equal(
        &remove_focal_json["data"]["activeMemoryIds"],
        &serde_json::json!([mem2_id]),
        "focus remove remaining activeMemoryIds",
    )?;
    ensure(
        remove_focal_json["data"]["focusState"]["focalMemoryId"].is_null(),
        "removing focal memory must clear focalMemoryId",
    )?;

    // Idempotent repeat removal reports mutated=false and focus_remove_not_present.
    let remove_repeat = run_ee(&[
        "--workspace",
        &workspace,
        "focus",
        "remove",
        &mem1_id,
        "--json",
    ])?;
    ensure_equal(
        &remove_repeat.status.code(),
        &Some(0),
        "focus remove repeat exit code",
    )?;
    let remove_repeat_json = stdout_json(&remove_repeat)?;
    ensure_equal(
        &remove_repeat_json["data"]["mutated"],
        &serde_json::json!(false),
        "focus remove repeat must not mutate",
    )?;
    let repeat_explanations = remove_repeat_json["data"]["explanations"]
        .as_array()
        .ok_or("focus remove repeat missing explanations")?;
    ensure(
        repeat_explanations
            .iter()
            .any(|item| item["code"] == "focus_remove_not_present"),
        "focus remove repeat must report focus_remove_not_present",
    )
}

#[test]
fn cli_loop_workflow_create_storeless_dry_run_persist_and_close() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let db_path = tempdir.path().join(".ee").join("ee.db");

    // Storeless workspace without --dry-run must fail closed with exit 10 and create no DB.
    let storeless = run_ee(&[
        "--workspace",
        &workspace,
        "workflow",
        "create",
        "release-prep",
        "--json",
    ])?;
    ensure_equal(
        &storeless.status.code(),
        &Some(10),
        "workflow create on storeless workspace must exit 10",
    )?;
    ensure(
        !db_path.exists(),
        "storeless workflow create must not create .ee/ee.db",
    )?;

    // Dry-run on storeless workspace succeeds without persisting state.
    let dry_run = run_ee(&[
        "--workspace",
        &workspace,
        "workflow",
        "create",
        "release-prep",
        "--description",
        "Release staging checklist",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(
        &dry_run.status.code(),
        &Some(0),
        "workflow create --dry-run exit code",
    )?;
    let dry_run_json = stdout_json(&dry_run)?;
    ensure_equal(
        &dry_run_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "workflow create dry-run envelope schema",
    )?;
    ensure_equal(
        &dry_run_json["data"]["workflowId"],
        &serde_json::json!("release-prep"),
        "workflow create dry-run workflowId",
    )?;
    ensure_equal(
        &dry_run_json["data"]["dryRun"],
        &serde_json::json!(true),
        "workflow create dryRun flag",
    )?;
    ensure_equal(
        &dry_run_json["data"]["persisted"],
        &serde_json::json!(false),
        "workflow create dry-run persisted=false",
    )?;
    ensure(
        !db_path.exists(),
        "workflow create --dry-run must not create .ee/ee.db",
    )?;

    // Initialize workspace, create workflow durably, verify idempotence, attach memory, close.
    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "workflow init exit code")?;

    let created = run_ee(&[
        "--workspace",
        &workspace,
        "workflow",
        "create",
        "release-prep",
        "--description",
        "Release staging checklist",
        "--json",
    ])?;
    ensure_equal(
        &created.status.code(),
        &Some(0),
        "workflow create persisted exit code",
    )?;
    let created_json = stdout_json(&created)?;
    ensure_equal(
        &created_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "workflow create persisted schema",
    )?;
    ensure_equal(
        &created_json["data"]["workflowId"],
        &serde_json::json!("release-prep"),
        "workflow create persisted workflowId",
    )?;
    ensure_equal(
        &created_json["data"]["persisted"],
        &serde_json::json!(true),
        "workflow create persisted=true",
    )?;
    ensure(
        created_json["data"]["auditId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "workflow create must record a non-empty auditId",
    )?;

    let created_again = run_ee(&[
        "--workspace",
        &workspace,
        "workflow",
        "create",
        "release-prep",
        "--json",
    ])?;
    ensure_equal(
        &created_again.status.code(),
        &Some(0),
        "workflow create idempotent repeat exit code",
    )?;

    let remembered = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        "Stage private release artifacts before human sign-off.",
        "--level",
        "working",
        "--kind",
        "fact",
        "--workflow",
        "release-prep",
        "--json",
    ])?;
    ensure_equal(
        &remembered.status.code(),
        &Some(0),
        "workflow remember exit code",
    )?;

    let closed = run_ee(&[
        "--workspace",
        &workspace,
        "workflow",
        "close",
        "release-prep",
        "--json",
    ])?;
    ensure_equal(&closed.status.code(), &Some(0), "workflow close exit code")
}

#[test]
fn cli_loop_diag_contention_default_triage_and_missing_daemon() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(0), "diag contention init")?;

    let default_out = run_ee(&["--workspace", &workspace, "diag", "contention", "--json"])?;
    ensure_equal(
        &default_out.status.code(),
        &Some(0),
        "diag contention exit code",
    )?;
    ensure(
        default_out.stderr.is_empty(),
        "diag contention --json stderr must be empty",
    )?;
    let default_json = stdout_json(&default_out)?;
    ensure_equal(
        &default_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "diag contention envelope schema",
    )?;
    ensure_equal(
        &default_json["success"],
        &serde_json::json!(true),
        "diag contention success",
    )?;
    ensure_equal(
        &default_json["data"]["command"],
        &serde_json::json!("diag contention"),
        "diag contention command",
    )?;
    ensure_equal(
        &default_json["data"]["report"]["schema"],
        &serde_json::json!("ee.diag.contention.v1"),
        "diag contention report schema",
    )?;
    ensure(
        default_json["data"]["report"]["overallPosture"]
            .as_str()
            .is_some_and(|posture| !posture.is_empty()),
        "diag contention must include overallPosture",
    )?;
    ensure_equal(
        &default_json["degraded"],
        &serde_json::json!([]),
        "diag contention in-process degraded empty",
    )?;

    let triage_out = run_ee(&[
        "--workspace",
        &workspace,
        "diag",
        "contention",
        "--robot-triage",
        "--json",
    ])?;
    ensure_equal(
        &triage_out.status.code(),
        &Some(0),
        "diag contention --robot-triage exit code",
    )?;
    let triage_json = stdout_json(&triage_out)?;
    ensure_equal(
        &triage_json["data"]["robotTriage"],
        &serde_json::json!(true),
        "diag contention robotTriage flag",
    )?;
    ensure(
        triage_json["data"]["topContention"].is_array(),
        "diag contention --robot-triage must surface topContention array",
    )?;

    let missing_socket = tempdir.path().join("missing-daemon.sock");
    let missing_socket_arg = missing_socket.to_string_lossy().to_string();
    let daemon_fallback = run_ee(&[
        "--workspace",
        &workspace,
        "diag",
        "contention",
        "--use-daemon",
        "--daemon-socket",
        &missing_socket_arg,
        "--json",
    ])?;
    ensure_equal(
        &daemon_fallback.status.code(),
        &Some(0),
        "diag contention --use-daemon missing socket fallback exit code",
    )?;
    let fallback_json = stdout_json(&daemon_fallback)?;
    let degraded = fallback_json["degraded"]
        .as_array()
        .ok_or("diag contention fallback missing degraded array")?;
    ensure(
        !degraded.is_empty(),
        "diag contention --use-daemon with missing socket must disclose degradation",
    )
}

#[test]
fn cli_loop_import_eidetic_legacy_dry_run_and_non_dry_run_refusal() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let canonical_root = tempdir
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize tempdir: {error}"))?;
    let legacy_dir = canonical_root.join("legacy-export");
    fs::create_dir_all(&legacy_dir).map_err(|error| error.to_string())?;
    fs::write(
        legacy_dir.join("sessions.json"),
        r#"{"messages":[{"role":"assistant","content":"Always verify checksums."}]}"#,
    )
    .map_err(|error| error.to_string())?;
    fs::write(legacy_dir.join("notes.bin"), b"ignored binary blob")
        .map_err(|error| error.to_string())?;
    let legacy_arg = legacy_dir.to_string_lossy().to_string();

    // Negative control: omitting --dry-run must fail closed (import error exit 5).
    let refused = run_ee(&[
        "import",
        "eidetic-legacy",
        "--source",
        &legacy_arg,
        "--json",
    ])?;
    ensure_equal(
        &refused.status.code(),
        &Some(5),
        "import eidetic-legacy without --dry-run must exit 5",
    )?;
    let refused_json = stdout_json(&refused)?;
    ensure_equal(
        &refused_json["schema"],
        &serde_json::json!("ee.error.v2"),
        "import eidetic-legacy refusal error envelope",
    )?;

    // Positive path: --dry-run scans without mutating storage and reports discovered artifacts.
    let scanned = run_ee(&[
        "import",
        "eidetic-legacy",
        "--source",
        &legacy_arg,
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(
        &scanned.status.code(),
        &Some(0),
        "import eidetic-legacy --dry-run exit code",
    )?;
    let scanned_json = stdout_json(&scanned)?;
    ensure_equal(
        &scanned_json["schema"],
        &serde_json::json!("ee.response.v2"),
        "import eidetic-legacy response schema",
    )?;
    ensure_equal(
        &scanned_json["success"],
        &serde_json::json!(true),
        "import eidetic-legacy success",
    )?;
    ensure_equal(
        &scanned_json["data"]["schema"],
        &serde_json::json!("ee.import.eidetic_legacy.scan.v1"),
        "import eidetic-legacy inner schema",
    )?;
    ensure_equal(
        &scanned_json["data"]["command"],
        &serde_json::json!("import eidetic-legacy"),
        "import eidetic-legacy command",
    )?;
    ensure_equal(
        &scanned_json["data"]["dryRun"],
        &serde_json::json!(true),
        "import eidetic-legacy dryRun flag",
    )?;
    ensure_equal(
        &scanned_json["data"]["artifactsFound"],
        &serde_json::json!(1),
        "import eidetic-legacy artifactsFound",
    )?;
    ensure_equal(
        &scanned_json["data"]["scannedFiles"],
        &serde_json::json!(2),
        "import eidetic-legacy scannedFiles",
    )
}
