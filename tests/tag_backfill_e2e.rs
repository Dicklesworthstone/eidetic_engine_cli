//! bd-historical-tag-backfill-0pgro — real-binary E2E for `ee index backfill-tags`.
//!
//! The field report (2026-08-26) was that historical memories carry no tags at
//! all, so a full underwrite verdict was reachable by `ee pack` semantic search
//! but completely invisible to `ee memory list --tag ticker:...`. This test
//! drives the whole migration through the real `ee` binary and pins the four
//! things the acceptance asks for:
//!
//!   * **The row really is dark first.** `--tag ticker:rdvt` returns nothing
//!     before the backfill runs, so the later hit proves the backfill and not
//!     some pre-existing tag.
//!   * **Dry run writes nothing.** The default invocation reports its plan and
//!     leaves the tag query still empty.
//!   * **Apply makes the dark row reachable**, and only the justified rows —
//!     a control memory with no ticker evidence must not acquire one.
//!   * **Re-running is a no-op.** A second apply proposes zero and writes zero,
//!     which is what makes this safe to run more than once.
//!
//! The JSONL mutation log is checked for real content: every line must parse
//! and carry the schema, the memory id, and the evidence span that justified
//! each tag.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn workspace_root() -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let root = target_root
        .join("ee-tag-backfill-e2e")
        .join(format!("{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("failed to create {}: {error}", root.display()))?;
    Ok(root)
}

fn run_ee(workspace: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary())
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_json(workspace: &Path, args: &[&str]) -> Result<Value, String> {
    let output = run_ee(workspace, args)?;
    if !output.status.success() {
        return Err(format!(
            "ee {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .map_err(|error| format!("ee {} emitted non-JSON: {error}: {stdout}", args.join(" ")))
}

fn remember(workspace: &Path, content: &str, kind: &str) -> Result<String, String> {
    let value = run_json(
        workspace,
        &[
            "remember", content, "--level", "semantic", "--kind", kind, "--json",
        ],
    )?;
    value["data"]["memoryId"]
        .as_str()
        .or_else(|| value["data"]["memory_id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("remember response missing memory id: {value}"))
}

/// How many memories `ee memory list --tag <tag>` returns.
fn tag_hit_count(workspace: &Path, tag: &str) -> Result<usize, String> {
    let value = run_json(
        workspace,
        &["memory", "list", "--tag", tag, "--limit", "50", "--json"],
    )?;
    Ok(value["data"]["memories"]
        .as_array()
        .map_or(0, std::vec::Vec::len))
}

fn tag_contains_memory(workspace: &Path, tag: &str, memory_id: &str) -> Result<bool, String> {
    let value = run_json(
        workspace,
        &["memory", "list", "--tag", tag, "--limit", "50", "--json"],
    )?;
    Ok(value["data"]["memories"]
        .as_array()
        .is_some_and(|memories| {
            memories
                .iter()
                .any(|memory| memory["id"].as_str() == Some(memory_id))
        }))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn backfill_makes_dark_memories_reachable_by_tag_and_is_idempotent() -> TestResult {
    let root = workspace_root()?;
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let log_path = root.join("backfill.jsonl");

    run_json(&workspace, &["init", "--json"])?;

    // A memory that names a ticker the way the field report's did, and a
    // control with no ticker evidence at all.
    let dark = remember(
        &workspace,
        "Underwrite verdict for $RDVT: pass on the screen.",
        "decision",
    )?;
    let control = remember(
        &workspace,
        "The JSON API returned HTTP 500 during the nightly run.",
        "fact",
    )?;

    // 1. The row really is dark before we touch anything.
    ensure(
        tag_hit_count(&workspace, "ticker:rdvt")? == 0,
        "ticker:rdvt must match nothing before the backfill runs",
    )?;

    // 2. Dry run plans but writes nothing.
    let dry = run_json(&workspace, &["index", "backfill-tags", "--json"])?;
    ensure(
        dry["data"]["dryRun"] == Value::Bool(true),
        format!("default invocation must be a dry run: {dry}"),
    )?;
    ensure(
        dry["data"]["proposedMemoryCount"].as_u64().unwrap_or(0) >= 1,
        format!("dry run must propose the dark memory: {dry}"),
    )?;
    ensure(
        tag_hit_count(&workspace, "ticker:rdvt")? == 0,
        "a dry run must not make the memory reachable",
    )?;

    // 3. Apply makes exactly the justified row reachable.
    let applied = run_json(
        &workspace,
        &[
            "index",
            "backfill-tags",
            "--apply",
            "--log",
            log_path.to_str().ok_or("log path is not valid UTF-8")?,
            "--json",
        ],
    )?;
    ensure(
        applied["data"]["dryRun"] == Value::Bool(false),
        format!("--apply must not report a dry run: {applied}"),
    )?;
    ensure(
        applied["data"]["appliedMemoryCount"].as_u64().unwrap_or(0) >= 1,
        format!("--apply must report applied memories: {applied}"),
    )?;
    ensure(
        tag_contains_memory(&workspace, "ticker:rdvt", &dark)?,
        "the previously dark memory must now be reachable by --tag",
    )?;
    ensure(
        !tag_contains_memory(&workspace, "ticker:rdvt", &control)?,
        "the control memory has no ticker evidence and must not acquire one",
    )?;

    // 4. Re-running proposes nothing and writes nothing.
    let replay = run_json(&workspace, &["index", "backfill-tags", "--apply", "--json"])?;
    ensure(
        replay["data"]["proposedMemoryCount"].as_u64() == Some(0),
        format!("a second apply must propose nothing: {replay}"),
    )?;
    ensure(
        replay["data"]["appliedMemoryCount"].as_u64() == Some(0),
        format!("a second apply must write nothing: {replay}"),
    )?;
    ensure(
        tag_contains_memory(&workspace, "ticker:rdvt", &dark)?,
        "the replay must leave the applied tag in place",
    )?;

    // 5. The JSONL log carries real, checkable evidence.
    let log = std::fs::read_to_string(&log_path)
        .map_err(|error| format!("failed to read {}: {error}", log_path.display()))?;
    let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    ensure(
        !lines.is_empty(),
        "applying must append at least one JSONL log line",
    )?;
    let mut saw_ticker_evidence = false;
    for line in &lines {
        let entry: Value = serde_json::from_str(line)
            .map_err(|error| format!("log line is not JSON: {error}: {line}"))?;
        ensure(
            entry["schema"] == Value::String("ee.tag_backfill.log.v1".to_owned()),
            format!("log line must carry its schema: {line}"),
        )?;
        ensure(
            entry["memoryId"].as_str().is_some_and(|id| !id.is_empty()),
            format!("log line must name the memory it mutated: {line}"),
        )?;
        let empty: Vec<Value> = Vec::new();
        for derivation in entry["derivations"].as_array().unwrap_or(&empty) {
            ensure(
                derivation["evidence"]
                    .as_str()
                    .is_some_and(|evidence| !evidence.is_empty()),
                format!("every logged derivation must record its evidence span: {line}"),
            )?;
            if derivation["tag"] == Value::String("ticker:rdvt".to_owned()) {
                saw_ticker_evidence = true;
                ensure(
                    derivation["rule"] == Value::String("ticker_cashtag".to_owned()),
                    format!("the cash-tag rule must be reported for $RDVT: {line}"),
                )?;
            }
        }
    }
    ensure(
        saw_ticker_evidence,
        "the log must record the ticker derivation that made the memory reachable",
    )
}
