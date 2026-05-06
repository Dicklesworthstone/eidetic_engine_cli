//! E2E coverage for graceful search degradation with a stale index.
//!
//! NO MOCKS. Real ee binary, real workspace database, real search index.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::db::{CreateMemoryInput, DbConnection};
use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

struct EeOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    json: Value,
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
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
        .as_nanos();
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("e2e-graceful-degradation")
        .join(format!("{}-{}-{nanos}", name, std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee<I, S>(workspace: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee: {error}"))
}

fn run_ee_json<I, S>(workspace: &Path, args: I, context: &str) -> Result<EeOutput, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_ee(workspace, args)?;
    let stdout =
        String::from_utf8(output.stdout).map_err(|error| format!("{context} stdout: {error}"))?;
    let stderr =
        String::from_utf8(output.stderr).map_err(|error| format!("{context} stderr: {error}"))?;
    let json = serde_json::from_str(&stdout)
        .map_err(|error| format!("{context} stdout was not JSON: {error}\nstdout: {stdout}"))?;
    Ok(EeOutput {
        exit_code: output.status.code(),
        stdout,
        stderr,
        json,
    })
}

fn assert_success(output: &EeOutput, context: &str) -> TestResult {
    ensure_equal(&output.exit_code, &Some(EXIT_SUCCESS), context)?;
    ensure(
        output.stderr.trim().is_empty(),
        format!(
            "{context}: JSON stderr must stay empty, got {:?}",
            output.stderr
        ),
    )?;
    ensure_equal(
        &output.json.pointer("/schema"),
        &Some(&Value::String("ee.response.v1".to_owned())),
        context,
    )?;
    ensure_equal(
        &output.json.pointer("/success"),
        &Some(&Value::Bool(true)),
        context,
    )
}

fn remember(workspace: &Path, content: &str) -> Result<String, String> {
    let output = run_ee_json(
        workspace,
        [
            "remember",
            content,
            "--level",
            "procedural",
            "--kind",
            "rule",
        ],
        "remember",
    )?;
    assert_success(&output, "remember")?;
    output
        .json
        .pointer("/data/memory_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember output missing memory id: {}", output.stdout))
}

fn insert_unindexed_memory(workspace: &Path, content: &str) -> Result<String, String> {
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?;
    let workspace_id = workspaces
        .first()
        .map(|workspace| workspace.id.clone())
        .ok_or_else(|| "workspace row missing after ee init".to_owned())?;
    let memory_id = "mem_00000000000000000000007001".to_owned();
    let input = CreateMemoryInput {
        workspace_id,
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        content: content.to_owned(),
        workflow_id: None,
        confidence: 0.8,
        utility: 0.5,
        importance: 0.5,
        provenance_uri: Some("test://eidetic_engine_cli-0io7/unindexed-memory".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: Some("e2e direct stale-index fixture".to_owned()),
        tags: vec!["stale-index".to_owned(), "fallback".to_owned()],
        valid_from: None,
        valid_to: None,
    };
    connection
        .insert_memory(&memory_id, &input)
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;
    Ok(memory_id)
}

fn result_doc_ids(search_json: &Value) -> Result<Vec<String>, String> {
    let results = search_json
        .pointer("/data/results")
        .and_then(Value::as_array)
        .ok_or_else(|| "search output missing /data/results array".to_owned())?;
    Ok(results
        .iter()
        .filter_map(|result| {
            result
                .get("docId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}

fn degraded_codes(search_json: &Value) -> Vec<String> {
    search_json
        .pointer("/data/degraded")
        .or_else(|| search_json.pointer("/degraded"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("code").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn derived_asset_status(status_json: &Value, name: &str) -> Option<String> {
    status_json
        .pointer("/data/derivedAssets")
        .and_then(Value::as_array)?
        .iter()
        .find(|asset| asset.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|asset| asset.get("status"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[test]
fn stale_index_search_degrades_to_lexical_fallback_and_recovers_after_rebuild() -> TestResult {
    let artifact_dir = unique_artifact_dir("stale-index-search")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace: {error}"))?;

    let init = run_ee_json(&workspace, ["init"], "init")?;
    assert_success(&init, "init")?;

    let first_memory = remember(
        &workspace,
        "staleindex alpha release fallback search remains available for old indexed memories",
    )?;
    let second_memory = remember(
        &workspace,
        "staleindex alpha cargo check guidance can be retrieved while semantic search is degraded",
    )?;
    let third_memory = remember(
        &workspace,
        "staleindex alpha context packs should explain provenance and degraded retrieval",
    )?;

    let rebuild = run_ee_json(&workspace, ["index", "rebuild"], "initial index rebuild")?;
    assert_success(&rebuild, "initial index rebuild")?;
    ensure_equal(
        &rebuild.json.pointer("/data/memories_indexed"),
        &Some(&Value::from(3)),
        "initial rebuild memory count",
    )?;

    let fresh_index = run_ee_json(&workspace, ["index", "status"], "fresh index status")?;
    assert_success(&fresh_index, "fresh index status")?;
    ensure_equal(
        &fresh_index.json.pointer("/data/health"),
        &Some(&Value::String("ready".to_owned())),
        "fresh index health",
    )?;

    let new_memory = insert_unindexed_memory(
        &workspace,
        "staleindex bravo lexical fallback target appears only after the stale index is rebuilt",
    )?;

    let stale_status = run_ee_json(&workspace, ["status"], "stale workspace status")?;
    assert_success(&stale_status, "stale workspace status")?;
    ensure_equal(
        &derived_asset_status(&stale_status.json, "search_index"),
        &Some("stale".to_owned()),
        "status reports stale search index",
    )?;

    let stale_search = run_ee_json(
        &workspace,
        ["search", "staleindex alpha bravo fallback", "--limit", "10"],
        "stale search",
    )?;
    assert_success(&stale_search, "stale search")?;
    let stale_doc_ids = result_doc_ids(&stale_search.json)?;
    ensure(
        stale_doc_ids.iter().any(|doc_id| {
            doc_id == &first_memory || doc_id == &second_memory || doc_id == &third_memory
        }),
        format!("stale search should still return old indexed lexical results: {stale_doc_ids:?}"),
    )?;
    ensure(
        !stale_doc_ids.iter().any(|doc_id| doc_id == &new_memory),
        "stale search should not claim the unindexed new memory before rebuild",
    )?;
    let stale_degraded_codes = degraded_codes(&stale_search.json);
    ensure(
        stale_degraded_codes
            .iter()
            .any(|code| code == "search_index_stale" || code == "stale_index"),
        format!("stale search should expose stale-index degradation: {stale_degraded_codes:?}"),
    )?;
    ensure(
        stale_search
            .json
            .pointer("/data/degraded/0/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("lexical") && message.contains("fallback")),
        "stale search degradation must explain lexical fallback",
    )?;

    let final_rebuild = run_ee_json(&workspace, ["index", "rebuild"], "final index rebuild")?;
    assert_success(&final_rebuild, "final index rebuild")?;
    ensure_equal(
        &final_rebuild.json.pointer("/data/memories_indexed"),
        &Some(&Value::from(4)),
        "final rebuild memory count",
    )?;

    let recovered_search = run_ee_json(
        &workspace,
        [
            "search",
            "staleindex bravo lexical fallback target",
            "--limit",
            "10",
        ],
        "recovered search",
    )?;
    assert_success(&recovered_search, "recovered search")?;
    let recovered_doc_ids = result_doc_ids(&recovered_search.json)?;
    ensure(
        recovered_doc_ids.iter().any(|doc_id| doc_id == &new_memory),
        format!("rebuilt search should return the newly indexed memory: {recovered_doc_ids:?}"),
    )?;
    ensure(
        recovered_doc_ids.len() > stale_doc_ids.len()
            || recovered_doc_ids
                .iter()
                .any(|doc_id| !stale_doc_ids.contains(doc_id)),
        "rebuilt search should improve result coverage after indexing the new memory",
    )?;
    ensure(
        degraded_codes(&recovered_search.json).is_empty(),
        "recovered search should not report stale-index degradation after rebuild",
    )
}
