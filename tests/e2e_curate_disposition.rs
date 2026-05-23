//! bd-2p5ar: real-binary pin test for `ee curate disposition`
//! validators.
//!
//! `ee curate disposition` is the TTL-policy evaluator that promotes
//! / demotes / expires curation candidates over time — the
//! time-machine surface downstream pack assembly relies on for
//! stable recall. `handle_curate_disposition` plus
//! `run_curation_disposition` (src/core/curate.rs:3379) and
//! `parse_or_current_time` (src/core/curate.rs:7727) had no
//! end-to-end coverage. This pin test locks the three primary
//! surfaces:
//!
//! * Missing database -> Storage repair `"ee init --workspace ."`
//! * `--now garbage_timestamp` -> Usage `"invalid --now timestamp"`
//!   + repair `"ee curate disposition --help"`
//! * Happy path on empty workspace -> success envelope (no error,
//!   no candidates, no transitions)

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
        .join("ee-curate-disposition-pin")
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

fn run_disposition(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "curate",
        "disposition",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate disposition stdout must be JSON: {error}"))?;
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
fn curate_disposition_surfaces_storage_error_when_database_missing() -> TestResult {
    let workspace = unique_workspace("no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_disposition(&workspace_arg, &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate disposition without ee init must fail; stdout: {}",
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
fn curate_disposition_rejects_invalid_now_timestamp_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("bad-now")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_disposition(&workspace_arg, &["--now", "garbage_timestamp"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate disposition --now garbage_timestamp must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["invalid --now timestamp", "garbage_timestamp"],
        &["ee curate disposition --help"],
    )
}

#[test]
fn curate_disposition_happy_path_on_empty_workspace_succeeds() -> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_disposition(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "ee curate disposition on empty workspace must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["error"].is_null() || parsed.get("error").is_none(),
        format!("empty-workspace dispatch must not surface an error; got {parsed}"),
    )?;
    Ok(())
}
