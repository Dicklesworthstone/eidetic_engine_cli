//! Public `ee workspace hygiene` Beads metadata freshness coverage.
//!
//! bd-1eq3l.4 requires command-level evidence that the hygiene report can
//! distinguish DB-only Beads changes that still need JSONL export from JSONL
//! changes that still need DB import.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use serde_json::Value;

type TestResult = Result<(), String>;

fn run_command(command: &mut Command, context: &str) -> Result<Output, String> {
    command
        .output()
        .map_err(|error| format!("{context}: failed to run command: {error}"))
}

fn ensure_success(output: &Output, context: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{context}: exit {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ))
    }
}

fn workspace_dir() -> Result<PathBuf, String> {
    let mut root = std::env::var("EE_E2E_TMPDIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    if root.starts_with("/Volumes/") {
        root = "/tmp".to_owned();
    }
    let temp = tempfile::Builder::new()
        .prefix("ee-workspace-hygiene-beads-")
        .tempdir_in(root)
        .map_err(|error| format!("tempdir: {error}"))?;
    Ok(temp.keep())
}

fn init_beads_workspace() -> Result<PathBuf, String> {
    let workspace = workspace_dir()?;
    ensure_success(
        &run_command(
            Command::new("git")
                .arg("init")
                .arg("-b")
                .arg("main")
                .current_dir(&workspace),
            "git init",
        )?,
        "git init",
    )?;

    let beads_dir = workspace.join(".beads");
    fs::create_dir_all(&beads_dir).map_err(|error| format!("create .beads: {error}"))?;
    fs::write(beads_dir.join(".gitignore"), "*.db\nlast-touched\n")
        .map_err(|error| format!("write .beads/.gitignore: {error}"))?;
    fs::write(beads_dir.join("issues.jsonl"), "{\"id\":\"bd-public\"}\n")
        .map_err(|error| format!("write .beads/issues.jsonl: {error}"))?;
    ensure_success(
        &run_command(
            Command::new("git")
                .arg("add")
                .arg(".beads/.gitignore")
                .arg(".beads/issues.jsonl")
                .current_dir(&workspace),
            "git add",
        )?,
        "git add",
    )?;
    ensure_success(
        &run_command(
            Command::new("git")
                .arg("-c")
                .arg("user.email=ee-test@example.invalid")
                .arg("-c")
                .arg("user.name=ee test")
                .arg("commit")
                .arg("-m")
                .arg("seed beads metadata")
                .current_dir(&workspace),
            "git commit",
        )?,
        "git commit",
    )?;
    Ok(workspace)
}

fn run_hygiene_json(workspace: &Path, context: &str) -> Result<Value, String> {
    let output = run_command(
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("workspace")
            .arg("hygiene")
            .arg("--agent-name")
            .arg("SapphireElk")
            .arg("--workspace")
            .arg(workspace)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY"),
        context,
    )?;
    ensure_success(&output, context)?;
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context}: stdout must be JSON: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn workspace_hygiene_reports_db_dirty_pending_flush_when_beads_db_is_newer() -> TestResult {
    let workspace = init_beads_workspace()?;
    thread::sleep(Duration::from_millis(1_100));
    fs::write(
        workspace.join(".beads").join("beads.db"),
        "db changed after export",
    )
    .map_err(|error| format!("write beads.db marker: {error}"))?;

    let value = run_hygiene_json(&workspace, "workspace hygiene db dirty pending flush")?;

    if value
        .pointer("/data/beadsState/classification")
        .and_then(Value::as_str)
        != Some("beads_db_dirty_pending_flush")
    {
        return Err(format!(
            "expected beads_db_dirty_pending_flush classification; response: {value}"
        ));
    }
    if value
        .pointer("/data/beadsState/metadataSignal")
        .and_then(Value::as_str)
        != Some("db_dirty_pending_flush")
    {
        return Err(format!(
            "expected db_dirty_pending_flush metadata signal; response: {value}"
        ));
    }
    if value
        .pointer("/data/beadsState/jsonlPosture/presentInDirtySet")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err(format!(
            "DB-only Beads change must not require dirty JSONL posture; response: {value}"
        ));
    }
    Ok(())
}

#[test]
fn workspace_hygiene_reports_external_import_pending_when_jsonl_is_newer() -> TestResult {
    let workspace = init_beads_workspace()?;
    fs::write(
        workspace.join(".beads").join("beads.db"),
        "db before external export",
    )
    .map_err(|error| format!("write beads.db marker: {error}"))?;
    thread::sleep(Duration::from_millis(1_100));
    fs::write(
        workspace.join(".beads").join("issues.jsonl"),
        "{\"id\":\"bd-public\"}\n{\"id\":\"bd-external\"}\n",
    )
    .map_err(|error| format!("write newer .beads/issues.jsonl: {error}"))?;

    let value = run_hygiene_json(&workspace, "workspace hygiene external import pending")?;

    if value
        .pointer("/data/beadsState/classification")
        .and_then(Value::as_str)
        != Some("beads_external_changes_pending_import")
    {
        return Err(format!(
            "expected beads_external_changes_pending_import classification; response: {value}"
        ));
    }
    if value
        .pointer("/data/beadsState/metadataSignal")
        .and_then(Value::as_str)
        != Some("external_changes_pending_import")
    {
        return Err(format!(
            "expected external_changes_pending_import metadata signal; response: {value}"
        ));
    }
    if value
        .pointer("/data/beadsState/jsonlPosture/presentInDirtySet")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(format!(
            "newer JSONL must appear in dirty posture; response: {value}"
        ));
    }
    Ok(())
}
