//! J7 — In-process determinism harness for tie-breaking and pack-hash
//! reproduction (bd-17c65.10.7).
//!
//! Companion to `scripts/e2e_overhaul/determinism.sh`. The bash script
//! exercises six surfaces end-to-end across three child invocations;
//! this Rust test focuses narrowly on the determinism invariants that
//! are easiest to regress and most painful to debug after the fact:
//!
//! * **Tie-break by memory_id ascending.** Two memories whose content
//!   produces byte-equal scores must rank by `memory_id` ascending
//!   (lower ULID first), and that order must be byte-stable across
//!   repeated invocations of the same query against the same
//!   workspace.
//! * **Pack-hash reproducibility.** Two `ee context` invocations
//!   against the same workspace + query + budget + profile must
//!   produce identical `data.pack.hash` values.
//!
//! The test spawns `ee` as a child process so state leaks (per-process
//! caches, in-memory RNGs, wall-clock fields embedded in responses)
//! surface here even though they would not surface in a single-process
//! library-level test. This mirrors the production usage pattern:
//! agents invoke `ee` one shot at a time, never as a daemon.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary())
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

fn init_workspace(workspace: &Path) -> Result<(), String> {
    let output = run_ee(&["--workspace", workspace.to_str().unwrap(), "init", "--json"])?;
    if !output.status.success() {
        return Err(format!(
            "ee init failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

fn remember(workspace: &Path, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace.to_str().unwrap(),
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ])?;
    let value = parse_json(&output, "remember")?;
    // Multiple shape variants in flight across the swarm; try each.
    value
        .pointer("/data/memory_id")
        .or_else(|| value.pointer("/data/memoryId"))
        .or_else(|| value.pointer("/data/memory/id"))
        .or_else(|| value.pointer("/data/id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember response did not surface a memory id: {value}",))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn tmp_workspace(label: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!(
        "ee-determinism-{}-{}-{}",
        label,
        std::process::id(),
        nonce
    ));
    std::fs::create_dir_all(&base).map_err(|error| format!("create workspace: {error}"))?;
    Ok(base)
}

fn search_result_ids(value: &Value) -> Vec<String> {
    value
        .pointer("/data/results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|hit| {
                    hit.get("docId")
                        .or_else(|| hit.get("doc_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn search_tie_break_stable_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("tie_break")?;
    init_workspace(&workspace)?;

    // Seed three memories that pre-fusion share the same content shape
    // and therefore land at the same fused RRF score for a search query
    // that matches all three lexically.
    let id_b = remember(&workspace, "Run cargo fmt before release v0.2.")?;
    let id_a = remember(&workspace, "Run cargo fmt before release v0.1.")?;
    let id_c = remember(&workspace, "Run cargo fmt before release v0.3.")?;

    let run_search = || -> Result<Vec<String>, String> {
        let output = run_ee(&[
            "--workspace",
            workspace.to_str().unwrap(),
            "search",
            "cargo fmt before release",
            "--limit",
            "10",
            "--relevance-floor",
            "0",
            "--json",
        ])?;
        let value = parse_json(&output, "search")?;
        Ok(search_result_ids(&value))
    };

    let run1 = run_search()?;
    let run2 = run_search()?;
    let run3 = run_search()?;

    ensure(
        !run1.is_empty(),
        "search must return at least one result".to_owned(),
    )?;
    ensure(run1 == run2, format!("run1 != run2: {run1:?} vs {run2:?}"))?;
    ensure(run2 == run3, format!("run2 != run3: {run2:?} vs {run3:?}"))?;

    // Tie-break direction check: when all three memory IDs appear and
    // share an equal score, lower ULID must rank first. Sort the
    // observed memory IDs in our run by occurrence position and the
    // canonical alphabetical sort and assert they match.
    let mut canonical = vec![id_a.clone(), id_b.clone(), id_c.clone()];
    canonical.sort();
    let observed: Vec<String> = run1
        .iter()
        .filter(|id| id == &&id_a || id == &&id_b || id == &&id_c)
        .cloned()
        .collect();
    if observed.len() == canonical.len() {
        ensure(
            observed == canonical,
            format!(
                "tie-break must rank by memory_id ascending; observed={observed:?} canonical={canonical:?}",
            ),
        )?;
    }
    Ok(())
}

#[test]
fn context_pack_hash_reproduces_across_three_invocations() -> TestResult {
    let workspace = tmp_workspace("pack_hash")?;
    init_workspace(&workspace)?;
    remember(&workspace, "Use cargo fmt before release.")?;
    remember(&workspace, "Database connection pooling guide.")?;
    remember(&workspace, "Migration 0042 added user_email column.")?;

    let run_context = || -> Result<Option<String>, String> {
        let output = run_ee(&[
            "--workspace",
            workspace.to_str().unwrap(),
            "context",
            "prepare release",
            "--max-tokens",
            "1000",
            "--json",
        ])?;
        let value = parse_json(&output, "context")?;
        Ok(value
            .pointer("/data/pack/hash")
            .and_then(Value::as_str)
            .map(str::to_owned))
    };

    let h1 = run_context()?;
    let h2 = run_context()?;
    let h3 = run_context()?;

    if h1.is_none() || h2.is_none() || h3.is_none() {
        // Some build configurations leave the pack hash null on
        // degraded paths. The bash harness covers this case; here we
        // accept the absence and skip the equality check (the test
        // does not produce a misleading green).
        return Err(format!(
            "context pack hash absent in at least one run: {h1:?} {h2:?} {h3:?}; \
             determinism cannot be asserted",
        ));
    }
    ensure(
        h1 == h2,
        format!("pack hash run1 != run2: {h1:?} vs {h2:?}"),
    )?;
    ensure(
        h2 == h3,
        format!("pack hash run2 != run3: {h2:?} vs {h3:?}"),
    )?;
    Ok(())
}
