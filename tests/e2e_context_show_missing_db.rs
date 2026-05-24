//! bd-15ilq: real-binary pin test for the `Database not found` Storage error
//! surface in `ee context-show`.
//!
//! `handle_context_show` (src/cli/mod.rs:28138) checks `database_path.exists()`
//! before opening and emits `DomainError::Storage { message: "Database not
//! found: <path>", repair: Some("ee init --workspace .") }` when missing.
//! The existing contract test
//! `tests/contracts/context_show_persisted_pack.rs:296` covers the NotFound
//! branch (unknown pack id against a migrated DB) but not the missing-db
//! Storage branch. Sibling pin-tests for snapshot refresh (bd-291ho),
//! neighborhood (bd-13b2n), centrality (bd-2ajxu), and why (e2e_why.rs)
//! already pin the parallel Storage branch in those handlers;
//! `ee context-show` is the remaining core read-only command with this
//! branch unpinned against the real binary.
//!
//! This pin-test mirrors the
//! `tests/e2e_graph_centrality_missing_db.rs` harness shape.

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
        .join("ee-context-show-missing-db-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

#[test]
fn context_show_without_init_surfaces_database_missing_storage_error() -> TestResult {
    let workspace = unique_workspace("no-init")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    // Intentionally skip `ee init` so .ee/ee.db does not exist.

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "context-show",
        "pack_anything",
    ])?;
    ensure(
        !output.status.success(),
        format!(
            "context-show on uninitialized workspace must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout must be JSON: {error}"))?;
    ensure(
        parsed["schema"].as_str() == Some("ee.error.v2"),
        format!("error envelope schema must be ee.error.v2; got {parsed}"),
    )?;
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
