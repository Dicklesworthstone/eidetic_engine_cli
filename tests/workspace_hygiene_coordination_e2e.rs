//! Public `ee workspace hygiene` Agent Mail coordination snapshot coverage.
//!
//! bd-1eq3l.5 requires command-level evidence that a read-only reservation
//! snapshot can block a dirty source path without live Agent Mail access.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        .prefix("ee-workspace-hygiene-coordination-")
        .tempdir_in(root)
        .map_err(|error| format!("tempdir: {error}"))?;
    Ok(temp.keep())
}

fn write_file(path: &Path, body: &str) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    fs::write(path, body).map_err(|error| format!("write {}: {error}", path.display()))
}

fn init_dirty_git_workspace() -> Result<PathBuf, String> {
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
    write_file(
        &workspace.join("src/core/workspace.rs"),
        "pub fn hygiene_fixture() -> &'static str { \"clean\" }\n",
    )?;
    ensure_success(
        &run_command(
            Command::new("git")
                .arg("add")
                .arg("src/core/workspace.rs")
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
                .arg("seed source file")
                .current_dir(&workspace),
            "git commit",
        )?,
        "git commit",
    )?;
    write_file(
        &workspace.join("src/core/workspace.rs"),
        "pub fn hygiene_fixture() -> &'static str { \"dirty\" }\n",
    )?;
    Ok(workspace)
}

fn run_hygiene_with_snapshot(workspace: &Path, snapshot_path: &Path) -> Result<Value, String> {
    let output = run_command(
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("workspace")
            .arg("hygiene")
            .arg("--agent-name")
            .arg("SapphireElk")
            .arg("--agent-mail-snapshot")
            .arg(snapshot_path)
            .arg("--workspace")
            .arg(workspace)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY"),
        "workspace hygiene coordination snapshot",
    )?;
    ensure_success(&output, "workspace hygiene coordination snapshot")?;
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "workspace hygiene stdout must be JSON: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn string_array_at<'a>(value: &'a Value, pointer: &str) -> Vec<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[test]
fn workspace_hygiene_snapshot_reservation_blocks_dirty_source_path() -> TestResult {
    let workspace = init_dirty_git_workspace()?;
    let snapshot_path = workspace.join("agent-mail-snapshot.json");
    write_file(
        &snapshot_path,
        r#"{
          "file_reservations": [
            {
              "path_pattern": "src/core/workspace.rs",
              "holder": "OtherAgent",
              "exclusive": true,
              "expires_at": "2099-01-01T00:00:00Z"
            }
          ],
          "active_agents": [
            {"name": "OtherAgent", "last_active_at": "2026-05-18T09:00:00Z"}
          ],
          "inbox": [],
          "threads": []
        }"#,
    )?;

    let value = run_hygiene_with_snapshot(&workspace, &snapshot_path)?;
    if value
        .pointer("/data/coordinationState/agentMailAvailable")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(format!("Agent Mail snapshot was not applied: {value}"));
    }
    let blocked_path = value
        .pointer("/data/coordinationState/blockedByCoordination/0/path")
        .and_then(Value::as_str);
    if blocked_path != Some("src/core/workspace.rs") {
        return Err(format!(
            "dirty source path was not coordination-blocked: {value}"
        ));
    }
    let holder = value
        .pointer("/data/coordinationState/blockedByCoordination/0/holderAgent")
        .and_then(Value::as_str);
    if holder != Some("OtherAgent") {
        return Err(format!("reservation holder was not surfaced: {value}"));
    }
    let degraded = string_array_at(&value, "/data/degraded");
    if degraded.contains(&"workspace_hygiene_agent_mail_unavailable") {
        return Err(format!(
            "snapshot-backed report must not emit Agent Mail unavailable: {value}"
        ));
    }
    let stage_paths = value
        .pointer("/data/stagingRecommendations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("paths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
        })
        .collect::<Vec<_>>();
    if stage_paths.contains(&"src/core/workspace.rs") {
        return Err(format!(
            "coordination-blocked path must not remain commit-ready: {value}"
        ));
    }
    Ok(())
}

#[test]
fn workspace_hygiene_empty_snapshot_distinguishes_no_reservations_from_unavailable() -> TestResult {
    let workspace = init_dirty_git_workspace()?;
    let snapshot_path = workspace.join("agent-mail-empty.json");
    write_file(
        &snapshot_path,
        r#"{
          "file_reservations": [],
          "active_agents": [],
          "inbox": [],
          "threads": []
        }"#,
    )?;

    let value = run_hygiene_with_snapshot(&workspace, &snapshot_path)?;
    if value
        .pointer("/data/coordinationState/agentMailAvailable")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(format!(
            "empty snapshot should still mark Agent Mail available: {value}"
        ));
    }
    let blocked = value
        .pointer("/data/coordinationState/blockedByCoordination")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(usize::MAX);
    if blocked != 0 {
        return Err(format!("empty snapshot should not block paths: {value}"));
    }
    let degraded = string_array_at(&value, "/data/degraded");
    if degraded.contains(&"workspace_hygiene_agent_mail_unavailable") {
        return Err(format!(
            "empty snapshot must not be confused with unavailable Agent Mail: {value}"
        ));
    }
    let stage_paths = value
        .pointer("/data/stagingRecommendations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("paths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
        })
        .collect::<Vec<_>>();
    if !stage_paths.contains(&"src/core/workspace.rs") {
        return Err(format!(
            "unreserved dirty source path should remain stageable: {value}"
        ));
    }
    Ok(())
}
