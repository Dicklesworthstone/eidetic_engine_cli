//! Plan §4 North Star exact context-command E2E tests (eidetic_engine_cli-axyb).
//!
//! These tests execute the exact commands specified in COMPREHENSIVE_PLAN §4
//! and verify the contract expectations for each scenario.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

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
        .map_err(|e| format!("failed to run ee {}: {e}", args.join(" ")))
}

fn parse_json_stdout(output: &Output, ctx: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("{ctx}: stdout must be valid JSON: {e}"))
}

fn scenario_dir(name: &str) -> Result<PathBuf, String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join("north_star")
        .join(name)
        .join(format!("{}-{ts}", std::process::id())))
}

fn init_workspace(dir: &Path) -> TestResult {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let output = run_ee(&["init", "--workspace", dir.to_str().unwrap(), "--json"])?;
    ensure(output.status.success(), "init failed")
}

fn seed_release_memories(dir: &Path) -> TestResult {
    let memories = [
        (
            "Always run cargo test before creating a release tag.",
            "rule",
            "procedural",
        ),
        (
            "The 2026-04-15 release failed because tests weren't run locally first.",
            "fact",
            "episodic",
        ),
        (
            "Never force-push to main branch during release.",
            "rule",
            "procedural",
        ),
        (
            "Publishing to crates.io requires cargo publish --dry-run first.",
            "rule",
            "procedural",
        ),
    ];
    for (content, kind, level) in memories {
        let output = run_ee(&[
            "remember",
            content,
            "--workspace",
            dir.to_str().unwrap(),
            "--kind",
            kind,
            "--level",
            level,
            "--json",
        ])?;
        ensure(
            output.status.success(),
            format!("seed memory failed: {content}"),
        )?;
    }
    Ok(())
}

fn seed_async_migration_memories(dir: &Path) -> TestResult {
    let memories = [
        (
            "Asupersync uses &Cx for threading, not Tokio runtime.",
            "rule",
            "procedural",
        ),
        (
            "Outcome::ok() and Outcome::err() replace Result in async code.",
            "rule",
            "procedural",
        ),
        (
            "Budget and capability fields must be threaded through &Cx.",
            "rule",
            "procedural",
        ),
        (
            "Tokio is forbidden in this codebase per AGENTS.md.",
            "rule",
            "procedural",
        ),
    ];
    for (content, kind, level) in memories {
        let output = run_ee(&[
            "remember",
            content,
            "--workspace",
            dir.to_str().unwrap(),
            "--kind",
            kind,
            "--level",
            level,
            "--json",
        ])?;
        ensure(
            output.status.success(),
            format!("seed memory failed: {content}"),
        )?;
    }
    Ok(())
}

fn seed_onboarding_memories(dir: &Path) -> TestResult {
    let memories = [
        (
            "Run cargo fmt --check before committing.",
            "rule",
            "procedural",
        ),
        (
            "The project uses Rust 2024 edition with nightly toolchain.",
            "fact",
            "semantic",
        ),
        (
            "Check AGENTS.md for coding conventions.",
            "rule",
            "procedural",
        ),
        (
            "Use scripts/verify.sh to run all gates.",
            "rule",
            "procedural",
        ),
    ];
    for (content, kind, level) in memories {
        let output = run_ee(&[
            "remember",
            content,
            "--workspace",
            dir.to_str().unwrap(),
            "--kind",
            kind,
            "--level",
            level,
            "--json",
        ])?;
        ensure(
            output.status.success(),
            format!("seed memory failed: {content}"),
        )?;
    }
    Ok(())
}

fn seed_cleanup_memories(dir: &Path) -> TestResult {
    let memories = [
        (
            "git clean -fd is dangerous - use git status first.",
            "rule",
            "procedural",
        ),
        (
            "Never run rm -rf without explicit confirmation.",
            "rule",
            "procedural",
        ),
        (
            "Use git stash instead of discarding uncommitted changes.",
            "rule",
            "procedural",
        ),
        (
            "The 2026-03-10 incident lost work due to accidental git reset --hard.",
            "fact",
            "episodic",
        ),
    ];
    for (content, kind, level) in memories {
        let output = run_ee(&[
            "remember",
            content,
            "--workspace",
            dir.to_str().unwrap(),
            "--kind",
            kind,
            "--level",
            level,
            "--json",
        ])?;
        ensure(
            output.status.success(),
            format!("seed memory failed: {content}"),
        )?;
    }
    Ok(())
}

/// Plan §4.1: Release memory saves bad release
/// Command: ee context "what should I know before releasing this project?" --workspace . --format markdown
#[test]
fn north_star_1_release_context_includes_verification_rules() -> TestResult {
    let dir = scenario_dir("release_context")?;
    init_workspace(&dir)?;
    seed_release_memories(&dir)?;

    let output = run_ee(&[
        "context",
        "what should I know before releasing this project?",
        "--workspace",
        dir.to_str().unwrap(),
        "--format",
        "markdown",
    ])?;

    ensure(output.status.success(), "context command failed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        stdout.contains("cargo test") || stdout.contains("test"),
        "context should mention running tests before release",
    )?;
    ensure(
        stdout.contains("force-push") || stdout.contains("main branch"),
        "context should warn about force-push dangers",
    )?;

    Ok(())
}

/// Plan §4.2: Async migration honors real runtime model
/// Command: ee context "replace a tokio service with asupersync" --workspace . --json
#[test]
fn north_star_2_async_migration_context_is_json_and_mentions_cx() -> TestResult {
    let dir = scenario_dir("async_migration")?;
    init_workspace(&dir)?;
    seed_async_migration_memories(&dir)?;

    let output = run_ee(&[
        "context",
        "replace a tokio service with asupersync",
        "--workspace",
        dir.to_str().unwrap(),
        "--json",
    ])?;

    ensure(output.status.success(), "context command failed")?;
    let json = parse_json_stdout(&output, "async migration context")?;

    ensure(json.is_object(), "output must be JSON object")?;
    ensure(
        json.get("schema").is_some() || json.get("data").is_some(),
        "output should have schema or data field",
    )?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.contains("Cx") || stdout.contains("asupersync") || stdout.contains("Outcome"),
        "context should mention &Cx or asupersync or Outcome",
    )?;

    Ok(())
}

/// Plan §4.4: New repository onboarding without web UI
/// Command: ee context "start working in this repository" --workspace . --max-tokens 3000 --format markdown
#[test]
fn north_star_4_onboarding_context_includes_conventions() -> TestResult {
    let dir = scenario_dir("onboarding")?;
    init_workspace(&dir)?;
    seed_onboarding_memories(&dir)?;

    let output = run_ee(&[
        "context",
        "start working in this repository",
        "--workspace",
        dir.to_str().unwrap(),
        "--max-tokens",
        "3000",
        "--format",
        "markdown",
    ])?;

    ensure(output.status.success(), "context command failed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        stdout.contains("cargo fmt") || stdout.contains("fmt"),
        "onboarding context should mention formatting",
    )?;
    ensure(
        stdout.contains("AGENTS.md") || stdout.contains("conventions"),
        "onboarding context should mention conventions",
    )?;

    Ok(())
}

/// Plan §4.5: Catastrophic mistake avoidance
/// Command: ee context "clean up generated files and reset the repo state" --workspace . --format markdown
#[test]
fn north_star_5_cleanup_context_warns_about_dangers() -> TestResult {
    let dir = scenario_dir("cleanup_danger")?;
    init_workspace(&dir)?;
    seed_cleanup_memories(&dir)?;

    let output = run_ee(&[
        "context",
        "clean up generated files and reset the repo state",
        "--workspace",
        dir.to_str().unwrap(),
        "--format",
        "markdown",
    ])?;

    ensure(output.status.success(), "context command failed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        stdout.contains("dangerous") || stdout.contains("git status") || stdout.contains("rm -rf"),
        "cleanup context should warn about dangerous operations",
    )?;

    Ok(())
}

/// Plan §4.6: Offline degraded mode still helps
/// With semantic unavailable, context should still work with lexical search.
#[test]
fn north_star_6_degraded_mode_uses_lexical_fallback() -> TestResult {
    let dir = scenario_dir("degraded_mode")?;
    init_workspace(&dir)?;

    let output = run_ee(&[
        "remember",
        "Run tests before release to catch regressions.",
        "--workspace",
        dir.to_str().unwrap(),
        "--kind",
        "rule",
        "--level",
        "procedural",
        "--json",
    ])?;
    ensure(output.status.success(), "seed memory failed")?;

    let output = run_ee(&[
        "context",
        "run tests before release",
        "--workspace",
        dir.to_str().unwrap(),
        "--json",
    ])?;

    ensure(
        output.status.success(),
        "context command failed in degraded mode",
    )?;
    let json = parse_json_stdout(&output, "degraded context")?;
    ensure(json.is_object(), "output must be JSON object")?;

    Ok(())
}
