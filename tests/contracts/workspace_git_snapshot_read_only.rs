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

use ee::core::swarm_brief::{
    SystemSwarmBriefCommandRunner, WorkspaceGitSnapshotOptions, WorkspaceGitStatusEntry,
    collect_workspace_git_snapshot,
};

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
    write_file(&workspace.join("rename_old.txt"), "rename me\n")?;
    write_file(&workspace.join("delete_me.txt"), "delete me\n")?;
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
            "rename_old.txt",
            "delete_me.txt",
            "binary.bin",
        ],
    )?;
    run_git(workspace, &["commit", "-q", "-m", "seed"])?;

    write_file(&workspace.join("tracked.txt"), "base\nunstaged\n")?;
    write_file(&workspace.join("staged.txt"), "staged\n")?;
    run_git(workspace, &["add", "staged.txt"])?;
    run_git(workspace, &["mv", "rename_old.txt", "rename_new.txt"])?;
    fs::remove_file(workspace.join("delete_me.txt"))
        .map_err(|error| format!("remove delete_me.txt in temp repo: {error}"))?;
    write_file(&workspace.join("untracked.txt"), "untracked\n")?;
    write_file(&workspace.join("ignored.tmp"), "ignored\n")?;
    write_file(&workspace.join("large.dat"), vec![b'x'; 128])?;
    unix_fs::symlink("tracked.txt", workspace.join("tracked.link"))
        .map_err(|error| format!("symlink tracked.link: {error}"))?;

    let before_status = status_digest(workspace)?;
    let before_files = file_state_digest(workspace)?;

    let mut options = WorkspaceGitSnapshotOptions::for_workspace(workspace);
    options.large_file_threshold_bytes = 16;
    let snapshot = collect_workspace_git_snapshot(&options, &SystemSwarmBriefCommandRunner)
        .map_err(|error| format!("collect workspace git snapshot: {error:?}"))?;

    let after_status = status_digest(workspace)?;
    let after_files = file_state_digest(workspace)?;
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
    assert!(entries_by_path.contains_key("tracked.txt"));
    assert!(entries_by_path.contains_key("staged.txt"));
    assert!(entries_by_path.contains_key("rename_new.txt"));
    assert!(entries_by_path.contains_key("delete_me.txt"));
    assert!(entries_by_path.contains_key("untracked.txt"));
    assert!(entries_by_path.contains_key("large.dat"));
    assert!(entries_by_path.contains_key("tracked.link"));
    assert!(
        !entries_by_path.contains_key("ignored.tmp"),
        "ignored files should not appear in porcelain-v2 snapshot"
    );

    let tracked = entry_by_path(&snapshot.entries, "tracked.txt")?;
    assert_eq!(tracked.entry_kind, "ordinary");
    assert_eq!(tracked.staged, ".");
    assert_eq!(tracked.unstaged, "M");

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

    Ok(())
}
