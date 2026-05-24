//! bd-2ajxu: real-binary pin test for the `Database not found` Storage
//! error surface in `ee graph centrality` and `ee graph centrality-refresh`.
//!
//! `handle_graph_centrality_read` (src/cli/mod.rs:25467) and
//! `handle_graph_centrality_refresh` (src/cli/mod.rs:25251) both check
//! `database_path.exists()` and emit `DomainError::Storage { message:
//! "Database not found at <path>", repair: Some("ee init --workspace .") }`
//! when missing. tests/e2e_graph_centrality.rs covers the read flow with a
//! successful init plus refresh dry-run, but the missing-db Storage branch
//! has no real-binary assertions for either command. Sibling pin-tests
//! `tests/e2e_graph_snapshot_refresh_validators.rs` (bd-291ho) and
//! `tests/e2e_graph_neighborhood_validators.rs` (bd-13b2n) already pin the
//! parallel Storage error in those handlers; centrality-read and
//! centrality-refresh are the remaining graph commands with this branch
//! unpinned.
//!
//! This pin-test mirrors the snapshot-refresh validator harness shape and
//! pins both commands' missing-db Storage error and repair.

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
        .join("ee-graph-centrality-missing-db-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn parse_error_response(output: &Output) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| format!("stdout must be JSON: {error}"))
}

fn assert_missing_db_error(parsed: &Value) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains("Database not found"),
        format!("storage message must pin the Database not found guard; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee init --workspace ."),
        format!("storage repair must point at `ee init --workspace .`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_without_init_surfaces_database_missing_storage_error() -> TestResult {
    let workspace = unique_workspace("read-no-init")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    // Intentionally skip `ee init` so .ee/ee.db does not exist.

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality",
    ])?;
    ensure(
        !output.status.success(),
        format!(
            "graph centrality on uninitialized workspace must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed = parse_error_response(&output)?;
    assert_missing_db_error(&parsed)
}

#[test]
fn graph_centrality_refresh_without_init_surfaces_database_missing_storage_error() -> TestResult {
    let workspace = unique_workspace("refresh-no-init")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    // Intentionally skip `ee init` so .ee/ee.db does not exist.

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality-refresh",
        "--dry-run",
    ])?;
    ensure(
        !output.status.success(),
        format!(
            "graph centrality-refresh on uninitialized workspace must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed = parse_error_response(&output)?;
    assert_missing_db_error(&parsed)
}
