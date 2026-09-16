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

fn u64_at(value: &Value, pointer: &str) -> u64 {
    value.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

fn bool_at(value: &Value, pointer: &str) -> bool {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn staging_group_omitted_path_count(value: &Value) -> u64 {
    value
        .pointer("/data/outputTruncation/stagingGroups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("omittedPathCount").and_then(Value::as_u64))
        .sum()
}

fn emit_hygiene_e2e_event(
    test_name: &str,
    value: Option<&Value>,
    elapsed_ms: u128,
    first_failure_diagnosis: Option<&str>,
) {
    let path_count = value
        .map(|report| u64_at(report, "/data/dirtyPathCount"))
        .unwrap_or(0);
    let git_path_count = value
        .map(|report| u64_at(report, "/data/gitSummary/dirtyPathCount"))
        .unwrap_or(0);
    let truncated = value
        .map(|report| bool_at(report, "/data/outputTruncation/truncated"))
        .unwrap_or(false);
    let omitted_path_classifications = value
        .map(|report| u64_at(report, "/data/outputTruncation/omittedPathClassifications"))
        .unwrap_or(0);
    let omitted_do_not_commit = value
        .map(|report| u64_at(report, "/data/outputTruncation/omittedDoNotCommit"))
        .unwrap_or(0);
    let omitted_needs_human_review = value
        .map(|report| u64_at(report, "/data/outputTruncation/omittedNeedsHumanReview"))
        .unwrap_or(0);
    let omitted_staging_group_paths = value.map(staging_group_omitted_path_count).unwrap_or(0);

    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "beadId": "bd-1eq3l.13",
            "surface": "workspace_hygiene",
            "phase": "large_repo_e2e",
            "testName": test_name,
            "pathCount": path_count,
            "gitPathCount": git_path_count,
            "truncated": truncated,
            "omittedPathClassifications": omitted_path_classifications,
            "omittedDoNotCommit": omitted_do_not_commit,
            "omittedNeedsHumanReview": omitted_needs_human_review,
            "omittedStagingGroupPaths": omitted_staging_group_paths,
            "elapsedMs": u64::try_from(elapsed_ms).unwrap_or(u64::MAX),
            "firstFailureDiagnosis": first_failure_diagnosis,
        })
    );
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
fn workspace_hygiene_large_dirty_workspace_logs_truncation_metrics() -> TestResult {
    let workspace = workspace_dir()?;
    ensure_success(
        &run_command(
            Command::new("git")
                .arg("init")
                .arg("-b")
                .arg("main")
                .current_dir(&workspace),
            "git init large hygiene workspace",
        )?,
        "git init large hygiene workspace",
    )?;

    let dirty_dir = workspace.join("src/perf");
    fs::create_dir_all(&dirty_dir)
        .map_err(|error| format!("create {}: {error}", dirty_dir.display()))?;
    for index in 0..10_010 {
        fs::write(
            dirty_dir.join(format!("file_{index:05}.rs")),
            b"pub fn generated_fixture() {}\n",
        )
        .map_err(|error| format!("write large hygiene fixture {index}: {error}"))?;
    }

    let snapshot_path = workspace.with_extension("agent-mail-empty.json");
    write_file(
        &snapshot_path,
        r#"{
          "file_reservations": [],
          "active_agents": [],
          "inbox": [],
          "threads": []
        }"#,
    )?;

    let started = std::time::Instant::now();
    let value = match run_hygiene_with_snapshot(&workspace, &snapshot_path) {
        Ok(value) => value,
        Err(error) => {
            emit_hygiene_e2e_event(
                "workspace_hygiene_large_dirty_workspace_logs_truncation_metrics",
                None,
                started.elapsed().as_millis(),
                Some("workspace_hygiene_command_failed"),
            );
            return Err(error);
        }
    };
    let elapsed_ms = started.elapsed().as_millis();
    emit_hygiene_e2e_event(
        "workspace_hygiene_large_dirty_workspace_logs_truncation_metrics",
        Some(&value),
        elapsed_ms,
        None,
    );

    let path_count = u64_at(&value, "/data/dirtyPathCount");
    if path_count < 10_010 {
        return Err(format!("large hygiene fixture path count too low: {value}"));
    }
    if !bool_at(&value, "/data/outputTruncation/truncated") {
        return Err(format!("large hygiene fixture did not truncate: {value}"));
    }
    if u64_at(&value, "/data/outputTruncation/omittedPathClassifications") == 0 {
        return Err(format!(
            "large hygiene fixture missed omitted classification count: {value}"
        ));
    }
    let degraded = string_array_at(&value, "/data/degraded");
    if !degraded.contains(&"workspace_hygiene_output_truncated") {
        return Err(format!(
            "large hygiene fixture did not emit truncation degradation: {value}"
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
