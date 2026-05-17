//! Contract coverage for the workspace-hygiene Git snapshot provider.
//!
//! The provider is allowed to read `git rev-parse`, `git status
//! --porcelain=v2`, and symlink-safe filesystem metadata. It must not stage,
//! delete, rewrite, or otherwise mutate the repository while collecting the
//! dirty-path snapshot.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use ee::core::swarm_brief::{
    SwarmBriefCommandError, SystemSwarmBriefCommandRunner, WorkspaceGitSnapshotOptions,
    WorkspaceGitStatusEntry, collect_workspace_git_snapshot,
    parse_workspace_git_status_porcelain_v2,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn run_git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("spawn git {args:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("git stdout utf8: {error}"))
}

fn run_git_expect_failure(workspace: &Path, args: &[&str]) -> TestResult {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("spawn git {args:?}: {error}"))?;
    if output.status.success() {
        return Err(format!(
            "git {args:?} should fail for this scenario\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn write_file(path: &Path, content: impl AsRef<[u8]>) -> TestResult {
    fs::write(path, content).map_err(|error| format!("write {}: {error}", path.display()))
}

fn file_state_digest(workspace: &Path) -> Result<Vec<(String, String)>, String> {
    fn visit(root: &Path, path: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("metadata {}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("strip_prefix {}: {error}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ".git" || relative.starts_with(".git/") {
            return Ok(());
        }
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)
                .map_err(|error| format!("read_dir {}: {error}", path.display()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("read_dir entry {}: {error}", path.display()))?;
            children.sort_by_key(|entry| entry.path());
            for child in children {
                visit(root, &child.path(), out)?;
            }
            return Ok(());
        }
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(path)
                .map_err(|error| format!("read_link {}: {error}", path.display()))?;
            out.push((relative, format!("symlink:{}", target.to_string_lossy())));
            return Ok(());
        }
        if metadata.is_file() {
            let bytes =
                fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
            out.push((relative, format!("file:{}", blake3::hash(&bytes).to_hex())));
        }
        Ok(())
    }

    let mut out = Vec::new();
    visit(workspace, workspace, &mut out)?;
    out.sort();
    Ok(out)
}

fn status_digest(workspace: &Path) -> Result<String, String> {
    run_git(
        workspace,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
        ],
    )
}

fn artifact_summary(name: &str, content: &str) -> Value {
    json!({
        "name": name,
        "bytes": content.len(),
        "blake3": blake3::hash(content.as_bytes()).to_hex().to_string(),
    })
}

fn mutation_hash(status: &str, files: &[(String, String)]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(status.as_bytes());
    for (path, state) in files {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(state.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn read_only_e2e_log(
    scenario: &str,
    workspace: &Path,
    elapsed_ms: u128,
    before_status: &str,
    after_status: &str,
    before_files: &[(String, String)],
    after_files: &[(String, String)],
) -> Value {
    let before_mutation_hash = mutation_hash(before_status, before_files);
    let after_mutation_hash = mutation_hash(after_status, after_files);
    let first_failure_diagnosis = if before_mutation_hash == after_mutation_hash {
        Value::Null
    } else {
        json!("workspace git snapshot provider mutated git status or files")
    };

    json!({
        "schema": "ee.workspace_hygiene.git_snapshot_readonly_log.v1",
        "command": "collect_workspace_git_snapshot",
        "workspace": workspace.display().to_string(),
        "scenario": scenario,
        "elapsedMs": elapsed_ms,
        "exitCode": 0,
        "stdoutArtifacts": [
            artifact_summary("git_status_before.stdout", before_status),
            artifact_summary("git_status_after.stdout", after_status),
        ],
        "stderrArtifacts": [
            artifact_summary("collect_workspace_git_snapshot.stderr", ""),
        ],
        "beforeMutationHash": before_mutation_hash,
        "afterMutationHash": after_mutation_hash,
        "firstFailureDiagnosis": first_failure_diagnosis,
    })
}

fn assert_read_only_e2e_log(log: &Value, scenario: &str) -> TestResult {
    assert_eq!(
        log["schema"],
        "ee.workspace_hygiene.git_snapshot_readonly_log.v1"
    );
    assert_eq!(log["command"], "collect_workspace_git_snapshot");
    assert_eq!(log["scenario"], scenario);
    assert_eq!(log["exitCode"], 0);
    assert!(
        log["workspace"]
            .as_str()
            .is_some_and(|path| !path.is_empty())
    );
    assert!(log["elapsedMs"].as_u64().is_some());
    assert_eq!(log["firstFailureDiagnosis"], Value::Null);
    assert_eq!(log["beforeMutationHash"], log["afterMutationHash"]);
    assert!(log["stdoutArtifacts"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["name"] == "git_status_before.stdout")
    }));
    assert!(log["stderrArtifacts"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["name"] == "collect_workspace_git_snapshot.stderr")
    }));
    Ok(())
}

fn entry_by_path<'a>(
    entries: &'a [WorkspaceGitStatusEntry],
    path: &str,
) -> Result<&'a WorkspaceGitStatusEntry, String> {
    entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| format!("missing git snapshot entry for {path}: {entries:#?}"))
}

#[test]
fn workspace_git_porcelain_v2_parser_preserves_copied_paths() -> TestResult {
    let entries = parse_workspace_git_status_porcelain_v2(
        "2 C. N... 100644 100644 100644 abc def C100 src/copy.rs\tsrc/source.rs\n",
    );

    assert_eq!(
        entries.len(),
        1,
        "expected one copied-path entry: {entries:#?}"
    );
    let copied = entry_by_path(&entries, "src/copy.rs")?;
    assert_eq!(copied.entry_kind, "renamed_or_copied");
    assert_eq!(copied.original_path.as_deref(), Some("src/source.rs"));
    assert_eq!(copied.staged, "C");
    assert_eq!(copied.unstaged, ".");

    Ok(())
}

#[test]
fn workspace_git_snapshot_provider_is_read_only_for_clean_repo() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("ee-workspace-git-clean-readonly-")
        .tempdir()
        .map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path();

    run_git(workspace, &["init", "-q", "-b", "main"])?;
    run_git(
        workspace,
        &["config", "user.email", "ee-test@example.invalid"],
    )?;
    run_git(workspace, &["config", "user.name", "ee test"])?;

    write_file(&workspace.join("README.md"), "# clean repo\n")?;
    run_git(workspace, &["add", "README.md"])?;
    run_git(workspace, &["commit", "-q", "-m", "seed"])?;

    let before_status = status_digest(workspace)?;
    let before_files = file_state_digest(workspace)?;

    let options = WorkspaceGitSnapshotOptions::for_workspace(workspace);
    let started = Instant::now();
    let snapshot = collect_workspace_git_snapshot(&options, &SystemSwarmBriefCommandRunner)
        .map_err(|error| format!("collect workspace git snapshot: {error:?}"))?;
    let elapsed_ms = started.elapsed().as_millis();

    let after_status = status_digest(workspace)?;
    let after_files = file_state_digest(workspace)?;
    let log = read_only_e2e_log(
        "clean_repo",
        workspace,
        elapsed_ms,
        &before_status,
        &after_status,
        &before_files,
        &after_files,
    );
    assert_read_only_e2e_log(&log, "clean_repo")?;
    assert_eq!(
        after_status, before_status,
        "provider must not change clean git status"
    );
    assert_eq!(
        after_files, before_files,
        "provider must not change clean repo files"
    );
    assert!(
        snapshot.entries.is_empty(),
        "clean repository should not report dirty snapshot entries: {:#?}",
        snapshot.entries
    );

    Ok(())
}

#[test]
fn workspace_git_snapshot_provider_uses_repo_root_from_nested_workspace() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("ee-workspace-git-nested-readonly-")
        .tempdir()
        .map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path();

    run_git(workspace, &["init", "-q", "-b", "main"])?;
    run_git(
        workspace,
        &["config", "user.email", "ee-test@example.invalid"],
    )?;
    run_git(workspace, &["config", "user.name", "ee test"])?;

    fs::create_dir_all(workspace.join("nested/tool"))
        .map_err(|error| format!("create nested/tool: {error}"))?;
    fs::create_dir_all(workspace.join("src")).map_err(|error| format!("create src: {error}"))?;
    write_file(
        &workspace.join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )?;
    write_file(
        &workspace.join("nested/tool/config.toml"),
        "enabled = true\n",
    )?;
    run_git(workspace, &["add", "src/lib.rs", "nested/tool/config.toml"])?;
    run_git(workspace, &["commit", "-q", "-m", "seed"])?;

    write_file(
        &workspace.join("src/lib.rs"),
        "pub fn value() -> u8 { 2 }\n",
    )?;
    write_file(&workspace.join("nested/tool/generated.txt"), "scratch\n")?;

    let nested_workspace = workspace.join("nested/tool");
    let before_status = status_digest(workspace)?;
    let before_files = file_state_digest(workspace)?;

    let options = WorkspaceGitSnapshotOptions::for_workspace(&nested_workspace);
    let started = Instant::now();
    let snapshot = collect_workspace_git_snapshot(&options, &SystemSwarmBriefCommandRunner)
        .map_err(|error| format!("collect nested workspace git snapshot: {error:?}"))?;
    let elapsed_ms = started.elapsed().as_millis();

    let after_status = status_digest(workspace)?;
    let after_files = file_state_digest(workspace)?;
    let log = read_only_e2e_log(
        "nested_workspace",
        workspace,
        elapsed_ms,
        &before_status,
        &after_status,
        &before_files,
        &after_files,
    );
    assert_read_only_e2e_log(&log, "nested_workspace")?;
    assert_eq!(
        after_status, before_status,
        "provider must not change git status when invoked from a subdirectory"
    );
    assert_eq!(
        after_files, before_files,
        "provider must not change files when invoked from a subdirectory"
    );

    assert_eq!(snapshot.repository_root, workspace.display().to_string());
    let entry_paths = snapshot
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(entry_paths, vec!["nested/tool/generated.txt", "src/lib.rs"]);

    let root_file = entry_by_path(&snapshot.entries, "src/lib.rs")?;
    assert_eq!(root_file.entry_kind, "ordinary");
    assert_eq!(root_file.staged, ".");
    assert_eq!(root_file.unstaged, "M");
    assert!(root_file.metadata.as_ref().is_some_and(|metadata| {
        metadata.exists && metadata.file_type == "file" && !metadata.large_file
    }));

    let nested_untracked = entry_by_path(&snapshot.entries, "nested/tool/generated.txt")?;
    assert_eq!(nested_untracked.entry_kind, "untracked");
    assert_eq!(nested_untracked.staged, "?");
    assert_eq!(nested_untracked.unstaged, "?");
    assert!(nested_untracked.metadata.as_ref().is_some_and(|metadata| {
        metadata.exists
            && metadata.file_type == "file"
            && metadata.size_bytes == Some("scratch\n".len() as u64)
    }));

    Ok(())
}

#[test]
fn workspace_git_snapshot_provider_is_read_only_for_dirty_repo() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("ee-workspace-git-readonly-")
        .tempdir()
        .map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path();

    run_git(workspace, &["init", "-q"])?;
    run_git(
        workspace,
        &["config", "user.email", "ee-test@example.invalid"],
    )?;
    run_git(workspace, &["config", "user.name", "ee test"])?;

    write_file(&workspace.join(".gitignore"), "ignored.tmp\n")?;
    write_file(&workspace.join("tracked.txt"), "base\n")?;
    write_file(&workspace.join("both.txt"), "base\n")?;
    write_file(&workspace.join("rename_old.txt"), "rename me\n")?;
    write_file(&workspace.join("delete_me.txt"), "delete me\n")?;
    write_file(&workspace.join("staged_delete.txt"), "stage delete me\n")?;
    write_file(
        &workspace.join("binary.bin"),
        [0_u8, 159, 146, 150, 0, 1, 2],
    )?;
    run_git(
        workspace,
        &[
            "add",
            ".gitignore",
            "tracked.txt",
            "both.txt",
            "rename_old.txt",
            "delete_me.txt",
            "staged_delete.txt",
            "binary.bin",
        ],
    )?;
    run_git(workspace, &["commit", "-q", "-m", "seed"])?;

    write_file(&workspace.join("tracked.txt"), "base\nunstaged\n")?;
    write_file(&workspace.join("both.txt"), "base\nstaged\n")?;
    write_file(
        &workspace.join("binary.bin"),
        [0_u8, 159, 146, 150, 0, 1, 2, 3],
    )?;
    run_git(workspace, &["add", "both.txt"])?;
    write_file(&workspace.join("both.txt"), "base\nstaged\nunstaged\n")?;
    write_file(&workspace.join("staged.txt"), "staged\n")?;
    run_git(workspace, &["add", "staged.txt"])?;
    run_git(workspace, &["mv", "rename_old.txt", "rename_new.txt"])?;
    fs::remove_file(workspace.join("delete_me.txt"))
        .map_err(|error| format!("remove delete_me.txt in temp repo: {error}"))?;
    run_git(workspace, &["rm", "-q", "staged_delete.txt"])?;
    write_file(&workspace.join("untracked.txt"), "untracked\n")?;
    write_file(&workspace.join("ignored.tmp"), "ignored\n")?;
    write_file(&workspace.join("large.dat"), vec![b'x'; 128])?;
    unix_fs::symlink("tracked.txt", workspace.join("tracked.link"))
        .map_err(|error| format!("symlink tracked.link: {error}"))?;
    unix_fs::symlink(".", workspace.join("self.loop"))
        .map_err(|error| format!("symlink self.loop: {error}"))?;

    let before_status = status_digest(workspace)?;
    let before_files = file_state_digest(workspace)?;

    let mut options = WorkspaceGitSnapshotOptions::for_workspace(workspace);
    options.large_file_threshold_bytes = 16;
    let started = Instant::now();
    let snapshot = collect_workspace_git_snapshot(&options, &SystemSwarmBriefCommandRunner)
        .map_err(|error| format!("collect workspace git snapshot: {error:?}"))?;
    let elapsed_ms = started.elapsed().as_millis();

    let after_status = status_digest(workspace)?;
    let after_files = file_state_digest(workspace)?;
    let log = read_only_e2e_log(
        "dirty_repo",
        workspace,
        elapsed_ms,
        &before_status,
        &after_status,
        &before_files,
        &after_files,
    );
    assert_read_only_e2e_log(&log, "dirty_repo")?;
    assert_eq!(
        after_status, before_status,
        "provider must not change git status"
    );
    assert_eq!(after_files, before_files, "provider must not change files");

    let entries_by_path = snapshot
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let entry_paths = snapshot
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    let mut sorted_entry_paths = entry_paths.clone();
    sorted_entry_paths.sort_unstable();
    assert_eq!(
        entry_paths, sorted_entry_paths,
        "snapshot entries must be deterministically sorted by path"
    );
    assert!(entries_by_path.contains_key("tracked.txt"));
    assert!(entries_by_path.contains_key("both.txt"));
    assert!(entries_by_path.contains_key("staged.txt"));
    assert!(entries_by_path.contains_key("rename_new.txt"));
    assert!(entries_by_path.contains_key("delete_me.txt"));
    assert!(entries_by_path.contains_key("staged_delete.txt"));
    assert!(entries_by_path.contains_key("binary.bin"));
    assert!(entries_by_path.contains_key("untracked.txt"));
    assert!(entries_by_path.contains_key("large.dat"));
    assert!(entries_by_path.contains_key("tracked.link"));
    assert!(entries_by_path.contains_key("self.loop"));
    assert!(
        !entries_by_path.contains_key("ignored.tmp"),
        "ignored files should not appear in porcelain-v2 snapshot"
    );

    let tracked = entry_by_path(&snapshot.entries, "tracked.txt")?;
    assert_eq!(tracked.entry_kind, "ordinary");
    assert_eq!(tracked.staged, ".");
    assert_eq!(tracked.unstaged, "M");

    let both = entry_by_path(&snapshot.entries, "both.txt")?;
    assert_eq!(both.entry_kind, "ordinary");
    assert_eq!(both.staged, "M");
    assert_eq!(both.unstaged, "M");

    let staged = entry_by_path(&snapshot.entries, "staged.txt")?;
    assert_eq!(staged.staged, "A");
    assert_eq!(staged.unstaged, ".");

    let renamed = entry_by_path(&snapshot.entries, "rename_new.txt")?;
    assert_eq!(renamed.entry_kind, "renamed_or_copied");
    assert_eq!(renamed.original_path.as_deref(), Some("rename_old.txt"));

    let deleted = entry_by_path(&snapshot.entries, "delete_me.txt")?;
    assert_eq!(deleted.unstaged, "D");
    assert!(
        deleted
            .metadata
            .as_ref()
            .is_some_and(|metadata| !metadata.exists && metadata.file_type == "missing")
    );

    let staged_deleted = entry_by_path(&snapshot.entries, "staged_delete.txt")?;
    assert_eq!(staged_deleted.entry_kind, "ordinary");
    assert_eq!(staged_deleted.staged, "D");
    assert_eq!(staged_deleted.unstaged, ".");
    assert!(
        staged_deleted
            .metadata
            .as_ref()
            .is_some_and(|metadata| !metadata.exists && metadata.file_type == "missing")
    );

    let binary = entry_by_path(&snapshot.entries, "binary.bin")?;
    let binary_metadata = binary
        .metadata
        .as_ref()
        .ok_or_else(|| "binary.bin missing metadata".to_string())?;
    assert_eq!(binary.entry_kind, "ordinary");
    assert_eq!(binary.staged, ".");
    assert_eq!(binary.unstaged, "M");
    assert_eq!(binary_metadata.file_type, "file");
    assert_eq!(binary_metadata.size_bytes, Some(8));
    assert!(!binary_metadata.large_file);
    assert_eq!(binary_metadata.skip_reason, None);

    let large = entry_by_path(&snapshot.entries, "large.dat")?;
    let large_metadata = large
        .metadata
        .as_ref()
        .ok_or_else(|| "large.dat missing metadata".to_string())?;
    assert!(large_metadata.large_file);
    assert_eq!(large_metadata.size_bytes, None);
    assert_eq!(
        large_metadata.skip_reason.as_deref(),
        Some("large_file_metadata_only")
    );

    let symlink = entry_by_path(&snapshot.entries, "tracked.link")?;
    assert_eq!(
        symlink
            .metadata
            .as_ref()
            .map(|metadata| metadata.file_type.as_str()),
        Some("symlink")
    );

    let symlink_loop = entry_by_path(&snapshot.entries, "self.loop")?;
    assert_eq!(symlink_loop.entry_kind, "untracked");
    assert_eq!(
        symlink_loop
            .metadata
            .as_ref()
            .map(|metadata| metadata.file_type.as_str()),
        Some("symlink")
    );

    Ok(())
}

#[test]
fn workspace_git_snapshot_provider_is_read_only_for_unmerged_conflict() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("ee-workspace-git-conflict-readonly-")
        .tempdir()
        .map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path();

    run_git(workspace, &["init", "-q", "-b", "main"])?;
    run_git(
        workspace,
        &["config", "user.email", "ee-test@example.invalid"],
    )?;
    run_git(workspace, &["config", "user.name", "ee test"])?;

    write_file(&workspace.join("conflict.txt"), "base\n")?;
    run_git(workspace, &["add", "conflict.txt"])?;
    run_git(workspace, &["commit", "-q", "-m", "base"])?;

    run_git(workspace, &["checkout", "-q", "-b", "side"])?;
    write_file(&workspace.join("conflict.txt"), "side\n")?;
    run_git(workspace, &["commit", "-am", "side", "-q"])?;

    run_git(workspace, &["checkout", "-q", "main"])?;
    write_file(&workspace.join("conflict.txt"), "main\n")?;
    run_git(workspace, &["commit", "-am", "main", "-q"])?;
    run_git_expect_failure(workspace, &["merge", "side"])?;

    let before_status = status_digest(workspace)?;
    let before_files = file_state_digest(workspace)?;
    if !before_status.lines().any(|line| line.starts_with("u ")) {
        return Err(format!(
            "expected porcelain-v2 unmerged record before snapshot, got:\n{before_status}"
        ));
    }

    let options = WorkspaceGitSnapshotOptions::for_workspace(workspace);
    let started = Instant::now();
    let snapshot = collect_workspace_git_snapshot(&options, &SystemSwarmBriefCommandRunner)
        .map_err(|error| format!("collect workspace git snapshot: {error:?}"))?;
    let elapsed_ms = started.elapsed().as_millis();

    let after_status = status_digest(workspace)?;
    let after_files = file_state_digest(workspace)?;
    let log = read_only_e2e_log(
        "unmerged_conflict",
        workspace,
        elapsed_ms,
        &before_status,
        &after_status,
        &before_files,
        &after_files,
    );
    assert_read_only_e2e_log(&log, "unmerged_conflict")?;
    assert_eq!(
        after_status, before_status,
        "provider must not change unmerged git status"
    );
    assert_eq!(
        after_files, before_files,
        "provider must not change conflicted files"
    );

    let conflict = entry_by_path(&snapshot.entries, "conflict.txt")?;
    assert_eq!(conflict.entry_kind, "unmerged");
    assert_eq!(conflict.staged, "U");
    assert_eq!(conflict.unstaged, "U");
    assert!(conflict.metadata.as_ref().is_some_and(|metadata| {
        metadata.exists && metadata.file_type == "file" && !metadata.large_file
    }));

    Ok(())
}

#[test]
fn workspace_git_snapshot_provider_degrades_read_only_outside_git_repo() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("ee-workspace-git-not-repo-readonly-")
        .tempdir()
        .map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path();
    write_file(&workspace.join("loose.txt"), "not a git checkout\n")?;

    let before_files = file_state_digest(workspace)?;
    let options = WorkspaceGitSnapshotOptions::for_workspace(workspace);
    let error = collect_workspace_git_snapshot(&options, &SystemSwarmBriefCommandRunner)
        .expect_err("non-repository workspace should degrade instead of returning a snapshot");
    let after_files = file_state_digest(workspace)?;

    assert_eq!(
        after_files, before_files,
        "provider must not mutate files when git rev-parse fails"
    );
    match error {
        SwarmBriefCommandError::Failed { status, stderr } => {
            assert_ne!(status, Some(0));
            assert!(
                stderr.contains("not a git repository")
                    || stderr.contains("not a git repo")
                    || stderr.contains("outside a git repository"),
                "unexpected git rev-parse stderr: {stderr}"
            );
        }
        other => {
            return Err(format!(
                "expected git rev-parse failure outside a repository, got {other:?}"
            ));
        }
    }

    Ok(())
}
