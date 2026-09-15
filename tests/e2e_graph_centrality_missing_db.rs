//! bd-2ajxu: real-binary missing-store errors for graph centrality reads
//! and refreshes. Both commands must report the addressed database,
//! recommend checking the address before conditional initialization,
//! and leave the absent store untouched.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
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

fn assert_missing_db_error(parsed: &Value, workspace: &Path) -> TestResult {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = workspace.join(".ee/ee.db");
    let error = &parsed["error"];
    ensure(
        parsed["schema"] == "ee.error.v2" && error["code"] == "workspace_store_missing",
        format!("response must retain the canonical missing-store error; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message == format!("Database not found at {}", database.display())
            && error["details"]["addressedStorePath"] == database.to_string_lossy().as_ref(),
        format!("missing-store error must identify the exact database; got {error}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.starts_with("Re-check --workspace addressing")
            && repair.ends_with(&format!(
                "Only if you intended to create a NEW store here: ee init --workspace {}",
                workspace.display()
            )),
        format!("repair must check addressing before conditional exact-path init; got {repair}"),
    )?;
    ensure(
        !workspace.join(".ee").exists(),
        "missing-store reads must not initialize a store",
    )
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
        output.status.code() == Some(10),
        format!(
            "graph centrality on uninitialized workspace must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed = parse_error_response(&output)?;
    assert_missing_db_error(&parsed, &workspace)
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
        output.status.code() == Some(10),
        format!(
            "graph centrality-refresh on uninitialized workspace must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed = parse_error_response(&output)?;
    assert_missing_db_error(&parsed, &workspace)
}
