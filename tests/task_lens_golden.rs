use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ee::db::DbConnection;

type TestResult = Result<(), String>;

fn run_ee(workspace: &Path, args: &[&str]) -> Result<Output, String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize {}: {error}", workspace.display()))?;
    let runtime_dir = workspace.join(".test-runtime");
    let data_home = workspace.join(".test-data");
    let cache_home = workspace.join(".test-cache");
    for path in [&runtime_dir, &data_home, &cache_home] {
        fs::create_dir_all(path)
            .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    }

    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(&workspace)
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_PROFILE")
        .env_remove("EE_MAX_TOKENS")
        .env_remove("EE_DATABASE")
        .env_remove("EE_INDEX_DIR")
        .env_remove("EE_AGENT_NAME")
        .env_remove("EE_OUTPUT_FORMAT")
        .env_remove("EE_JSON")
        .env_remove("EE_HOOK_MODE")
        .env_remove("EE_MAX_OUTPUT_TOKENS")
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_CACHE_HOME", &cache_home)
        .env("TMPDIR", &runtime_dir)
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

fn ensure_success(output: &Output, context: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{context} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn stdout_json(output: &Output, context: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context} stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context} stdout was not JSON: {error}\nstdout:\n{stdout}"))
}

fn json_pointer<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
) -> Result<&'a serde_json::Value, String> {
    value
        .pointer(pointer)
        .ok_or_else(|| format!("missing JSON pointer {pointer}"))
}

fn blake3_sentinel(value: &serde_json::Value, context: &str) -> Result<serde_json::Value, String> {
    let hash = value
        .as_str()
        .ok_or_else(|| format!("{context} must be a string"))?;
    ensure(
        hash.starts_with("blake3:"),
        format!("{context} must be blake3-prefixed, got {hash:?}"),
    )?;
    Ok(serde_json::json!("<blake3>"))
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("task_lens")
        .join(format!("{name}.json.golden"))
}

fn assert_json_golden(name: &str, actual: serde_json::Value) -> TestResult {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create golden directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let mut actual_text =
            serde_json::to_string_pretty(&actual).map_err(|error| error.to_string())?;
        actual_text.push('\n');
        fs::write(&path, actual_text)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }
    let expected_text = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let expected: serde_json::Value = serde_json::from_str(&expected_text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    if actual == expected {
        return Ok(());
    }
    let expected_pretty =
        serde_json::to_string_pretty(&expected).map_err(|error| error.to_string())?;
    let actual_pretty = serde_json::to_string_pretty(&actual).map_err(|error| error.to_string())?;
    Err(format!(
        "golden mismatch for {}\nexpected:\n{}\nactual:\n{}",
        path.display(),
        expected_pretty,
        actual_pretty
    ))
}

#[test]
fn lens_explain_bugfix_matches_golden_projection() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    ensure_success(&run_ee(tempdir.path(), &["--json", "init"])?, "ee init")?;

    let explain = run_ee(tempdir.path(), &["--json", "lens", "explain", "bugfix"])?;
    ensure_success(&explain, "ee lens explain")?;
    let value = stdout_json(&explain, "ee lens explain")?;
    let lens = json_pointer(&value, "/data/lens")?;

    let projection = serde_json::json!({
        "schema": json_pointer(&value, "/schema")?,
        "success": json_pointer(&value, "/success")?,
        "degraded": json_pointer(&value, "/degraded")?,
        "command": json_pointer(&value, "/data/command")?,
        "requestedLens": json_pointer(&value, "/data/requestedLens")?,
        "lens": {
            "schema": json_pointer(lens, "/schema")?,
            "id": json_pointer(lens, "/id")?,
            "version": json_pointer(lens, "/version")?,
            "lensHash": blake3_sentinel(json_pointer(lens, "/lensHash")?, "lens hash")?,
            "overlay": json_pointer(lens, "/overlay")?,
        },
        "explanation": json_pointer(&value, "/data/explanation")?,
    });
    assert_json_golden("lens_explain_bugfix", projection)
}

#[test]
fn pack_with_bugfix_lens_replay_matches_golden_projection() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path();
    ensure_success(&run_ee(workspace, &["--json", "init"])?, "ee init")?;

    let remember = run_ee(
        workspace,
        &[
            "--json",
            "remember",
            "Release failure reproduced by rerunning the exact failing command.",
            "--level",
            "episodic",
            "--kind",
            "failure",
        ],
    )?;
    ensure_success(&remember, "ee remember")?;
    let remember_json = stdout_json(&remember, "ee remember")?;
    let memory_id = json_pointer(&remember_json, "/data/memory_id")?
        .as_str()
        .ok_or_else(|| "remember memory_id must be a string".to_owned())?
        .to_owned();

    let pack = run_ee(
        workspace,
        &[
            "--json",
            "pack",
            "fix release failure",
            "--lens",
            "bugfix",
            "--profile",
            "compact",
            "--max-tokens",
            "2000",
            "--candidate-pool",
            "17",
            "--source-mode",
            "lexical-only",
        ],
    )?;
    ensure_success(&pack, "ee pack --lens")?;
    let pack_json = stdout_json(&pack, "ee pack --lens")?;
    ensure(
        json_pointer(&pack_json, "/data/pack/items")?
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "lens pack should select at least one memory",
    )?;

    let database = workspace.join(".ee").join("ee.db");
    let connection =
        DbConnection::open_schema_only(&database).map_err(|error| error.to_string())?;
    let pack_history = connection
        .list_pack_records_for_memory(&memory_id, 1)
        .map_err(|error| format!("failed to list pack records for {memory_id}: {error}"))?;
    let pack_id = pack_history
        .first()
        .map(|(record, _)| record.id.clone())
        .ok_or_else(|| "expected persisted pack record for remembered memory".to_owned())?;
    connection.close().map_err(|error| error.to_string())?;

    let replay = run_ee(workspace, &["--json", "pack", "replay", &pack_id])?;
    ensure_success(&replay, "ee pack replay")?;
    let replay_json = stdout_json(&replay, "ee pack replay")?;
    let ledger_task_lens = json_pointer(&replay_json, "/data/replay/ledger/taskLens")?;

    let projection = serde_json::json!({
        "pack": {
            "schema": json_pointer(&pack_json, "/schema")?,
            "success": json_pointer(&pack_json, "/success")?,
            "command": json_pointer(&pack_json, "/data/command")?,
            "request": {
                "query": json_pointer(&pack_json, "/data/request/query")?,
                "profile": json_pointer(&pack_json, "/data/request/profile")?,
                "maxTokens": json_pointer(&pack_json, "/data/request/maxTokens")?,
                "candidatePool": json_pointer(&pack_json, "/data/request/candidatePool")?,
            },
            "hash": blake3_sentinel(json_pointer(&pack_json, "/data/pack/hash")?, "pack hash")?,
            "selectedItemCount": json_pointer(&pack_json, "/data/pack/items")?
                .as_array()
                .map_or(0, Vec::len),
        },
        "replay": {
            "schema": json_pointer(&replay_json, "/schema")?,
            "success": json_pointer(&replay_json, "/success")?,
            "command": json_pointer(&replay_json, "/data/command")?,
            "status": json_pointer(&replay_json, "/data/replay/status")?,
            "pack": {
                "profile": json_pointer(&replay_json, "/data/pack/profile")?,
                "maxTokens": json_pointer(&replay_json, "/data/pack/maxTokens")?,
                "hash": blake3_sentinel(json_pointer(&replay_json, "/data/pack/packHash")?, "replay pack hash")?,
                "ledgerHash": blake3_sentinel(json_pointer(&replay_json, "/data/pack/ledgerHash")?, "replay ledger hash")?,
            },
            "ledgerRequest": {
                "profile": json_pointer(&replay_json, "/data/replay/ledger/request/profile")?,
                "maxTokens": json_pointer(&replay_json, "/data/replay/ledger/request/maxTokens")?,
            },
            "ledgerTaskLens": {
                "id": json_pointer(ledger_task_lens, "/id")?,
                "version": json_pointer(ledger_task_lens, "/version")?,
                "lensHash": blake3_sentinel(json_pointer(ledger_task_lens, "/lensHash")?, "ledger task lens hash")?,
            },
            "selectedItemCount": json_pointer(&replay_json, "/data/replay/selectedItems")?
                .as_array()
                .map_or(0, Vec::len),
        },
    });
    assert_json_golden("pack_bugfix_replay", projection)
}
