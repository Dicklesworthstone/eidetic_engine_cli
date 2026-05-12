//! L0 phase 1 contract test (eidetic_engine_cli bd-17c65.12.1).
//!
//! Documents the top-level `ee migrate` surface added in this bead:
//!
//! - `ee migrate status` emits an ee.response.v1 envelope with
//!   schemaVersion / latestCompiledSchemaVersion / pendingCount /
//!   needsMigration / upToDate. Exit code is 0 when up-to-date and
//!   8 (MigrationRequired) when pending migrations exist.
//! - `ee migrate run` applies any unapplied migrations idempotently
//!   and emits an envelope listing applied + skipped version numbers.
//! - `ee migrate run --dry-run` reports what WOULD be applied without
//!   touching the database.
//!
//! The test invokes the real binary against a real workspace so the
//! exit codes and the JSON envelope are exercised end-to-end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary())
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_stdout(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

fn init_workspace(workspace: &Path) -> Result<(), String> {
    let init = run_ee(&[
        "--workspace",
        workspace.to_str().unwrap(),
        "init",
        "--json",
    ])?;
    if init.status.code() != Some(0) {
        return Err(format!(
            "init failed: exit {:?}, stderr {}",
            init.status.code(),
            String::from_utf8_lossy(&init.stderr)
        ));
    }
    Ok(())
}

#[test]
fn migrate_status_on_fresh_workspace_reports_up_to_date() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    init_workspace(tmpdir.path())?;

    let status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    if status.status.code() != Some(0) {
        return Err(format!(
            "migrate status should exit 0 on fresh workspace, got {:?}; stderr {}",
            status.status.code(),
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    let json = parse_stdout(&status, "migrate status")?;
    let schema = json
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing schema".to_string())?;
    if schema != "ee.response.v1" {
        return Err(format!("schema mismatch: got {schema}"));
    }
    let up_to_date = json
        .pointer("/data/upToDate")
        .and_then(Value::as_bool)
        .ok_or_else(|| "missing data.upToDate".to_string())?;
    if !up_to_date {
        return Err("fresh workspace should report upToDate=true".to_string());
    }
    let pending = json
        .pointer("/data/pendingCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing pendingCount".to_string())?;
    if pending != 0 {
        return Err(format!("fresh workspace should have 0 pending, got {pending}"));
    }
    Ok(())
}

#[test]
fn migrate_run_dry_run_does_not_mutate_database() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    init_workspace(tmpdir.path())?;

    let pre_status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    let pre_json = parse_stdout(&pre_status, "pre status")?;
    let pre_schema_version = pre_json.pointer("/data/schemaVersion").cloned();

    let dry = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "run",
        "--dry-run",
        "--json",
    ])?;
    if dry.status.code() != Some(0) {
        return Err(format!(
            "migrate run --dry-run should exit 0, got {:?}",
            dry.status.code()
        ));
    }
    let dry_json = parse_stdout(&dry, "migrate run --dry-run")?;
    let dry_run_flag = dry_json
        .pointer("/data/dryRun")
        .and_then(Value::as_bool)
        .ok_or_else(|| "missing data.dryRun".to_string())?;
    if !dry_run_flag {
        return Err("data.dryRun must be true for --dry-run".to_string());
    }

    // Schema version unchanged after --dry-run.
    let post_status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    let post_json = parse_stdout(&post_status, "post status")?;
    let post_schema_version = post_json.pointer("/data/schemaVersion").cloned();
    if pre_schema_version != post_schema_version {
        return Err(format!(
            "schemaVersion changed across --dry-run: {pre_schema_version:?} → {post_schema_version:?}"
        ));
    }
    Ok(())
}

#[test]
fn migrate_run_is_idempotent_on_up_to_date_workspace() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    init_workspace(tmpdir.path())?;

    let first = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "run",
        "--json",
    ])?;
    if first.status.code() != Some(0) {
        return Err(format!(
            "first migrate run exit {:?}",
            first.status.code()
        ));
    }
    let first_json = parse_stdout(&first, "first run")?;
    let applied = first_json
        .pointer("/data/appliedCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing appliedCount".to_string())?;
    if applied != 0 {
        return Err(format!(
            "fresh workspace should have no migrations to apply, got {applied}"
        ));
    }

    // Second run: must also exit 0 and apply nothing.
    let second = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "run",
        "--json",
    ])?;
    if second.status.code() != Some(0) {
        return Err(format!(
            "second migrate run exit {:?}",
            second.status.code()
        ));
    }
    let second_json = parse_stdout(&second, "second run")?;
    let second_applied = second_json
        .pointer("/data/appliedCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing appliedCount on second run".to_string())?;
    if second_applied != 0 {
        return Err(format!(
            "second migrate run on up-to-date workspace should apply nothing, got {second_applied}"
        ));
    }
    Ok(())
}

#[test]
fn migrate_status_on_missing_database_returns_storage_error() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    // Do NOT init — workspace has no DB file.
    let status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    // Storage error exit code is 3 per AGENTS.md.
    let code = status.status.code();
    if code == Some(0) {
        return Err(format!(
            "migrate status on missing DB should not exit 0, got {code:?}"
        ));
    }
    let stdout = String::from_utf8_lossy(&status.stdout);
    if !stdout.contains("\"schema\":\"ee.error.v1\"") {
        return Err(format!(
            "expected ee.error.v1 envelope on missing DB, got: {stdout}"
        ));
    }
    Ok(())
}
