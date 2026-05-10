//! Real-binary E2E coverage for `ee index vacuum`.
//!
//! The command is intentionally preview-only: it inspects derived search-index
//! assets and reports reclaimable candidates without deleting or rewriting
//! files.

use serde_json::{Value as JsonValue, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_run_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-index-vacuum-e2e")
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee(workspace: &Path, args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", args))
}

fn parse_stdout_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context} stdout was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn append_event(
    log_path: &Path,
    step: &str,
    args: &[String],
    output: &Output,
) -> Result<(), String> {
    let event = json!({
        "schema": "ee.index_vacuum_e2e_event.v1",
        "step": step,
        "args": args,
        "exitCode": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("failed to open {}: {error}", log_path.display()))?;
    serde_json::to_writer(&mut file, &event)
        .map_err(|error| format!("failed to write event JSON: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("failed to write event newline: {error}"))
}

fn normalize_vacuum_response(mut value: JsonValue) -> JsonValue {
    value["data"]["databasePath"] = JsonValue::String("<DATABASE>".to_owned());
    value["data"]["indexDir"] = JsonValue::String("<INDEX>".to_owned());
    value["data"]["before"]["path"] = JsonValue::String("<INDEX>".to_owned());
    value["data"]["after"]["path"] = JsonValue::String("<INDEX>".to_owned());
    value["data"]["elapsedMs"] = JsonValue::from(0);
    if value["data"]["lock"]["lockId"].is_string() {
        value["data"]["lock"]["lockId"] = JsonValue::String("<LOCK_ID>".to_owned());
    }
    value
}

#[test]
fn index_vacuum_reports_missing_index_preview_with_logged_binary_run() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create {}: {error}", workspace.display()))?;
    let events_path = run_dir.join("events.jsonl");
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let init_args = vec![
        "--workspace".to_owned(),
        workspace_arg.clone(),
        "--json".to_owned(),
        "init".to_owned(),
    ];
    let init_output = run_ee(&workspace, &init_args)?;
    append_event(&events_path, "init", &init_args, &init_output)?;
    ensure(
        init_output.status.success(),
        format!(
            "init failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&init_output.stdout),
            String::from_utf8_lossy(&init_output.stderr)
        ),
    )?;

    let vacuum_args = vec![
        "--workspace".to_owned(),
        workspace_arg,
        "--json".to_owned(),
        "index".to_owned(),
        "vacuum".to_owned(),
    ];
    let vacuum_output = run_ee(&workspace, &vacuum_args)?;
    append_event(&events_path, "index_vacuum", &vacuum_args, &vacuum_output)?;
    ensure(
        vacuum_output.status.success(),
        format!(
            "index vacuum failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&vacuum_output.stdout),
            String::from_utf8_lossy(&vacuum_output.stderr)
        ),
    )?;
    ensure(
        vacuum_output.stderr.is_empty(),
        format!(
            "index vacuum should not write JSON diagnostics to stderr: {}",
            String::from_utf8_lossy(&vacuum_output.stderr)
        ),
    )?;

    let vacuum_json = parse_stdout_json(&vacuum_output, "index vacuum")?;
    ensure(vacuum_json["schema"] == "ee.response.v1", "response schema")?;
    ensure(vacuum_json["success"] == true, "response success")?;
    ensure(
        vacuum_json["data"]["command"] == "index_vacuum",
        "vacuum command field",
    )?;
    ensure(vacuum_json["data"]["status"] == "missing", "missing status")?;
    ensure(vacuum_json["data"]["dryRun"] == true, "dry-run flag")?;
    ensure(
        vacuum_json["data"]["mutationAllowed"] == false,
        "mutation is disabled",
    )?;
    ensure(
        vacuum_json["data"]["degraded"][0]["code"] == "index_missing",
        "missing index degradation code",
    )?;
    ensure(events_path.is_file(), "E2E JSONL log exists")?;

    let normalized = normalize_vacuum_response(vacuum_json);
    let actual = serde_json::to_string_pretty(&normalized)
        .map_err(|error| format!("failed to serialize normalized response: {error}"))?
        + "\n";
    let expected = include_str!("golden/index-vacuum.snap");
    ensure(
        actual == expected,
        format!("index vacuum golden mismatch\nexpected:\n{expected}\nactual:\n{actual}"),
    )
}
