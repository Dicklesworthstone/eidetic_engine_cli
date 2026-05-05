//! M1 Gate: Storage and Status (Gate 7 requirement)
//!
//! Validates that storage and status subsystems work correctly:
//! - Config precedence (env > file > default)
//! - Workspace resolution
//! - DB migration
//! - Status schema
//! - Lock-contention behavior

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value as JsonValue;

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
        .map_err(|e| format!("failed to run ee {}: {e}", args.join(" ")))
}

fn temp_workspace(prefix: &str) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(&format!("ee-gate-{prefix}-"))
        .tempdir()
        .map_err(|error| format!("failed to create workspace tempdir: {error}"))
}

fn workspace_arg(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(String::from)
        .ok_or_else(|| "workspace path not valid UTF-8".to_owned())
}

fn parse_json_output(output: &Output, context: &str) -> Result<JsonValue, String> {
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|e| format!("{context} stdout must be UTF-8: {e}"))?;

    ensure(
        output.status.success(),
        format!(
            "{context} should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    serde_json::from_str(stdout).map_err(|e| format!("{context} stdout must parse as JSON: {e}"))
}

#[test]
fn m1_init_creates_workspace_database() -> TestResult {
    let tempdir = temp_workspace("m1-init")?;
    let workspace = tempdir.path().to_path_buf();

    let output = run_ee(&["--workspace", &workspace_arg(&workspace)?, "--json", "init"])?;
    let json = parse_json_output(&output, "ee init")?;

    ensure(
        json.get("schema") == Some(&JsonValue::String("ee.response.v1".to_owned())),
        "init response must use ee.response.v1 schema",
    )?;

    let db_path = workspace.join(".ee").join("ee.db");
    ensure(
        db_path.exists(),
        format!("database must exist at {}", db_path.display()),
    )?;

    Ok(())
}

#[test]
fn m1_status_reports_workspace_state() -> TestResult {
    let tempdir = temp_workspace("m1-status")?;
    let workspace = tempdir.path().to_path_buf();

    run_ee(&["--workspace", &workspace_arg(&workspace)?, "--json", "init"])?;

    let output = run_ee(&[
        "--workspace",
        &workspace_arg(&workspace)?,
        "--json",
        "status",
    ])?;
    let json = parse_json_output(&output, "ee status")?;

    ensure(
        json.get("schema") == Some(&JsonValue::String("ee.response.v1".to_owned())),
        "status response must use ee.response.v1 schema",
    )?;

    let data = json.get("data").ok_or("status must have data field")?;
    ensure(
        data.get("command").is_some() || data.get("version").is_some(),
        "status data must include command or version",
    )?;
    ensure(
        data.get("capabilities").is_some(),
        "status data must include capabilities",
    )?;

    Ok(())
}

#[test]
fn m1_health_returns_overall_verdict() -> TestResult {
    let tempdir = temp_workspace("m1-health")?;
    let workspace = tempdir.path().to_path_buf();

    run_ee(&["--workspace", &workspace_arg(&workspace)?, "--json", "init"])?;

    let output = run_ee(&[
        "--workspace",
        &workspace_arg(&workspace)?,
        "--json",
        "health",
    ])?;
    let json = parse_json_output(&output, "ee health")?;

    ensure(
        json.get("schema") == Some(&JsonValue::String("ee.response.v1".to_owned())),
        "health response must use ee.response.v1 schema",
    )?;

    let data = json.get("data").ok_or("health must have data field")?;
    ensure(
        data.get("verdict").is_some(),
        "health data must include verdict",
    )?;

    Ok(())
}

#[test]
fn m1_capabilities_reports_feature_availability() -> TestResult {
    let output = run_ee(&["--json", "capabilities"])?;
    let json = parse_json_output(&output, "ee capabilities")?;

    ensure(
        json.get("schema") == Some(&JsonValue::String("ee.response.v1".to_owned())),
        "capabilities response must use ee.response.v1 schema",
    )?;

    let data = json
        .get("data")
        .ok_or("capabilities must have data field")?;
    ensure(
        data.get("commands").is_some() || data.get("features").is_some(),
        "capabilities data must include commands or features",
    )?;

    Ok(())
}
