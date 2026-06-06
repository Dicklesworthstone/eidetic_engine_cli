//! bd-3jvp0: real-binary pin test for `ee memory revise` error
//! surfaces.
//!
//! `ee memory revise` creates a revision (forming the revision DAG
//! that `graph_neighborhood_smoke.rs` exercises via
//! `impactAnalysis` / `revisionLineage`). `handle_memory_revise`
//! (src/cli/mod.rs:27381) has five distinct error surfaces no test
//! pins end-to-end against the real binary:
//!
//! * Missing database (no `ee init`) -> Storage repair
//!   `"ee init --workspace ."`
//! * `--confidence garbage` -> Usage from
//!   `parse_memory_revise_confidence`: `"Invalid confidence
//!   `garbage`: expected a finite number from 0.0 to 1.0"` +
//!   `"Use --confidence 0.8"`
//! * Non-existent memory id -> NotFound `"memory"` + `"ee memory
//!   list"` via `memory_revise_error_to_domain`
//! * Revise with no field flags (only `--reason`) -> Usage
//!   `"No memory revision changes were specified."` + repair listing
//!   every revisable field
//! * Revise a tombstoned memory -> PolicyDenied `"Cannot revise
//!   tombstoned memory."` + `"ee memory show"`

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
        .join("ee-memory-revise-errors-pin")
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

fn expire_memory(workspace_arg: &str, memory_id: &str) -> TestResult {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "memory",
        "expire",
        memory_id,
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee memory expire {memory_id} must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn run_revise(
    workspace_arg: &str,
    memory_id: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "memory", "revise"];
    args.push(memory_id);
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory revise stdout must be JSON: {error}"))?;
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
fn memory_revise_surfaces_storage_error_when_database_missing() -> TestResult {
    // Skip ee init so the database-existence guard fires before any
    // revision work.
    let workspace = unique_workspace("usage-no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_revise(&workspace_arg, "mem_any", &["--content", "x"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory revise without ee init must fail; stdout: {}",
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
fn memory_revise_rejects_invalid_confidence_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("usage-bad-confidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test revise bad-confidence target.")?;

    let (output, parsed) = run_revise(
        &workspace_arg,
        &memory_id,
        &["--confidence", "garbage", "--content", "irrelevant"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory revise --confidence garbage must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &[
            "Invalid confidence",
            "garbage",
            "expected a finite number from 0.0 to 1.0",
        ],
        &["Use --confidence 0.8"],
    )
}

#[test]
fn memory_revise_returns_not_found_for_unknown_memory_id() -> TestResult {
    let workspace = unique_workspace("not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_revise(
        &workspace_arg,
        "mem_does_not_exist_in_workspace",
        &["--content", "anything"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory revise on unknown memory must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory list"),
        format!("not-found repair must point at `ee memory list`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn memory_revise_rejects_no_changes_with_field_list_repair() -> TestResult {
    let workspace = unique_workspace("no-changes")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test revise no-changes target.")?;

    // Set only --reason (the default is "update", but passing it
    // explicitly mirrors how a careless agent might omit every
    // revisable field). --dry-run is included so we exercise the
    // validation branch rather than any "writes unavailable" branch.
    let (output, parsed) = run_revise(
        &workspace_arg,
        &memory_id,
        &["--reason", "correction", "--dry-run"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory revise with no field flags must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["No memory revision changes were specified."],
        &[
            "--content",
            "--level",
            "--kind",
            "--confidence",
            "--tag",
            "--provenance-uri",
        ],
    )
}

#[test]
fn memory_revise_rejects_tombstoned_memory_with_policy_repair() -> TestResult {
    let workspace = unique_workspace("tombstoned")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test revise tombstoned target.")?;
    expire_memory(&workspace_arg, &memory_id)?;

    let (output, parsed) = run_revise(
        &workspace_arg,
        &memory_id,
        &["--content", "anything", "--dry-run"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory revise on tombstoned memory must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Cannot revise tombstoned memory."],
        &["ee memory show"],
    )
}
