//! E2E coverage for graceful search degradation with a stale index.
//!
//! NO MOCKS. Real ee binary, real workspace database, real search index.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ee::db::{CreateMemoryInput, CreateSearchIndexJobInput, DbConnection, SearchIndexJobType};
use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const SNAPSHOT_RACE_MEMORY_ID: &str = "mem_00000000000000000000007901";
const SNAPSHOT_RACE_JOB_ID: &str = "sidx_00000000000000000000007901";
const SNAPSHOT_RACE_CONTENT: &str =
    "snapshotrace zircon unique second phrase committed by a separate writer process";

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

fn private_runtime_tempdir(name: &str) -> Result<tempfile::TempDir, String> {
    #[cfg(unix)]
    let temp_root = fs::canonicalize("/tmp")
        .map_err(|error| format!("failed to canonicalize Unix temp root: {error}"))?;
    #[cfg(not(unix))]
    let temp_root = env::temp_dir();

    tempfile::Builder::new()
        .prefix(&format!("ee-{name}-"))
        .tempdir_in(&temp_root)
        .map_err(|error| {
            format!(
                "failed to create private runtime tempdir under {}: {error}",
                temp_root.display()
            )
        })
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
    parse_ee_output(output, context)
}

fn parse_ee_output(output: Output, context: &str) -> Result<EeOutput, String> {
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

fn spawn_ee<I, S>(workspace: &Path, args: I) -> Result<Child, String>
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn ee: {error}"))
}

fn assert_success(output: &EeOutput, context: &str) -> TestResult {
    ensure(
        output.exit_code == Some(EXIT_SUCCESS),
        format!(
            "{context}: expected exit {EXIT_SUCCESS}, got {:?}; stdout: {}; stderr: {}",
            output.exit_code, output.stdout, output.stderr
        ),
    )?;
    ensure(
        output.stderr.trim().is_empty(),
        format!(
            "{context}: JSON stderr must stay empty, got {:?}",
            output.stderr
        ),
    )?;
    ensure_equal(
        &output.json.pointer("/schema"),
        &Some(&Value::String("ee.response.v2".to_owned())),
        context,
    )?;
    ensure_equal(
        &output.json.pointer("/success"),
        &Some(&Value::Bool(true)),
        context,
    )
}

fn remember_with_validity(
    workspace: &Path,
    content: &str,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> Result<String, String> {
    let mut args = vec![
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
    ];
    if let Some(valid_from) = valid_from {
        args.extend(["--valid-from", valid_from]);
    }
    if let Some(valid_to) = valid_to {
        args.extend(["--valid-to", valid_to]);
    }

    let output = run_ee_json(workspace, args, "remember")?;
    assert_success(&output, "remember")?;
    output
        .json
        .pointer("/data/memory_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember output missing memory id: {}", output.stdout))
}

fn remember(workspace: &Path, content: &str) -> Result<String, String> {
    remember_with_validity(workspace, content, None, None)
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

fn seed_snapshot_race_corpus(workspace: &Path, document_count: u32) -> TestResult {
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?
        .first()
        .map(|stored| stored.id.clone())
        .ok_or_else(|| "workspace row missing after ee init".to_owned())?;
    let padding = " deterministic baseline retrieval corpus".repeat(48);
    for ordinal in 0..document_count {
        let memory_id = format!("mem_{:026}", 80_000_u64 + u64::from(ordinal));
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!(
                        "snapshotrace baseline document {ordinal} captured before publication{padding}"
                    ),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("test://snapshot-race/baseline".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("multiprocess source-snapshot fixture".to_owned()),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
    }
    connection.close().map_err(|error| error.to_string())
}

fn wait_for_index_publish_window(workspace: &Path, child: &mut Child) -> TestResult {
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?
        .first()
        .map(|stored| stored.id.clone())
        .ok_or_else(|| "workspace row missing while waiting for index publisher".to_owned())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let lock_visible = connection
            .list_active_advisory_locks()
            .map_err(|error| error.to_string())?
            .iter()
            .any(|lock| {
                lock.id.resource_type() == "index" && lock.id.resource_id() == workspace_id.as_str()
            });
        let staging_visible = fs::read_dir(workspace.join(".ee"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".index.publish-"))
            });
        if lock_visible || staging_visible {
            connection.close().map_err(|error| error.to_string())?;
            return Ok(());
        }
        if child
            .try_wait()
            .map_err(|error| format!("failed to inspect rebuild child: {error}"))?
            .is_some()
        {
            return Err(
                "index rebuild exited before its post-snapshot publish window was observable"
                    .to_owned(),
            );
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for index rebuild publish window".to_owned());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn spawn_snapshot_writer_process(workspace: &Path) -> Result<Output, String> {
    let current_test_binary =
        env::current_exe().map_err(|error| format!("failed to resolve test binary: {error}"))?;
    Command::new(current_test_binary)
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture")
        .arg("multiprocess_snapshot_writer_helper")
        .env("EE_SNAPSHOT_WRITER_WORKSPACE", workspace)
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to spawn snapshot writer process: {error}"))
}

fn pack_memory_ids(pack_json: &Value) -> Vec<String> {
    pack_json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("memoryId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
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

fn derived_asset<'a>(status_json: &'a Value, name: &str) -> Option<&'a Value> {
    status_json
        .pointer("/data/derivedAssets")
        .and_then(Value::as_array)?
        .iter()
        .find(|asset| asset.get("name").and_then(Value::as_str) == Some(name))
}

fn doctor_check_severity<'a>(doctor_json: &'a Value, name: &str) -> Option<&'a str> {
    doctor_json
        .pointer("/data/checks")
        .and_then(Value::as_array)?
        .iter()
        .find(|check| check.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|check| check.get("severity"))
        .and_then(Value::as_str)
}

fn assert_index_admission_refused(output: &EeOutput, reason: &str) -> TestResult {
    ensure_equal(&output.exit_code, &Some(4), "unverifiable index exit code")?;
    ensure(
        output.stderr.trim().is_empty(),
        format!("JSON refusal stderr must stay empty: {}", output.stderr),
    )?;
    ensure_equal(
        &output.json.get("schema").and_then(Value::as_str),
        &Some("ee.error.v2"),
        "unverifiable index envelope",
    )?;
    ensure_equal(
        &output.json.pointer("/error/code").and_then(Value::as_str),
        &Some("search_index"),
        "unverifiable index error code",
    )?;
    ensure(
        output
            .json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains(reason) && message.contains("meta.json")),
        format!(
            "index refusal must retain its concrete metadata reason: {}",
            output.stdout
        ),
    )?;
    ensure_equal(
        &output.json.pointer("/error/repair").and_then(Value::as_str),
        &Some("ee index rebuild --workspace ."),
        "unverifiable index repair",
    )?;
    ensure(
        output.json.get("data").is_none() && output.json.get("success") != Some(&Value::Bool(true)),
        format!(
            "unverifiable index must not expose results or claim success: {}",
            output.stdout
        ),
    )
}

#[test]
#[ignore = "spawned by the multiprocess source-snapshot regression"]
fn multiprocess_snapshot_writer_helper() -> TestResult {
    let workspace = env::var_os("EE_SNAPSHOT_WRITER_WORKSPACE")
        .map(PathBuf::from)
        .ok_or_else(|| "EE_SNAPSHOT_WRITER_WORKSPACE is required".to_owned())?;
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?
        .first()
        .map(|stored| stored.id.clone())
        .ok_or_else(|| "writer process could not resolve workspace row".to_owned())?;
    connection
        .with_transaction(|| {
            connection.insert_memory(
                SNAPSHOT_RACE_MEMORY_ID,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: SNAPSHOT_RACE_CONTENT.to_owned(),
                    workflow_id: None,
                    confidence: 0.99,
                    utility: 0.75,
                    importance: 0.8,
                    provenance_uri: Some("test://snapshot-race/second-process".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("multiprocess writer".to_owned()),
                    tags: vec!["snapshot-race".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )?;
            connection.insert_search_index_job(
                SNAPSHOT_RACE_JOB_ID,
                &CreateSearchIndexJobInput {
                    workspace_id: workspace_id.clone(),
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(SNAPSHOT_RACE_MEMORY_ID.to_owned()),
                    documents_total: 1,
                },
            )
        })
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())
}

#[test]
fn corrupt_index_metadata_requires_rebuild_before_search() -> TestResult {
    let artifact_dir = unique_artifact_dir("corrupt-index-metadata")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace: {error}"))?;

    let init = run_ee_json(&workspace, ["init"], "init")?;
    assert_success(&init, "init")?;

    let memory_id = remember(
        &workspace,
        "corruptindex alpha search should report corrupt metadata before repair",
    )?;

    let rebuild = run_ee_json(&workspace, ["index", "rebuild"], "initial index rebuild")?;
    assert_success(&rebuild, "initial index rebuild")?;

    let metadata_path = workspace.join(".ee").join("index").join("meta.json");
    fs::write(&metadata_path, "{ not-json")
        .map_err(|error| format!("failed to corrupt {}: {error}", metadata_path.display()))?;

    let corrupt_status = run_ee_json(&workspace, ["index", "status"], "corrupt index status")?;
    assert_success(&corrupt_status, "corrupt index status")?;
    ensure_equal(
        &corrupt_status.json.pointer("/data/health"),
        &Some(&Value::String("corrupt".to_owned())),
        "corrupt metadata reports corrupt index health",
    )?;

    let corrupt_search = run_ee_json(
        &workspace,
        ["search", "corruptindex alpha metadata", "--limit", "10"],
        "corrupt metadata search",
    )?;
    // Metadata carries the evidence-egress policy epoch. Unreadable metadata
    // cannot authorize even lexical index bytes; preserve that admission gate
    // while distinguishing corruption from a genuinely absent index.
    assert_index_admission_refused(&corrupt_search, "failed to parse index metadata")?;
    let corrupt_diag = run_ee_json(
        &workspace,
        [
            "diag",
            "search",
            "corruptindex alpha metadata",
            "--all-arms",
        ],
        "corrupt metadata diagnostic search",
    )?;
    assert_index_admission_refused(&corrupt_diag, "failed to parse index metadata")?;

    fs::rename(
        &metadata_path,
        metadata_path.with_file_name("corrupt-meta.retained.json"),
    )
    .map_err(|error| format!("failed to retain corrupt metadata: {error}"))?;
    let missing_search = run_ee_json(
        &workspace,
        [
            "search",
            "corruptindex alpha metadata",
            "--source-mode",
            "lexical_only",
        ],
        "missing metadata lexical search",
    )?;
    assert_index_admission_refused(&missing_search, "is missing")?;

    let repair = run_ee_json(&workspace, ["index", "rebuild"], "repair corrupt index")?;
    assert_success(&repair, "repair corrupt index")?;
    let repaired_status = run_ee_json(&workspace, ["index", "status"], "repaired index status")?;
    assert_success(&repaired_status, "repaired index status")?;
    ensure_equal(
        &repaired_status
            .json
            .pointer("/data/health")
            .and_then(Value::as_str),
        &Some("ready"),
        "repaired index health",
    )?;
    let repaired_search = run_ee_json(
        &workspace,
        [
            "search",
            "corruptindex alpha metadata",
            "--source-mode",
            "lexical_only",
            "--limit",
            "10",
        ],
        "repaired lexical search",
    )?;
    assert_success(&repaired_search, "repaired lexical search")?;
    let repaired_doc_ids = result_doc_ids(&repaired_search.json)?;
    ensure(
        repaired_doc_ids.iter().any(|doc_id| doc_id == &memory_id),
        format!("repaired lexical search must return the original memory: {repaired_doc_ids:?}"),
    )?;
    ensure_equal(
        &repaired_search
            .json
            .pointer("/data/metrics/sourceModeApplied")
            .and_then(Value::as_str),
        &Some("lexical_only"),
        "repaired search source mode",
    )?;
    ensure(
        degraded_codes(&repaired_search.json).is_empty(),
        format!(
            "repaired lexical search must be healthy: {}",
            repaired_search.stdout
        ),
    )
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

    let stale_status = run_ee_json(
        &workspace,
        ["--fields", "standard", "status"],
        "stale workspace status",
    )?;
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
    let recovered_codes = degraded_codes(&recovered_search.json);
    ensure(
        !recovered_codes.iter().any(|code| {
            matches!(
                code.as_str(),
                "search_index_stale"
                    | "index_stale"
                    | "stale_index"
                    | "search_index_large_gap"
                    | "search_index_degraded"
                    | "index_missing"
                    | "index_corrupt"
            )
        }),
        format!(
            "recovered search should not report stale, missing, or corrupt index after rebuild: {recovered_codes:?}"
        ),
    )?;
    let recovered_status = run_ee_json(&workspace, ["index", "status"], "recovered index status")?;
    assert_success(&recovered_status, "recovered index status")?;
    ensure_equal(
        &recovered_status.json.pointer("/data/health"),
        &Some(&Value::String("ready".to_owned())),
        "recovered index health",
    )
}

#[test]
fn ready_index_posture_is_coherent_across_public_cli_surfaces() -> TestResult {
    let artifact_dir = unique_artifact_dir("ready-index-posture-coherence")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace: {error}"))?;

    let init = run_ee_json(&workspace, ["init"], "coherence init")?;
    assert_success(&init, "coherence init")?;
    let memory_id = remember(
        &workspace,
        "coherentindex x65f public diagnostics share one ready generation authority",
    )?;
    let rebuild = run_ee_json(&workspace, ["index", "rebuild"], "coherence rebuild")?;
    assert_success(&rebuild, "coherence rebuild")?;

    // No GENERATION-ADVANCING write occurs after this point: every command
    // observes the same durable workspace snapshot even though each public CLI
    // invocation is a fresh process.
    //
    // The unqualified "no writes" this said before is not true any more. The
    // `search` below appends a retrieval audit row (ADR 0071; declared as
    // `append_only_write("search", ["audit_log"])` since bd-czj3e), so it is a
    // durable writer and the `pack` after it does not run against a
    // byte-identical store. The coherence assertions still hold, and the
    // reason is worth stating rather than leaving a reader to re-derive it:
    // `db_generation` is read from the `workspace_generations` table
    // (src/db/mod.rs:11731), which is advanced only by triggers on `memories`,
    // `memory_tags`, `memory_links`, `procedural_rules`, `curation_candidates`,
    // `evidence_spans`, `rule_*`, `sessions`, `error_repair_links` and
    // `workspaces`. `audit_log` has no such trigger, so an audit append cannot
    // move a generation and cannot make this test's index posture disagree.
    let index_status = run_ee_json(&workspace, ["index", "status"], "coherent index status")?;
    assert_success(&index_status, "coherent index status")?;
    ensure_equal(
        &index_status.json.pointer("/data/health"),
        &Some(&Value::String("ready".to_owned())),
        "index status ready posture",
    )?;
    let database_generation = index_status
        .json
        .pointer("/data/dbGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("index status omitted dbGeneration: {}", index_status.stdout))?;
    let index_generation = index_status
        .json
        .pointer("/data/indexGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "index status omitted indexGeneration: {}",
                index_status.stdout
            )
        })?;
    ensure_equal(
        &index_generation,
        &database_generation,
        "ready index status generation equality",
    )?;

    let status = run_ee_json(
        &workspace,
        ["--fields", "standard", "status"],
        "coherent aggregate status",
    )?;
    assert_success(&status, "coherent aggregate status")?;
    let search_asset = derived_asset(&status.json, "search_index")
        .ok_or_else(|| format!("status omitted search_index asset: {}", status.stdout))?;
    ensure_equal(
        &search_asset.get("status"),
        &Some(&Value::String("current".to_owned())),
        "aggregate status ready posture",
    )?;
    ensure_equal(
        &search_asset
            .get("sourceHighWatermark")
            .and_then(Value::as_u64),
        &Some(database_generation),
        "status source generation matches index status",
    )?;
    ensure_equal(
        &search_asset
            .get("assetHighWatermark")
            .and_then(Value::as_u64),
        &Some(index_generation),
        "status asset generation matches index status",
    )?;

    let doctor = run_ee_json(&workspace, ["doctor", "--full"], "coherent doctor")?;
    assert_success(&doctor, "coherent doctor")?;
    ensure_equal(
        &doctor_check_severity(&doctor.json, "search_index"),
        &Some("ok"),
        "doctor ready posture",
    )?;

    let search = run_ee_json(
        &workspace,
        [
            "search",
            "coherentindex x65f generation authority",
            "--source-mode",
            "lexical_only",
            "--limit",
            "10",
        ],
        "coherent search",
    )?;
    assert_success(&search, "coherent search")?;
    ensure(
        result_doc_ids(&search.json)?
            .iter()
            .any(|doc_id| doc_id == &memory_id),
        format!("ready search omitted indexed memory: {}", search.stdout),
    )?;
    ensure(
        !degraded_codes(&search.json)
            .iter()
            .any(|code| code == "search_index_stale" || code == "index_stale"),
        format!(
            "ready search contradicted index status with stale posture: {}",
            search.stdout
        ),
    )?;

    let pack = run_ee_json(
        &workspace,
        [
            "pack",
            "coherentindex x65f generation authority",
            "--source-mode",
            "lexical_only",
            "--max-tokens",
            "2048",
        ],
        "coherent pack",
    )?;
    assert_success(&pack, "coherent pack")?;
    ensure(
        pack_memory_ids(&pack.json)
            .iter()
            .any(|packed_id| packed_id == &memory_id),
        format!("ready pack omitted indexed memory: {}", pack.stdout),
    )?;
    ensure(
        !degraded_codes(&pack.json)
            .iter()
            .any(|code| code == "search_index_stale" || code == "index_stale"),
        format!(
            "ready pack contradicted index status with stale posture: {}",
            pack.stdout
        ),
    )
}

#[test]
fn multiprocess_write_after_source_snapshot_is_present_or_explicitly_stale() -> TestResult {
    // The DB path hardening correctly rejects symlinked ancestors. Pinned
    // verification materializes the checkout below a compatibility symlink,
    // so this real multi-process fixture must use a canonical runtime root
    // rather than CARGO_TARGET_DIR.
    let artifact_dir = private_runtime_tempdir("multiprocess-source-snapshot")?;
    let workspace = artifact_dir.path().join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace: {error}"))?;

    let init = run_ee_json(&workspace, ["init"], "snapshot race init")?;
    assert_success(&init, "snapshot race init")?;
    seed_snapshot_race_corpus(&workspace, 256)?;

    let mut rebuild_child = spawn_ee(&workspace, ["index", "rebuild"])?;
    wait_for_index_publish_window(&workspace, &mut rebuild_child)?;

    let writer = spawn_snapshot_writer_process(&workspace)?;
    ensure(
        writer.status.success(),
        format!(
            "separate writer process failed: stdout={} stderr={}",
            String::from_utf8_lossy(&writer.stdout),
            String::from_utf8_lossy(&writer.stderr)
        ),
    )?;

    let rebuild = parse_ee_output(
        rebuild_child
            .wait_with_output()
            .map_err(|error| format!("failed waiting for index rebuild: {error}"))?,
        "snapshot race rebuild",
    )?;
    assert_success(&rebuild, "snapshot race rebuild")?;
    ensure_equal(
        &rebuild.json.pointer("/data/documents_total"),
        &Some(&Value::from(256)),
        "rebuild must publish exactly the captured pre-writer corpus",
    )?;

    let stale = run_ee_json(
        &workspace,
        ["index", "status"],
        "post-snapshot index status",
    )?;
    assert_success(&stale, "post-snapshot index status")?;
    ensure_equal(
        &stale.json.pointer("/data/health"),
        &Some(&Value::String("stale".to_owned())),
        "post-snapshot writer must make the older publication explicitly stale",
    )?;
    let database_generation = stale
        .json
        .pointer("/data/dbGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("index status missing dbGeneration: {}", stale.stdout))?;
    let index_generation = stale
        .json
        .pointer("/data/indexGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("index status missing indexGeneration: {}", stale.stdout))?;
    ensure(
        database_generation > index_generation,
        format!(
            "the manifest must retain its captured watermark: db={database_generation} index={index_generation}"
        ),
    )?;
    ensure_equal(
        &stale.json.pointer("/data/repairHint"),
        &Some(&Value::String("ee index rebuild --workspace .".to_owned())),
        "stale index exposes an actionable repair",
    )?;

    let pending_connection = DbConnection::open_file(workspace.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;
    let pending_job = pending_connection
        .get_search_index_job(SNAPSHOT_RACE_JOB_ID)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "second writer's index job is missing".to_owned())?;
    ensure_equal(
        &pending_job.status.as_str(),
        &"pending",
        "second writer leaves a durable repair job",
    )?;
    pending_connection
        .close()
        .map_err(|error| error.to_string())?;

    let stale_search = run_ee_json(
        &workspace,
        ["search", SNAPSHOT_RACE_CONTENT, "--limit", "10"],
        "stale unique-phrase search",
    )?;
    assert_success(&stale_search, "stale unique-phrase search")?;
    let stale_result_ids = result_doc_ids(&stale_search.json)?;
    ensure(
        !stale_result_ids
            .iter()
            .any(|doc_id| doc_id == SNAPSHOT_RACE_MEMORY_ID),
        format!(
            "pre-writer snapshot must not silently claim the second memory is indexed: {stale_result_ids:?}"
        ),
    )?;
    ensure(
        degraded_codes(&stale_search.json).iter().any(|code| {
            code == "search_index_stale" || code == "stale_index" || code == "search_index_degraded"
        }),
        format!(
            "missing second memory must be paired with stale-or-degraded index disclosure: {}",
            stale_search.stdout
        ),
    )?;

    let stale_pack = run_ee_json(
        &workspace,
        ["pack", SNAPSHOT_RACE_CONTENT, "--max-tokens", "2048"],
        "stale unique-phrase pack",
    )?;
    assert_success(&stale_pack, "stale unique-phrase pack")?;
    let stale_pack_ids = pack_memory_ids(&stale_pack.json);
    let stale_pack_degraded = degraded_codes(&stale_pack.json);
    ensure(
        stale_pack_ids
            .iter()
            .any(|memory_id| memory_id == SNAPSHOT_RACE_MEMORY_ID)
            || stale_pack_degraded
                .iter()
                .any(|code| code.contains("stale") || code == "search_index_degraded"),
        format!(
            "pack must either carry the committed memory or disclose stale/degraded retrieval: ids={stale_pack_ids:?} degraded={stale_pack_degraded:?}"
        ),
    )?;

    let coalesce = run_ee_json(
        &workspace,
        ["job", "run", "index_coalesce"],
        "snapshot repair coalesce",
    )?;
    assert_success(&coalesce, "snapshot repair coalesce")?;

    let ready = run_ee_json(
        &workspace,
        ["index", "status"],
        "repaired snapshot index status",
    )?;
    assert_success(&ready, "repaired snapshot index status")?;
    ensure_equal(
        &ready.json.pointer("/data/health"),
        &Some(&Value::String("ready".to_owned())),
        "bounded repair converges to a truthful ready index",
    )?;
    ensure_equal(
        &ready.json.pointer("/data/indexGeneration"),
        &ready.json.pointer("/data/dbGeneration"),
        "ready requires equal source and manifest generations",
    )?;

    let recovered_search = run_ee_json(
        &workspace,
        ["search", SNAPSHOT_RACE_CONTENT, "--limit", "10"],
        "recovered unique-phrase search",
    )?;
    assert_success(&recovered_search, "recovered unique-phrase search")?;
    let recovered_ids = result_doc_ids(&recovered_search.json)?;
    ensure(
        recovered_ids
            .iter()
            .any(|doc_id| doc_id == SNAPSHOT_RACE_MEMORY_ID),
        format!(
            "repaired current index must contain the second writer's memory: {recovered_ids:?}"
        ),
    )?;
    ensure(
        !degraded_codes(&recovered_search.json)
            .iter()
            .any(|code| code == "search_index_stale" || code == "stale_index"),
        format!(
            "recovered search must not retain stale-index degradation: {}",
            recovered_search.stdout
        ),
    )?;

    let recovered_pack = run_ee_json(
        &workspace,
        ["pack", SNAPSHOT_RACE_CONTENT, "--max-tokens", "2048"],
        "recovered unique-phrase pack",
    )?;
    assert_success(&recovered_pack, "recovered unique-phrase pack")?;
    ensure(
        pack_memory_ids(&recovered_pack.json)
            .iter()
            .any(|memory_id| memory_id == SNAPSHOT_RACE_MEMORY_ID),
        format!(
            "recovered pack must include the second writer's memory: {}",
            recovered_pack.stdout
        ),
    )
}

#[test]
fn search_validity_window_filters_real_index_results_and_opt_ins_restore_them() -> TestResult {
    let artifact_dir = unique_artifact_dir("search-validity-window")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace: {error}"))?;

    let init = run_ee_json(&workspace, ["init"], "init")?;
    assert_success(&init, "init")?;

    let current_memory = remember_with_validity(
        &workspace,
        "validwindow alpha temporal search current rule remains visible during May proof",
        Some("2026-01-01T00:00:00Z"),
        Some("2026-12-31T23:59:59Z"),
    )?;
    let expired_memory = remember_with_validity(
        &workspace,
        "validwindow alpha temporal search expired rule is hidden unless expired memories are requested",
        Some("2020-01-01T00:00:00Z"),
        Some("2021-01-01T00:00:00Z"),
    )?;
    let future_memory = remember_with_validity(
        &workspace,
        "validwindow alpha temporal search future rule is hidden unless future memories are requested",
        Some("2099-01-01T00:00:00Z"),
        None,
    )?;

    let rebuild = run_ee_json(&workspace, ["index", "rebuild"], "index rebuild")?;
    assert_success(&rebuild, "index rebuild")?;
    ensure_equal(
        &rebuild.json.pointer("/data/memories_indexed"),
        &Some(&Value::from(3)),
        "validity rebuild memory count",
    )?;

    let default_search = run_ee_json(
        &workspace,
        [
            "search",
            "validwindow alpha temporal search",
            "--as-of",
            "2026-05-13T00:00:00Z",
            "--source-mode",
            "lexical_only",
            "--relevance-floor",
            "0.0",
            "--limit",
            "10",
        ],
        "default validity search",
    )?;
    assert_success(&default_search, "default validity search")?;
    let default_doc_ids = result_doc_ids(&default_search.json)?;
    ensure(
        default_doc_ids
            .iter()
            .any(|doc_id| doc_id == &current_memory),
        format!("default validity search should include current memory: {default_doc_ids:?}"),
    )?;
    ensure(
        !default_doc_ids
            .iter()
            .any(|doc_id| doc_id == &expired_memory),
        format!("default validity search should exclude expired memory: {default_doc_ids:?}"),
    )?;
    ensure(
        !default_doc_ids
            .iter()
            .any(|doc_id| doc_id == &future_memory),
        format!("default validity search should exclude future memory: {default_doc_ids:?}"),
    )?;

    let include_expired_search = run_ee_json(
        &workspace,
        [
            "search",
            "validwindow alpha temporal search expired",
            "--as-of",
            "2026-05-13T00:00:00Z",
            "--include-expired",
            "--source-mode",
            "lexical_only",
            "--relevance-floor",
            "0.0",
            "--limit",
            "10",
        ],
        "include expired validity search",
    )?;
    assert_success(&include_expired_search, "include expired validity search")?;
    let include_expired_doc_ids = result_doc_ids(&include_expired_search.json)?;
    ensure(
        include_expired_doc_ids
            .iter()
            .any(|doc_id| doc_id == &expired_memory),
        format!(
            "include-expired validity search should restore expired memory: {include_expired_doc_ids:?}"
        ),
    )?;
    ensure(
        !include_expired_doc_ids
            .iter()
            .any(|doc_id| doc_id == &future_memory),
        format!(
            "include-expired validity search should still exclude future memory: {include_expired_doc_ids:?}"
        ),
    )?;

    let include_future_search = run_ee_json(
        &workspace,
        [
            "search",
            "validwindow alpha temporal search future",
            "--as-of",
            "2026-05-13T00:00:00Z",
            "--include-future",
            "--source-mode",
            "lexical_only",
            "--relevance-floor",
            "0.0",
            "--limit",
            "10",
        ],
        "include future validity search",
    )?;
    assert_success(&include_future_search, "include future validity search")?;
    let include_future_doc_ids = result_doc_ids(&include_future_search.json)?;
    ensure(
        include_future_doc_ids
            .iter()
            .any(|doc_id| doc_id == &future_memory),
        format!(
            "include-future validity search should restore future memory: {include_future_doc_ids:?}"
        ),
    )?;
    ensure(
        !include_future_doc_ids
            .iter()
            .any(|doc_id| doc_id == &expired_memory),
        format!(
            "include-future validity search should still exclude expired memory: {include_future_doc_ids:?}"
        ),
    )
}

/// The scope disclaimer `pack` must carry whenever it reports a degraded
/// banner, in the two halves bd-pack-doctor-posture-disagreement-nts29 asked
/// for: the verdict's scope, and a pointer at the surface that owns workspace
/// health. Matched as substrings so wording can be revised without silently
/// dropping either half.
const PACK_SCOPE_DISCLAIMER_FRAGMENTS: [&str; 2] = ["this pack only", "ee doctor"];

fn pack_banner<'a>(pack_json: &'a Value, context: &str) -> Result<&'a Value, String> {
    pack_json
        .pointer("/data/pack/advisoryBanner")
        .ok_or_else(|| format!("{context}: pack output has no /data/pack/advisoryBanner"))
}

/// The reconciliation rule, applied identically to every observed state.
///
/// NOT "the two surfaces must agree". This bead's own root cause establishes
/// that `pack` reports the retrieval degradations of ONE invocation while
/// `doctor` reports static workspace health, from disjoint inputs, BY DESIGN.
/// A must-always-match assertion would encode a requirement ee deliberately
/// does not meet, and the first legitimate divergence would be "fixed" by
/// weakening it.
///
/// What is owed instead is disclosure: whenever pack renders `degraded`, its
/// banner must say that the verdict is scoped to that invocation and point at
/// the surface that does own workspace posture. Then a reader who sees
/// `doctor: ok` beside `pack: degraded` is told why, instead of concluding one
/// of them is lying.
fn ensure_pack_banner_reconciles_with_doctor(
    pack_json: &Value,
    doctor_verdict: &str,
    state: &str,
) -> TestResult {
    let banner = pack_banner(pack_json, state)?;
    let status = banner
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{state}: advisoryBanner has no status"))?;
    let summary = banner
        .get("summary")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{state}: advisoryBanner has no summary"))?;
    let degradation_count = banner
        .get("degradationCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{state}: advisoryBanner has no degradationCount"))?;

    // Vocabulary invariant, both directions. A `degraded` banner with no
    // counted degradation, or counted degradations under a `clear` banner,
    // is the two halves of the same surface disagreeing with each other --
    // which has to be excluded before any cross-surface claim means anything.
    ensure(
        (status == "degraded") == (degradation_count > 0),
        format!(
            "{state}: pack banner status and degradationCount contradict each other \
             (status={status}, degradationCount={degradation_count}, doctor posture={doctor_verdict})"
        ),
    )?;

    if status == "degraded" {
        for fragment in PACK_SCOPE_DISCLAIMER_FRAGMENTS {
            ensure(
                summary.contains(fragment),
                format!(
                    "{state}: pack reported `degraded` while doctor reported `{doctor_verdict}`, \
                     and its banner omitted `{fragment}`; an unexplained disagreement between \
                     these two surfaces is the defect this test exists to prevent. summary={summary:?}"
                ),
            )?;
        }
    }
    Ok(())
}

fn doctor_posture(doctor_json: &Value, context: &str) -> Result<String, String> {
    doctor_json
        .pointer("/data/posture")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context}: doctor output has no /data/posture"))
}

/// bd-pack-doctor-posture-disagreement-nts29, acceptance bullet 3: one
/// workspace evaluated by BOTH surfaces, verdict vocabulary reconciled.
///
/// Bullets 1 and 2 are unit-level and already pass. This is the cross-surface
/// half, and it was the one genuinely missing: the only other `nts29` hits
/// under `tests/` are recorded run logs, not assertions.
///
/// Shaped after `ready_index_posture_is_coherent_across_public_cli_surfaces`
/// above -- one workspace, no writes between invocations, several public CLI
/// surfaces -- which is the right shape and had simply never been pointed at
/// the pack/doctor pair.
///
/// STATE B is what keeps this from being a vacuous pass. On a healthy
/// workspace pack renders `clear`, so the disclosure rule holds with a false
/// antecedent and asserts nothing. `insert_unindexed_memory` writes a memory
/// row without an index job; the `memories` insert trigger advances
/// `workspace_generations` while the published index generation stays put, so
/// the index is stale deterministically -- no env knob, no model download,
/// and the same mechanism `stale_index_search_degrades_to_lexical_fallback_
/// and_recovers_after_rebuild` already relies on. The test asserts the
/// antecedent actually fired before leaning on it.
///
/// Deliberately NOT built on `embed_model_unavailable`, the other obvious way
/// to make pack degrade: `doctor` reports that same code
/// (src/core/doctor.rs:2937), so the two surfaces AGREE there and a test built
/// on it would be green without exercising a disagreement at all.
#[test]
fn pack_and_doctor_verdicts_are_reconciled_across_one_workspace() -> TestResult {
    let artifact_dir = unique_artifact_dir("pack-doctor-posture-reconciliation")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace: {error}"))?;

    let init = run_ee_json(&workspace, ["init"], "nts29 init")?;
    assert_success(&init, "nts29 init")?;
    remember(
        &workspace,
        "posturecheck quill release checklist requires cargo fmt before tagging",
    )?;
    remember(
        &workspace,
        "posturecheck quill a prior release failed when clippy was skipped entirely",
    )?;
    let rebuild = run_ee_json(&workspace, ["index", "rebuild"], "nts29 rebuild")?;
    assert_success(&rebuild, "nts29 rebuild")?;

    // STATE A -- healthy workspace, current index. Baseline: whatever the two
    // surfaces say here, they must already satisfy the rule.
    let healthy_doctor = run_ee_json(&workspace, ["doctor", "--full"], "nts29 healthy doctor")?;
    assert_success(&healthy_doctor, "nts29 healthy doctor")?;
    let healthy_posture = doctor_posture(&healthy_doctor.json, "nts29 healthy doctor")?;
    let healthy_pack = run_ee_json(
        &workspace,
        [
            "pack",
            "posturecheck quill release checklist",
            "--source-mode",
            "lexical_only",
            "--max-tokens",
            "2048",
        ],
        "nts29 healthy pack",
    )?;
    assert_success(&healthy_pack, "nts29 healthy pack")?;
    ensure_pack_banner_reconciles_with_doctor(
        &healthy_pack.json,
        &healthy_posture,
        "healthy workspace",
    )?;

    // STATE B -- a durable memory with no index job. The index is now stale
    // against the database, which is a REAL retrieval degradation for pack
    // and a workspace fact doctor evaluates on its own terms.
    insert_unindexed_memory(
        &workspace,
        "posturecheck quill unindexed row forces the published index generation to fall behind",
    )?;

    let stale_doctor = run_ee_json(&workspace, ["doctor", "--full"], "nts29 stale doctor")?;
    assert_success(&stale_doctor, "nts29 stale doctor")?;
    let stale_posture = doctor_posture(&stale_doctor.json, "nts29 stale doctor")?;

    // No `--source-mode` here, unlike STATE A: this is the invocation shape
    // that `stale_index_search_degrades_to_lexical_fallback_and_recovers_
    // after_rebuild` already proves surfaces the stale-index degradation.
    let stale_pack = run_ee_json(
        &workspace,
        [
            "pack",
            "posturecheck quill release checklist",
            "--max-tokens",
            "2048",
        ],
        "nts29 stale pack",
    )?;
    assert_success(&stale_pack, "nts29 stale pack")?;

    // NON-VACUITY GUARD. Everything below is an implication keyed on this
    // being `degraded`; if the stale index stops reaching the pack banner the
    // rule silently stops being tested, so the antecedent is asserted rather
    // than hoped for. If this line is what fails, the finding is "a stale
    // index no longer degrades the pack banner", not "the disclosure is
    // missing" -- and the two verdicts are printed so the reader can tell.
    let stale_banner = pack_banner(&stale_pack.json, "nts29 stale pack")?;
    let stale_status = stale_banner
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    ensure(
        stale_status == "degraded",
        format!(
            "a stale index must reach the pack advisory banner, or this test's disclosure rule \
             is never exercised: banner status={stale_status}, doctor posture={stale_posture}, \
             banner={stale_banner}"
        ),
    )?;

    ensure_pack_banner_reconciles_with_doctor(&stale_pack.json, &stale_posture, "stale index")?;

    // The disagreement this bead is named for is ALLOWED, and is exactly what
    // the disclaimer exists to explain -- so it is recorded, not forbidden.
    // What is forbidden, and asserted above, is that it happen silently.
    ensure(
        !healthy_posture.is_empty() && !stale_posture.is_empty(),
        format!(
            "doctor must render a posture in both states: healthy={healthy_posture:?}, \
             stale={stale_posture:?}"
        ),
    )
}

fn status_memory_subsystem(status_json: &Value, context: &str) -> Result<Value, String> {
    status_json
        .pointer("/data/posture/subsystems")
        .and_then(Value::as_array)
        .and_then(|subsystems| {
            subsystems
                .iter()
                .find(|subsystem| subsystem.get("id").and_then(Value::as_str) == Some("memory"))
        })
        .cloned()
        .ok_or_else(|| format!("{context}: status has no memory subsystem"))
}

/// Run ee with no semantic model reachable: downloads off and an empty model
/// directory. This pins the deterministic-hash fallback instead of depending on
/// whatever model a host happens to have cached.
fn run_ee_json_no_model<I, S>(
    workspace: &Path,
    model_dir: &Path,
    args: I,
    context: &str,
) -> Result<EeOutput, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env("NO_COLOR", "1")
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_EMBED_MODEL_DIR", model_dir)
        .output()
        .map_err(|error| format!("failed to run ee: {error}"))?;
    parse_ee_output(output, context)
}

/// bd-jmlzs. A fresh store of rules a human stated with `ee remember` (trust
/// class `human_explicit`, no `--source`) used to make `status` report memory
/// `degraded_recoverable` (`memory_health_degraded`, repaired by nothing),
/// while `doctor` said ok and `health` said healthy. The provenance component
/// was 0, and the health score is the minimum of its components.
///
/// bd-rzfov. With no semantic model, `status` also degraded its core `search`
/// row (and `pack` through it) for the deterministic-hash fallback, while
/// `doctor` (embedding_posture, advisory tier) and `health` said ok. ADR 0081
/// D1 says an honest hash fallback must not degrade the top-line, so the three
/// surfaces now agree on `overall`. The fallback stays visible in `degraded[]`
/// with a repair that fetches the model, which this test asserts, so the
/// hash-fallback path is shown to be the one exercised.
///
/// The second arm is the control that keeps the first honest: the same
/// explicit rules with a tombstoned majority must still degrade memory health,
/// and with it status's overall posture.
#[test]
fn fresh_explicit_store_posture_agrees_across_status_doctor_health() -> TestResult {
    let artifact_dir = unique_artifact_dir("fresh-explicit-posture")?;
    let workspace = artifact_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create workspace: {error}"))?;
    let model_dir = artifact_dir.join("empty-model-cache");
    fs::create_dir_all(&model_dir)
        .map_err(|error| format!("failed to create model dir: {error}"))?;
    let init = run_ee_json_no_model(&workspace, &model_dir, ["init"], "explicit init")?;
    assert_success(&init, "explicit init")?;
    let mut memory_ids = Vec::new();
    for index in 0..8 {
        let content = format!("explicitposture rule {index}: run the formatter before committing");
        let remembered = run_ee_json_no_model(
            &workspace,
            &model_dir,
            [
                "remember",
                content.as_str(),
                "--level",
                "procedural",
                "--kind",
                "rule",
            ],
            "explicit remember",
        )?;
        assert_success(&remembered, "explicit remember")?;
        memory_ids.push(
            remembered
                .json
                .pointer("/data/memory_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    format!("remember output missing memory id: {}", remembered.stdout)
                })?,
        );
    }

    let status = run_ee_json_no_model(&workspace, &model_dir, ["status"], "explicit status")?;
    assert_success(&status, "explicit status")?;
    let doctor = run_ee_json_no_model(&workspace, &model_dir, ["doctor"], "explicit doctor")?;
    assert_success(&doctor, "explicit doctor")?;
    let health = run_ee_json_no_model(&workspace, &model_dir, ["health"], "explicit health")?;
    assert_success(&health, "explicit health")?;
    eprintln!(
        "fresh explicit store: status overall={:?}, non-ok subsystems={:?}",
        status.json.pointer("/data/posture/overall"),
        status
            .json
            .pointer("/data/posture/subsystems")
            .and_then(Value::as_array)
            .map(|subsystems| subsystems
                .iter()
                .filter(|subsystem| subsystem.get("status").and_then(Value::as_str) != Some("ok"))
                .map(|subsystem| subsystem.get("id").cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()),
    );

    let memory = status_memory_subsystem(&status.json, "explicit status")?;
    ensure_equal(
        &memory.get("status").and_then(Value::as_str),
        &Some("ok"),
        "status memory subsystem on a fresh explicit store",
    )?;
    ensure(
        !status.stdout.contains("memory_health_degraded"),
        "status must not report memory_health_degraded for a fresh explicit store",
    )?;
    ensure_equal(
        &doctor
            .json
            .pointer("/data/healthy")
            .and_then(Value::as_bool),
        &Some(true),
        "doctor healthy on the same store",
    )?;
    ensure_equal(
        &health.json.pointer("/data/verdict").and_then(Value::as_str),
        &Some("healthy"),
        "health verdict on the same store",
    )?;
    // The overall agreement bd-jmlzs requires (bd-rzfov).
    ensure_equal(
        &status
            .json
            .pointer("/data/posture/overall")
            .and_then(Value::as_str),
        &Some("ok"),
        "status overall on a fresh explicit store with no model",
    )?;
    ensure_equal(
        &doctor.json.pointer("/data/posture").and_then(Value::as_str),
        &Some("ok"),
        "doctor posture on the same store",
    )?;
    // The cause beside the label: the hash fallback really is active, and
    // it is still reported, with a repair that fetches the model.
    let lexical = status
        .json
        .pointer("/degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.get("code").and_then(Value::as_str) == Some("search_lexical_only")
            })
        })
        .ok_or_else(|| {
            format!(
                "status must still report search_lexical_only under the hash fallback: {}",
                status.stdout
            )
        })?;
    ensure(
        lexical
            .get("repair")
            .and_then(Value::as_str)
            .is_some_and(|repair| repair.contains("ee model fetch")),
        format!("hash-fallback repair must fetch the model: {lexical}"),
    )?;

    // Control: tombstone 6 of the 8. Live rows are still attested, but the
    // active ratio and tombstone penalty must still degrade memory health.
    for memory_id in &memory_ids[..6] {
        let tombstone = run_ee_json_no_model(
            &workspace,
            &model_dir,
            [
                "curate",
                "tombstone",
                memory_id.as_str(),
                "--actor",
                "bd-jmlzs-control",
                "--reason",
                "Planted: a tombstoned majority must still degrade memory health.",
            ],
            "explicit tombstone",
        )?;
        // `curate tombstone` answers with its bare report, not the
        // ee.response.v2 envelope (tests/e2e_curate_tombstone.rs pins this),
        // so check the report itself.
        ensure_equal(&tombstone.exit_code, &Some(EXIT_SUCCESS), "tombstone exit")?;
        ensure_equal(
            &tombstone.json.pointer("/schema").and_then(Value::as_str),
            &Some("ee.curate.tombstone.v1"),
            "tombstone schema",
        )?;
        ensure_equal(
            &tombstone.json.pointer("/memoryId").and_then(Value::as_str),
            &Some(memory_id.as_str()),
            "tombstone memory id",
        )?;
        ensure_equal(
            &tombstone
                .json
                .pointer("/persisted")
                .and_then(Value::as_bool),
            &Some(true),
            "tombstone persisted",
        )?;
    }
    let degraded = run_ee_json_no_model(&workspace, &model_dir, ["status"], "tombstoned status")?;
    assert_success(&degraded, "tombstoned status")?;
    let memory = status_memory_subsystem(&degraded.json, "tombstoned status")?;
    ensure_equal(
        &memory.get("status").and_then(Value::as_str),
        &Some("degraded_recoverable"),
        "status memory subsystem with a tombstoned majority",
    )?;
    ensure_equal(
        &memory.get("reason").and_then(Value::as_str),
        &Some("memory_health_degraded"),
        "tombstoned majority reason",
    )?;
    ensure_equal(
        &degraded
            .json
            .pointer("/data/posture/overall")
            .and_then(Value::as_str),
        &Some("degraded_recoverable"),
        "status overall with a tombstoned majority",
    )
}
