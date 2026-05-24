//! bd-2yiet: real-binary pin test for the `ee insights --json-stream` mutex
//! against the other output format flags.
//!
//! `handle_insights` (src/cli/mod.rs:13002) emits a `DomainError::Usage`
//! when `--json-stream` is combined with any of `--json`, `--robot`, or
//! `--format`. The message is:
//!
//!   `ee insights --json-stream` is mutually exclusive with other output
//!   format flags.
//!
//! and the repair is:
//!
//!   Run `ee insights --json-stream` without --json, --robot, or --format.
//!
//! This cross-flag runtime validator has no real-binary test coverage
//! today. This pin-test mirrors the
//! `tests/e2e_graph_centrality_missing_db.rs` harness shape and pins the
//! three rejection branches plus the success branch (the bare
//! `--json-stream` invocation must emit a parseable JSONL header line).

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
        .join("ee-insights-json-stream-mutex-pin")
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

fn assert_mutex_error_envelope(output: &Output, conflicting_flag: &str) -> TestResult {
    ensure(
        !output.status.success(),
        format!(
            "ee insights --json-stream combined with {conflicting_flag} must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout must be JSON: {error}"))?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains(
            "`ee insights --json-stream` is mutually exclusive with other output format flags.",
        ),
        format!("usage message must pin the json-stream mutex text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("Run `ee insights --json-stream` without --json, --robot, or --format."),
        format!("usage repair must enumerate the conflicting flags; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn insights_json_stream_with_json_flag_is_usage_error() -> TestResult {
    let workspace = unique_workspace("with-json")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "insights",
        "--json-stream",
    ])?;
    assert_mutex_error_envelope(&output, "--json")
}

#[test]
fn insights_json_stream_with_robot_flag_is_usage_error() -> TestResult {
    let workspace = unique_workspace("with-robot")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--robot",
        "insights",
        "--json-stream",
    ])?;
    assert_mutex_error_envelope(&output, "--robot")
}

#[test]
fn insights_json_stream_with_format_flag_is_usage_error() -> TestResult {
    let workspace = unique_workspace("with-format")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--format",
        "json",
        "insights",
        "--json-stream",
    ])?;
    assert_mutex_error_envelope(&output, "--format")
}

#[test]
fn insights_json_stream_alone_emits_parseable_jsonl_header() -> TestResult {
    let workspace = unique_workspace("alone")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "insights",
        "--json-stream",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee insights --json-stream alone must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout
        .lines()
        .next()
        .ok_or_else(|| format!("stdout must contain at least one line; got {stdout:?}"))?;
    let header: Value = serde_json::from_str(first_line)
        .map_err(|error| format!("first stdout line must be JSON: {error}; line={first_line}"))?;
    ensure(
        header["schema"].as_str() == Some("ee.insights.json_stream.header.v1"),
        format!(
            "first JSONL line must be the header with schema ee.insights.json_stream.header.v1; got {header}"
        ),
    )?;
    Ok(())
}
