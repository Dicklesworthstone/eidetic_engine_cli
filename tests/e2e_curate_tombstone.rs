//! bd-bivsl: real-binary pin test for `ee curate tombstone`
//! behaviors.
//!
//! `ee curate tombstone` is the audited graph-node removal operation
//! — it sets `memory.tombstoned_at` so subsequent graph queries
//! exclude the node by default (the `include_tombstoned` filter in
//! `graph_filter_tombstoned_links` and elsewhere). `run_curate_tombstone`
//! (src/core/curate.rs:3724) and `handle_curate_tombstone`
//! (src/cli/mod.rs:36971) had no end-to-end coverage. This pin test
//! locks the four primary surfaces:
//!
//! * Missing database -> Storage repair `"ee init --workspace ."`
//! * Non-existent memory id -> NotFound `"memory"` + repair
//!   `"ee memory list --json"`
//! * Happy-path tombstone -> success report with `persisted=true`,
//!   `dryRun=false`, `auditId` present, schema `ee.curate.tombstone.v1`,
//!   command `"curate tombstone"`
//! * Re-tombstone an already-tombstoned memory -> Usage
//!   `"Memory <id> is already tombstoned."` + repair
//!   `"ee memory list --json"`

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-curate-tombstone-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn run_tombstone(
    workspace_arg: &str,
    memory_id: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "curate",
        "tombstone",
        memory_id,
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate tombstone stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_error_with_repair(
    parsed: &Value,
    message_needles: &[&str],
    repair_needles: &[&str],
) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    for needle in message_needles {
        ensure(
            message.contains(needle),
            format!("message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    for needle in repair_needles {
        ensure(
            repair.contains(needle),
            format!("repair must contain {needle:?}; got {repair}"),
        )?;
    }
    Ok(())
}

#[test]
fn curate_tombstone_surfaces_storage_error_when_database_missing() -> TestResult {
    let workspace = unique_workspace("no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_tombstone(&workspace_arg, "mem_any", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate tombstone without ee init must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Database not found at"],
        &["ee init --workspace ."],
    )
}

#[test]
fn curate_tombstone_returns_not_found_for_unknown_memory_id() -> TestResult {
    let workspace = unique_workspace("not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_tombstone(&workspace_arg, "mem_does_not_exist_in_workspace", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate tombstone on unknown memory must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    ensure(
        error["code"].as_str() == Some("not_found"),
        format!("unknown memory must return not_found; got {error}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("memory"),
        format!("not-found message must reference memory; got {message}"),
    )?;
    ensure(
        error["details"]["resource"].as_str() == Some("memory"),
        format!("not-found details.resource must be memory; got {error}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory list --json"),
        format!("not-found repair must point at `ee memory list --json`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn curate_tombstone_happy_path_returns_persisted_envelope_with_audit() -> TestResult {
    let workspace = unique_workspace("happy")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test tombstone happy-path target.")?;

    let (output, parsed) = run_tombstone(&workspace_arg, &memory_id, &[])?;
    ensure(
        output.status.success(),
        format!(
            "ee curate tombstone on active memory must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    // The tombstone handler emits the bare report (no
    // schema/success/data envelope wrapper). camelCase fields come
    // from `#[serde(rename_all = "camelCase")]` on CurateTombstoneReport.
    ensure(
        parsed["schema"].as_str() == Some("ee.curate.tombstone.v1"),
        format!("schema must be ee.curate.tombstone.v1; got {parsed}"),
    )?;
    ensure(
        parsed["command"].as_str() == Some("curate tombstone"),
        format!("command must be `curate tombstone`; got {parsed}"),
    )?;
    ensure(
        parsed["memoryId"].as_str() == Some(memory_id.as_str()),
        format!("memoryId must echo the requested id; got {parsed}"),
    )?;
    ensure(
        parsed["dryRun"] == Value::Bool(false),
        format!("dryRun must be false on happy path; got {parsed}"),
    )?;
    ensure(
        parsed["persisted"] == Value::Bool(true),
        format!("persisted must be true on happy path; got {parsed}"),
    )?;
    ensure(
        parsed["auditId"].is_string(),
        format!("auditId must be a string on happy path; got {parsed}"),
    )?;
    ensure(
        parsed["tombstonedAt"].is_string(),
        format!("tombstonedAt must be set on happy path; got {parsed}"),
    )?;
    Ok(())
}

#[test]
fn curate_tombstone_rejects_re_tombstone_already_tombstoned_memory() -> TestResult {
    let workspace = unique_workspace("already-tombstoned")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(
        &workspace_arg,
        "Pin-test tombstone already-tombstoned target.",
    )?;

    // First tombstone: must succeed.
    let (first, _) = run_tombstone(&workspace_arg, &memory_id, &[])?;
    ensure(
        first.status.success(),
        "first tombstone must succeed".to_string(),
    )?;

    // Second tombstone on the same memory: must fail with the
    // documented "already tombstoned" Usage error and point at the
    // memory list as the next action.
    let (second_output, second_parsed) = run_tombstone(&workspace_arg, &memory_id, &[])?;
    ensure(
        !second_output.status.success(),
        format!(
            "re-tombstone of already-tombstoned memory must fail; stdout: {}",
            String::from_utf8_lossy(&second_output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &second_parsed,
        &["is already tombstoned"],
        &["ee memory list --json"],
    )
}
