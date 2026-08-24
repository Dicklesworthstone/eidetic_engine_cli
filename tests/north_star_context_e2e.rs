//! Plan §4 North Star exact context-command E2E tests (eidetic_engine_cli-axyb).
//!
//! These tests execute the exact commands specified in COMPREHENSIVE_PLAN §4
//! and verify the contract expectations for each scenario.
//!
//! Scenario coverage here: §4.1–§4.6 plus the §4.3/§4.7 promoted-rule loop
//! (`north_star_3*`). Plan §4.8 (multi-agent concurrent writers) is proven by
//! the dedicated multiprocess E2E lane tracked under bd-d67os.27 rather than
//! duplicated in-process here.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, MutexGuard};
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
    let _serial_guard = lock_real_ee_serial();
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|e| format!("failed to run ee {}: {e}", args.join(" ")))
}

/// File-local serialization gate for every real-binary spawn below.
///
/// Same contention-hygiene pattern as `tests/contracts/common_spawn.rs`
/// (bd-7vtqm): concurrent libtest workers spawning the real `ee` binary
/// compete for CPU and skew latency-sensitive assertions. Each integration
/// test crate is a separate compilation unit, so this file keeps its own
/// gate until the shared helper is promoted crate-wide.
static REAL_EE_SERIAL: Mutex<()> = Mutex::new(());

fn lock_real_ee_serial() -> MutexGuard<'static, ()> {
    // A panicked peer poisons the gate; the lock guards scheduling hygiene
    // only, so poisoned or not, later spawns must proceed.
    REAL_EE_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Spawn the real binary with extra environment overrides while holding the
/// serialization gate. Used for fault injection (e.g. breaking the embed
/// model path) in degradation scenarios.
fn run_ee_with_env(args: &[&str], envs: &[(&str, &str)]) -> Result<Output, String> {
    let _serial_guard = lock_real_ee_serial();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
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
            "Preserve uncommitted changes with a WIP commit instead of discarding them.",
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
        "pack",
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
        "pack",
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
        "pack",
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
        "pack",
        "clean up generated files and reset the repo state",
        "--workspace",
        dir.to_str().unwrap(),
        "--format",
        "markdown",
    ])?;

    ensure(output.status.success(), "context command failed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Exact closed-loop assertions (bd-2mpct): every seeded guard must
    // surface in the pack — not merely one of several substrings. The
    // fixtures are ours, so exact matches cannot flake on unrelated copy.
    for expected in ["git clean -fd", "rm -rf", "WIP commit", "git reset --hard"] {
        ensure(
            stdout.contains(expected),
            format!("cleanup pack must contain seeded guard {expected:?}"),
        )?;
    }

    Ok(())
}

/// Plan §4.6: Offline degraded mode still helps — with dependencies actually
/// removed, not merely assumed (bd-2mpct).
///
/// Injection:
/// - semantic tier faulted out via the documented `EE_EMBED_MODEL_PATH`
///   diagnostics knob plus `EE_EMBED_DOWNLOAD=off` (no network escape hatch),
/// - CASS removed via workspace config `[cass] enabled = false` (also proves
///   unknown-key rejection does not fire for this documented key).
///
/// The pack must still retrieve explicit memories through lexical retrieval
/// AND report its degradation truthfully instead of pretending to be healthy.
#[test]
fn north_star_6_degraded_mode_uses_lexical_fallback() -> TestResult {
    let dir = scenario_dir("degraded_mode")?;
    init_workspace(&dir)?;

    let ee_dir = dir.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|e| e.to_string())?;
    fs::write(ee_dir.join("config.toml"), "[cass]\nenabled = false\n")
        .map_err(|e| e.to_string())?;

    let seed_args = [
        "remember",
        "Run tests before release to catch regressions.",
        "--workspace",
        dir.to_str().unwrap(),
        "--kind",
        "rule",
        "--level",
        "procedural",
        "--json",
    ];
    let output = run_ee_with_env(
        &seed_args,
        &[
            ("EE_EMBED_MODEL_PATH", "/nonexistent/ee-no-such-model"),
            ("EE_EMBED_DOWNLOAD", "off"),
        ],
    )?;
    ensure(output.status.success(), "seed memory failed")?;
    let seeded = parse_json_stdout(&output, "seed memory")?;
    let memory_id = seeded
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .ok_or("seed memory response must carry data.memory_id")?
        .to_owned();

    // Index rebuild under the same faulted environment so no semantic tier is
    // ever available during the scenario.
    let rebuild_args = [
        "index",
        "rebuild",
        "--workspace",
        dir.to_str().unwrap(),
        "--json",
    ];
    let output = run_ee_with_env(
        &rebuild_args,
        &[
            ("EE_EMBED_MODEL_PATH", "/nonexistent/ee-no-such-model"),
            ("EE_EMBED_DOWNLOAD", "off"),
        ],
    )?;
    ensure(
        output.status.success(),
        "index rebuild failed in offline mode",
    )?;

    let pack_args = [
        "pack",
        "run tests before release",
        "--workspace",
        dir.to_str().unwrap(),
        "--json",
    ];
    let output = run_ee_with_env(
        &pack_args,
        &[
            ("EE_EMBED_MODEL_PATH", "/nonexistent/ee-no-such-model"),
            ("EE_EMBED_DOWNLOAD", "off"),
        ],
    )?;

    ensure(
        output.status.success(),
        "context command failed in degraded mode",
    )?;
    let json = parse_json_stdout(&output, "degraded context")?;

    // Exact behavioral contract, not shape checks.
    ensure(
        json.get("schema").and_then(JsonValue::as_str) == Some("ee.response.v2"),
        "degraded pack must emit ee.response.v2 envelope",
    )?;
    ensure(
        json.get("success").and_then(JsonValue::as_bool) == Some(true),
        "lexical fallback pack must succeed",
    )?;
    let items = json
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .ok_or("degraded pack must include data.pack.items")?;
    ensure(
        !items.is_empty(),
        "degraded mode must still retrieve explicit memories",
    )?;
    ensure(
        items.iter().any(|item| {
            item.get("memoryId").and_then(JsonValue::as_str) == Some(memory_id.as_str())
        }),
        "degraded pack must surface the seeded rule memory verbatim by id",
    )?;

    // Truthful degradation: the broken semantic tier must be reported, not
    // silently swallowed.
    let degraded = json
        .get("degraded")
        .and_then(JsonValue::as_array)
        .ok_or("response must carry degraded[] array")?;
    ensure(
        !degraded.is_empty(),
        "offline scenario must report non-empty degraded[] (semantic unavailable)",
    )?;

    Ok(())
}

fn seed_failure_memory(dir: &Path) -> Result<String, String> {
    let output = run_ee(&[
        "remember",
        "Release failed because clippy warnings were not treated as errors before tagging.",
        "--workspace",
        dir.to_str().unwrap(),
        "--level",
        "episodic",
        "--kind",
        "failure",
        "--json",
    ])?;
    ensure(output.status.success(), "failure-memory seed failed")?;
    let seeded = parse_json_stdout(&output, "failure memory")?;
    seeded
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "failure memory response must carry data.memory_id".to_owned())
}

fn promote_rule_from_memory(dir: &Path, memory_id: &str) -> Result<String, String> {
    let output = run_ee(&[
        "rule",
        "add",
        "Treat clippy warnings as errors before any release tag.",
        "--workspace",
        dir.to_str().unwrap(),
        "--source-memory",
        memory_id,
        "--json",
    ])?;
    ensure(output.status.success(), "rule promotion failed")?;
    let added = parse_json_stdout(&output, "rule add")?;
    let rule_id = added
        .pointer("/data/ruleId")
        .and_then(JsonValue::as_str)
        .ok_or("rule add response must carry data.ruleId")?
        .to_owned();
    ensure(
        rule_id.starts_with("rule_"),
        "rule id must use the rule_ document namespace",
    )?;
    Ok(rule_id)
}

/// Plan §4.3/§4 success signal: a repeated CI failure promoted to procedural
/// memory must be retrievable before the agent repeats it. Public CLI only,
/// no mocks, no manually seeded index state (bd-2mpct).
#[test]
fn north_star_3_promoted_ci_rule_is_retrievable() -> TestResult {
    let dir = scenario_dir("ci_failure_procedural")?;
    init_workspace(&dir)?;
    let memory_id = seed_failure_memory(&dir)?;
    let rule_id = promote_rule_from_memory(&dir, &memory_id)?;

    ensure(
        run_ee(&[
            "index",
            "rebuild",
            "--workspace",
            dir.to_str().unwrap(),
            "--json",
        ])?
        .status
        .success(),
        "index rebuild failed",
    )?;

    let output = run_ee(&[
        "search",
        "clippy warnings release tag",
        "--workspace",
        dir.to_str().unwrap(),
        "--limit",
        "10",
        "--json",
    ])?;
    ensure(output.status.success(), "search failed")?;
    let json = parse_json_stdout(&output, "search")?;
    let results = json
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .ok_or("search must return data.results")?;
    ensure(
        results.iter().any(|hit| {
            hit.get("docId")
                .and_then(JsonValue::as_str)
                .map(|doc| doc == rule_id.as_str())
                .unwrap_or(false)
        }),
        format!("search must surface the promoted rule document {rule_id}"),
    )?;

    // Plan §4.7 success signal folded in: the promotion must be auditable —
    // hash-chained rows, typed mutation kind, no silent rewrites.
    let output = run_ee(&["audit", "timeline", "--target", &rule_id, "--json"])?;
    ensure(output.status.success(), "audit timeline failed")?;
    let audit = parse_json_stdout(&output, "audit timeline")?;
    let entries = audit
        .pointer("/data/entries")
        .and_then(JsonValue::as_array)
        .ok_or("audit timeline must return data.entries")?;
    ensure(
        entries.iter().any(|entry| {
            entry.get("mutation_kind").and_then(JsonValue::as_str) == Some("rule.create")
        }),
        "promotion must produce a rule.create audit entry",
    )?;
    ensure(
        entries.iter().any(|entry| {
            entry
                .get("this_row_hash")
                .and_then(JsonValue::as_str)
                .map(|hash| hash.starts_with("blake3:"))
                .unwrap_or(false)
        }),
        "audit entries must be hash-chained (blake3)",
    )?;

    Ok(())
}

/// Regression tripwire for the remaining §4.3 gap: the NEXT context call must
/// surface the promoted rule itself, not only its source memory. Fails today
/// because `rule_linked_memory_id` hydration collapses rules into their source
/// memory items; un-ignore when bd-3h6bz lands direct-rule pack admission.
#[test]
#[ignore = "bd-3h6bz: pack omits linked rule documents; enable when direct-rule pack items land"]
fn north_star_3b_pack_surfaces_promoted_rule() -> TestResult {
    let dir = scenario_dir("ci_failure_procedural_pack")?;
    init_workspace(&dir)?;
    let memory_id = seed_failure_memory(&dir)?;
    let rule_id = promote_rule_from_memory(&dir, &memory_id)?;

    ensure(
        run_ee(&[
            "index",
            "rebuild",
            "--workspace",
            dir.to_str().unwrap(),
            "--json",
        ])?
        .status
        .success(),
        "index rebuild failed",
    )?;

    let output = run_ee(&[
        "pack",
        "release tagging with clippy warnings",
        "--workspace",
        dir.to_str().unwrap(),
        "--max-tokens",
        "3000",
        "--format",
        "markdown",
    ])?;
    ensure(output.status.success(), "pack failed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.contains("Treat clippy warnings as errors before any release tag."),
        format!("pack must surface promoted rule {rule_id} body verbatim"),
    )?;

    Ok(())
}
