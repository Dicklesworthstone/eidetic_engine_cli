//! Public `ee workspace hygiene` staging recommendation coverage.
//!
//! bd-1eq3l.2 requires deterministic, read-only commit-slice hints that keep
//! source, tests, docs, and golden updates separate while excluding risky paths.

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
        .prefix("ee-workspace-hygiene-recommendations-")
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

fn init_mixed_dirty_workspace() -> Result<PathBuf, String> {
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
    for (path, body) in [
        ("src/core/lib.rs", "pub fn blocked() -> bool { false }\n"),
        (
            "src/core/workspace.rs",
            "pub fn source() -> bool { false }\n",
        ),
        ("tests/workspace_hygiene.rs", "#[test]\nfn fixture() {}\n"),
        (
            "tests/fixtures/golden/workspace.json",
            "{\"state\":\"clean\"}\n",
        ),
        ("docs/guide.md", "# Guide\n"),
        ("local.rules", "tracked unknown\n"),
        ("drift-report.txt", "tracked scratch\n"),
    ] {
        write_file(&workspace.join(path), body)?;
    }
    ensure_success(
        &run_command(
            Command::new("git")
                .arg("add")
                .arg(".")
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
                .arg("seed mixed workspace")
                .current_dir(&workspace),
            "git commit",
        )?,
        "git commit",
    )?;

    for (path, body) in [
        ("src/core/lib.rs", "pub fn blocked() -> bool { true }\n"),
        (
            "src/core/workspace.rs",
            "pub fn source() -> bool { true }\n",
        ),
        (
            "tests/workspace_hygiene.rs",
            "#[test]\nfn fixture_changed() {}\n",
        ),
        (
            "tests/fixtures/golden/workspace.json",
            "{\"state\":\"dirty\"}\n",
        ),
        ("docs/guide.md", "# Guide\n\nUpdated.\n"),
        ("local.rules", "tracked unknown changed\n"),
        ("drift-report.txt", "tracked scratch changed\n"),
        ("Cargo.lock", "# generated lock fixture\n"),
    ] {
        write_file(&workspace.join(path), body)?;
    }
    Ok(workspace)
}

fn git_status(workspace: &Path) -> Result<String, String> {
    let output = run_command(
        Command::new("git")
            .arg("status")
            .arg("--porcelain=v2")
            .arg("--branch")
            .current_dir(workspace),
        "git status",
    )?;
    ensure_success(&output, "git status")?;
    String::from_utf8(output.stdout).map_err(|error| format!("git status utf8: {error}"))
}

fn run_hygiene_json(workspace: &Path, snapshot_path: &Path) -> Result<Value, String> {
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
        "workspace hygiene recommendations",
    )?;
    ensure_success(&output, "workspace hygiene recommendations")?;
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "workspace hygiene stdout must be JSON: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn array_strings_at<'a>(value: &'a Value, pointer: &str) -> Vec<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn staging_group<'a>(value: &'a Value, name: &str) -> Result<&'a Value, String> {
    value
        .pointer("/data/stagingRecommendations")
        .and_then(Value::as_array)
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| format!("missing staging group {name}: {value}"))
}

#[test]
fn workspace_hygiene_recommendations_are_grouped_read_only_and_explainable() -> TestResult {
    let workspace = init_mixed_dirty_workspace()?;
    let snapshot_path = workspace.join("agent-mail.json");
    write_file(
        &snapshot_path,
        r#"{
          "file_reservations": [
            {
              "path_pattern": "src/core/lib.rs",
              "holder": "OtherAgent",
              "exclusive": true,
              "expires_at": "2099-01-01T00:00:00Z"
            }
          ],
          "active_agents": [{"name": "OtherAgent"}],
          "inbox": [],
          "threads": []
        }"#,
    )?;
    let before = git_status(&workspace)?;
    let value = run_hygiene_json(&workspace, &snapshot_path)?;
    let after = git_status(&workspace)?;
    if before != after {
        return Err("workspace hygiene must not mutate git state".to_owned());
    }
    if value.pointer("/data/readOnly").and_then(Value::as_bool) != Some(true) {
        return Err(format!("workspace hygiene must be read-only: {value}"));
    }

    let group_names = value
        .pointer("/data/stagingRecommendations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if group_names != ["docs", "goldens", "source", "tests"] {
        return Err(format!(
            "unexpected staging groups {group_names:?}: {value}"
        ));
    }

    let source = staging_group(&value, "source")?;
    if array_strings_at(source, "/paths") != ["src/core/workspace.rs"] {
        return Err(format!(
            "source group must include only unblocked source path: {value}"
        ));
    }
    if source.get("pathCount").and_then(Value::as_u64) != Some(1) {
        return Err(format!("source group missing pathCount: {value}"));
    }
    if source.get("readOnly").and_then(Value::as_bool) != Some(true) {
        return Err(format!("source group must be read-only: {value}"));
    }
    if source.get("recommendation").and_then(Value::as_str)
        != Some("review_and_stage_as_one_logical_commit")
    {
        return Err(format!(
            "source group missing stable recommendation: {value}"
        ));
    }
    if !array_strings_at(source, "/reasons").contains(&"src_rust_source") {
        return Err(format!("source group missing reason code: {value}"));
    }
    if value.to_string().contains("git add") {
        return Err(format!("report must not emit staging commands: {value}"));
    }

    let do_not_commit = array_strings_at(&value, "/data/doNotCommit");
    if !do_not_commit.contains(&"drift-report.txt") || !do_not_commit.contains(&"Cargo.lock") {
        return Err(format!(
            "scratch/generated paths missing from doNotCommit: {value}"
        ));
    }
    let needs_review = array_strings_at(&value, "/data/needsHumanReview");
    if !needs_review.contains(&"local.rules") {
        return Err(format!(
            "tracked unknown path missing from needsHumanReview: {value}"
        ));
    }
    let blocked = value
        .pointer("/data/coordinationState/blockedByCoordination/0/path")
        .and_then(Value::as_str);
    if blocked != Some("src/core/lib.rs") {
        return Err(format!(
            "blocked source path missing from coordination state: {value}"
        ));
    }
    Ok(())
}
