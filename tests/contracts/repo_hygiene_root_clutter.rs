//! bd-3usjw.74 - repo-root scratchpad ignore contract.
//!
//! The root ignore rules must cover recurring agent scratchpad families without
//! hiding real tests or fixtures in subdirectories. The assertions here are
//! read-only over git metadata and Cargo metadata.

use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), String>;

const REQUIRED_PATTERNS: &[&str] = &[
    "/test_*.rs",
    "/test_capture*",
    "/test_clamp*",
    "/test_drop*",
    "/test_ln_1p*",
    "/test_min*",
    "/test_minmax*",
    "/test_multibyte*",
    "/test_output*.log",
    "/temp_*.rs",
    "/ubs_*.txt",
    "/ubs_*.json",
    "/ubs_*.jsonl",
    "/ubs.json",
    "/db_for_loops*",
    "/findings.jsonl",
    "/pass*.jsonl",
    "/fix_*.sh",
    "/find_*.sh",
    "/*-upgrade-progress.json",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(repo_root().join(path)).map_err(|error| format!("read {path}: {error}"))
}

fn run_git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("git output was not utf-8: {error}"))
}

fn root_only(path: &str) -> bool {
    !path.is_empty() && !path.contains('/')
}

fn matches_root_scratchpad(path: &str) -> bool {
    if !root_only(path) {
        return false;
    }
    path == "ubs.json"
        || path == "findings.jsonl"
        || path.starts_with("test_capture")
        || path.starts_with("test_clamp")
        || path.starts_with("test_drop")
        || path.starts_with("test_ln_1p")
        || path.starts_with("test_min")
        || path.starts_with("test_minmax")
        || path.starts_with("test_multibyte")
        || path.starts_with("db_for_loops")
        || (path.starts_with("test_") && path.ends_with(".rs"))
        || (path.starts_with("test_output") && path.ends_with(".log"))
        || (path.starts_with("temp_") && path.ends_with(".rs"))
        || (path.starts_with("ubs_")
            && (path.ends_with(".txt") || path.ends_with(".json") || path.ends_with(".jsonl")))
        || (path.starts_with("pass") && path.ends_with(".jsonl"))
        || (path.starts_with("fix_") && path.ends_with(".sh"))
        || (path.starts_with("find_") && path.ends_with(".sh"))
        || path.ends_with("-upgrade-progress.json")
}

fn parse_status_path(line: &str) -> Option<(&str, &str)> {
    if line.len() < 4 {
        return None;
    }
    let status = &line[..2];
    let path = &line[3..];
    Some((status, path.trim_matches('"')))
}

#[test]
fn root_scratchpad_patterns_are_registered_for_git_and_rch() -> TestResult {
    let gitignore = read_repo_file(".gitignore")?;
    let rchignore = read_repo_file(".rchignore")?;

    let missing_git: Vec<&str> = REQUIRED_PATTERNS
        .iter()
        .copied()
        .filter(|pattern| !gitignore.lines().any(|line| line.trim() == *pattern))
        .collect();
    let missing_rch: Vec<&str> = REQUIRED_PATTERNS
        .iter()
        .copied()
        .filter(|pattern| !rchignore.lines().any(|line| line.trim() == *pattern))
        .collect();

    if !missing_git.is_empty() || !missing_rch.is_empty() {
        return Err(format!(
            "repo-root scratchpad ignore patterns missing: .gitignore={missing_git:?} .rchignore={missing_rch:?}"
        ));
    }

    Ok(())
}

#[test]
fn matching_root_scratchpads_are_ignored_not_untracked() -> TestResult {
    let status = run_git(&[
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--ignored",
    ])?;
    let mut offenders: Vec<String> = Vec::new();

    for line in status.lines() {
        let Some((status_code, path)) = parse_status_path(line) else {
            continue;
        };
        if !matches_root_scratchpad(path) {
            continue;
        }
        if status_code == "!!" {
            continue;
        }
        offenders.push(format!("{status_code} {path}"));
    }

    if !offenders.is_empty() {
        return Err(format!(
            "repo-root scratchpad path(s) are visible to git instead of ignored:\n{}",
            offenders.join("\n")
        ));
    }

    Ok(())
}

#[test]
fn no_matching_root_scratchpad_is_tracked() -> TestResult {
    let tracked = run_git(&["ls-files", "-z"])?;
    let offenders: Vec<&str> = tracked
        .split('\0')
        .filter(|path| matches_root_scratchpad(path))
        .collect();

    if !offenders.is_empty() {
        return Err(format!(
            "repo-root scratchpad pattern matched tracked file(s): {offenders:?}"
        ));
    }

    Ok(())
}

#[test]
fn cargo_metadata_does_not_register_root_test_scratchpads_as_bins() -> TestResult {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse metadata: {error}"))?;
    let root = repo_root();
    let mut offenders: Vec<String> = Vec::new();

    for package in metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        for target in package
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let is_bin = target
                .get("kind")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .any(|kind| kind.as_str() == Some("bin"));
            if !is_bin {
                continue;
            }

            let name = target
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let src_path = target
                .get("src_path")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_default();
            let is_root_file = src_path
                .parent()
                .is_some_and(|parent| same_path(parent, &root));
            let file_name = src_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();

            if name.starts_with("test_") || (is_root_file && matches_root_scratchpad(file_name)) {
                offenders.push(format!("{name}:{}", src_path.display()));
            }
        }
    }

    if !offenders.is_empty() {
        return Err(format!(
            "Cargo metadata registered root scratchpad(s) as binary target(s): {}",
            offenders.join(", ")
        ));
    }

    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    left == right
}
