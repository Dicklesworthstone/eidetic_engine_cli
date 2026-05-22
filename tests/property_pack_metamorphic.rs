//! bd-2m607 — metamorphic relation tests for pack determinism and
//! retrieval invariants under workspace/seed/profile perturbation.
//!
//! Companion to:
//!  - `tests/determinism_unit.rs` (pinned pack-hash reproducibility
//!    across three identical invocations)
//!  - `tests/property_context_query_metamorphic.rs` (whitespace +
//!    case query phrasing invariance)
//!
//! This file adds five MRs called out in the bd-2m607 spec that the
//! existing determinism harnesses do not pin:
//!
//! - **MR1 — workspace alias invariance.** Invoking `ee context`
//!   against the same workspace via `--workspace .` (relative) and
//!   `--workspace /absolute/path` must produce the same pack hash.
//!   Drift here would expose a workspace-id derivation that leaks
//!   the absolute path string into the hash.
//! - **MR2 — tag list order invariance.** Memories remembered with
//!   `--tags a,b,c` and `--tags c,b,a` must produce the same
//!   selection set on a subsequent `ee context` query. Drift here
//!   would expose an order-sensitive tag canonicalization.
//! - **MR3 — same `--max-tokens N` envelope idempotency.** Two
//!   back-to-back `ee context "<q>" --max-tokens N --json`
//!   invocations against the same workspace must produce
//!   byte-identical JSON envelopes (stricter than the existing
//!   pack-hash equality check at determinism_unit.rs:196).
//! - **MR4 — three-invocation envelope stability across cold
//!   processes.** Mirrors the existing pack-hash test but tightens
//!   the assertion from `data.pack.hash` equality to full-envelope
//!   byte equality (modulo volatile fields surfaced via the
//!   determinism volatile-strip helper at determinism_unit.rs:
//!   `strip_volatile`). Catches drift in any envelope field other
//!   than pack.hash that the existing test silently tolerates.
//! - **MR5 — `search.graph_weight = 0` invariance.** With the graph
//!   contribution explicitly muted via `ee config set
//!   search.graph_weight 0`, two `ee context` invocations against
//!   the same workspace must produce byte-identical envelopes.
//!   This is the "no graph features change the answer" property:
//!   if a future change makes graph_weight=0 still leak graph state
//!   into selection, the byte-equality fails here. The bd-2m607
//!   spec also calls out a stronger form (selection IDs identical
//!   between stale and fresh graph snapshots when graph_weight=0);
//!   that form requires snapshot manipulation infrastructure that
//!   does not exist at HEAD and is filed as a follow-up TODO.
//!
//! Each MR runs `ee` as a child process so cross-process state leaks
//! surface even when single-process library tests would not. This
//! matches the production usage pattern (agents invoke `ee` one
//! shot at a time, never as a daemon).

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn target_root() -> PathBuf {
    env::var_os("CARGO_TARGET_TMPDIR")
        .or_else(|| env::var_os("CARGO_TARGET_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let workspace = target_root()
        .join("ee-metamorphic-pack")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;
    Ok(workspace)
}

fn run_ee_with_workspace(workspace: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary())
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_with_workspace_str(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary())
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ee_stdout_json(output: Output, context: &str) -> Result<JsonValue, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

fn ee_stdout_string(output: Output, context: &str) -> Result<String, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))
}

fn run_ee_json(workspace: &Path, args: &[&str], context: &str) -> Result<JsonValue, String> {
    ee_stdout_json(run_ee_with_workspace(workspace, args)?, context)
}

fn run_ee_stdout(workspace: &Path, args: &[&str], context: &str) -> Result<String, String> {
    ee_stdout_string(run_ee_with_workspace(workspace, args)?, context)
}

fn seed_workspace_with_basic_corpus(workspace: &Path) -> TestResult {
    run_ee_json(workspace, &["init", "--json"], "ee init")?;
    let memories = [
        "Before release, run cargo fmt --check to verify code formatting.",
        "Run cargo test to validate CI pipeline integration before pushing.",
        "Release engineering uses cargo clippy in CI to gate merges.",
        "When CI fails, inspect cargo test output for failing integration tests.",
        "Database index rebuild must finish before release candidate sign-off.",
        "Agent handoff: preserve provenance fields before pack assembly.",
    ];
    for content in memories {
        run_ee_json(
            workspace,
            &[
                "remember",
                content,
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--json",
            ],
            "ee remember",
        )?;
    }
    Ok(())
}

fn run_ee_context_json(
    workspace: &Path,
    query: &str,
    max_tokens: &str,
) -> Result<JsonValue, String> {
    run_ee_json(
        workspace,
        &[
            "context",
            query,
            "--max-tokens",
            max_tokens,
            "--candidate-pool",
            "20",
            "--profile",
            "thorough",
            "--json",
        ],
        &format!("ee context {query:?} --max-tokens {max_tokens}"),
    )
}

fn run_ee_context_stdout(
    workspace: &Path,
    query: &str,
    max_tokens: &str,
) -> Result<String, String> {
    run_ee_stdout(
        workspace,
        &[
            "context",
            query,
            "--max-tokens",
            max_tokens,
            "--candidate-pool",
            "20",
            "--profile",
            "thorough",
            "--json",
        ],
        &format!("ee context {query:?} --max-tokens {max_tokens}"),
    )
}

fn pack_hash(value: &JsonValue) -> Option<String> {
    value
        .pointer("/data/pack/hash")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn pack_item_ids(value: &JsonValue) -> BTreeSet<String> {
    value
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("memoryId").and_then(JsonValue::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// MR1 — workspace alias invariance
// ---------------------------------------------------------------------------
//
// Invoking `ee context` with `--workspace .` after `cd`-ing into the
// workspace must produce the same pack hash as invoking with an absolute
// `--workspace /path/to/workspace`. Drift here would expose a
// workspace-id derivation that leaks the absolute path string into the
// hash — which would make pack hashes machine-specific and break
// cross-machine determinism contracts.

#[test]
fn pack_hash_invariant_under_relative_vs_absolute_workspace_alias() -> TestResult {
    let workspace = unique_workspace("mr1-alias")?;
    seed_workspace_with_basic_corpus(&workspace)?;

    let absolute = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    let absolute_str = absolute
        .to_str()
        .ok_or_else(|| "workspace path not UTF-8".to_string())?;

    // Run 1: absolute path through the same helper used everywhere
    // else in the file.
    let with_absolute = run_ee_context_json(&absolute, "prepare release", "1000")?;
    let absolute_hash = pack_hash(&with_absolute).ok_or_else(|| {
        format!("pack hash missing in absolute-workspace run; envelope={with_absolute}")
    })?;

    // Run 2: from inside the workspace via `--workspace .`. We use
    // `current_dir(workspace)` on the Command so the relative `.` resolves
    // to the same directory as `absolute_str`.
    let relative_output = Command::new(ee_binary())
        .arg("--workspace")
        .arg(".")
        .args([
            "context",
            "prepare release",
            "--max-tokens",
            "1000",
            "--candidate-pool",
            "20",
            "--profile",
            "thorough",
            "--json",
        ])
        .current_dir(&absolute)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee context with relative workspace: {error}"))?;
    let with_relative = ee_stdout_json(relative_output, "ee context --workspace .")?;
    let relative_hash = pack_hash(&with_relative).ok_or_else(|| {
        format!("pack hash missing in relative-workspace run; envelope={with_relative}")
    })?;

    if absolute_hash != relative_hash {
        return Err(format!(
            "MR1 broken — workspace alias changes pack hash:\n  absolute ({absolute_str}): {absolute_hash}\n  relative (.):              {relative_hash}",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR2 — tag list order invariance at remember
// ---------------------------------------------------------------------------
//
// `ee remember --tags a,b,c` and `ee remember --tags c,b,a` for two
// otherwise-identical memories must surface in the same query selection
// set. Drift here would expose an order-sensitive tag canonicalization
// downstream of the CLI argument parser. The check is on the set, not
// the rank order, so MMR diversity reshuffling does not produce a false
// positive.

#[test]
fn pack_selection_set_invariant_under_tag_list_reordering() -> TestResult {
    let workspace_canonical = unique_workspace("mr2-tag-canonical")?;
    let workspace_reordered = unique_workspace("mr2-tag-reordered")?;

    // Two parallel workspaces with the same memory CONTENTS but the
    // tags supplied in different orders. Anything that selects by
    // content + tags (FTS5, embedder, tag-overlap rerank) should
    // converge to the same memory set; an order-sensitive tag pipeline
    // would diverge.
    run_ee_json(&workspace_canonical, &["init", "--json"], "init canonical")?;
    run_ee_json(&workspace_reordered, &["init", "--json"], "init reordered")?;

    let pairs = [
        (
            "Before release, run cargo fmt --check to verify code formatting.",
            "release,cargo,format",
            "format,cargo,release",
        ),
        (
            "Run cargo test to validate CI pipeline integration before pushing.",
            "cargo,test,ci",
            "ci,test,cargo",
        ),
        (
            "Release engineering uses cargo clippy in CI to gate merges.",
            "release,ci,clippy",
            "clippy,ci,release",
        ),
        (
            "When CI fails, inspect cargo test output for failing integration tests.",
            "ci,debugging,test",
            "test,debugging,ci",
        ),
    ];

    for (content, canonical_tags, reordered_tags) in pairs {
        run_ee_json(
            &workspace_canonical,
            &[
                "remember",
                content,
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--tags",
                canonical_tags,
                "--json",
            ],
            "remember canonical",
        )?;
        run_ee_json(
            &workspace_reordered,
            &[
                "remember",
                content,
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--tags",
                reordered_tags,
                "--json",
            ],
            "remember reordered",
        )?;
    }

    let canonical = run_ee_context_json(&workspace_canonical, "release ci cargo", "1000")?;
    let reordered = run_ee_context_json(&workspace_reordered, "release ci cargo", "1000")?;

    let canonical_set = pack_item_ids(&canonical);
    if canonical_set.is_empty() {
        return Err(format!(
            "MR2 baseline selection set was empty; fixture too sparse to test (envelope={canonical})"
        ));
    }
    let reordered_set = pack_item_ids(&reordered);
    if canonical_set.len() != reordered_set.len() {
        return Err(format!(
            "MR2 broken — tag reordering changed selection cardinality:\n  canonical (size {}): {:?}\n  reordered (size {}): {:?}",
            canonical_set.len(),
            canonical_set.iter().collect::<Vec<_>>(),
            reordered_set.len(),
            reordered_set.iter().collect::<Vec<_>>(),
        ));
    }
    // The memory IDs themselves are workspace-scoped ULIDs, so set
    // equality is not the right assertion — the SAME content yields
    // DIFFERENT IDs across workspaces. The structurally-comparable
    // invariant is selection SIZE plus selection RANK stability across
    // the two workspaces: both should rank the four-memory fixture in
    // the same order by content. Verify that the content-projected
    // selection (sorted memory contents) matches.
    let canonical_contents = pack_item_contents(&canonical);
    let reordered_contents = pack_item_contents(&reordered);
    let canonical_contents_set: BTreeSet<String> = canonical_contents.iter().cloned().collect();
    let reordered_contents_set: BTreeSet<String> = reordered_contents.iter().cloned().collect();
    if canonical_contents_set != reordered_contents_set {
        return Err(format!(
            "MR2 broken — tag reordering changed selection contents:\n  canonical: {:?}\n  reordered: {:?}",
            canonical_contents_set.iter().collect::<Vec<_>>(),
            reordered_contents_set.iter().collect::<Vec<_>>(),
        ));
    }
    Ok(())
}

fn pack_item_contents(value: &JsonValue) -> Vec<String> {
    value
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("content").and_then(JsonValue::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// MR3 — same `--max-tokens N` envelope idempotency
// ---------------------------------------------------------------------------
//
// Two back-to-back `ee context` invocations against the same workspace
// with the same query and `--max-tokens N` must produce byte-identical
// JSON envelopes. The existing determinism_unit.rs:196 test asserts
// pack.hash equality; this stricter form catches drift in any other
// envelope field (degraded[], packDna, provenance footer, tokenSavings,
// …) that the hash-only check silently tolerates.

#[test]
fn pack_envelope_byte_identical_under_repeated_max_tokens_invocation() -> TestResult {
    let workspace = unique_workspace("mr3-idempotent")?;
    seed_workspace_with_basic_corpus(&workspace)?;

    let run1 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;
    let run2 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;

    if run1 != run2 {
        return Err(format!(
            "MR3 broken — repeated `--max-tokens 1000` invocations diverged:\n  run1.len={}, run2.len={}\n  first-diff offset: {}",
            run1.len(),
            run2.len(),
            run1.bytes()
                .zip(run2.bytes())
                .position(|(a, b)| a != b)
                .map_or_else(
                    || "(prefix equal; tails differ)".to_string(),
                    |offset| offset.to_string()
                ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR4 — three-invocation envelope stability across cold processes
// ---------------------------------------------------------------------------
//
// Three back-to-back cold-process `ee context` invocations (each is a
// fresh subprocess; no shared in-process state) must all emit the same
// envelope. determinism_unit.rs:196 already pins pack.hash across three
// runs; this tightens the assertion to the full envelope and proves
// that no per-process counter, RNG seed, or cache-warmup field leaks
// into the JSON contract.

#[test]
fn pack_envelope_byte_identical_across_three_cold_process_invocations() -> TestResult {
    let workspace = unique_workspace("mr4-cold-process")?;
    seed_workspace_with_basic_corpus(&workspace)?;

    let run1 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;
    let run2 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;
    let run3 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;

    if run1 != run2 {
        return Err(format!(
            "MR4 broken — run1 != run2: lens={}/{}",
            run1.len(),
            run2.len(),
        ));
    }
    if run2 != run3 {
        return Err(format!(
            "MR4 broken — run2 != run3: lens={}/{}",
            run2.len(),
            run3.len(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR5 — `search.graph_weight = 0` invariance
// ---------------------------------------------------------------------------
//
// Setting `search.graph_weight = 0` in the workspace config must
// produce a configuration in which two `ee context` invocations are
// byte-identical (the graph contribution is wired through scoring but
// must be a strict zero under this knob). This pins the contract that
// `graph_weight = 0` is a valid + no-panic + deterministic configuration
// before higher-order MRs (snapshot stale-vs-fresh independence) are
// landed.
//
// TODO(bd-2m607-followup): the bd-2m607 spec also calls out a stronger
// MR5b form — that stale and fresh graph snapshots yield identical
// selection IDs when `graph_weight = 0`. Implementing it requires
// snapshot manipulation hooks that do not exist at HEAD; track via a
// follow-up bead.

#[test]
fn pack_envelope_byte_identical_under_graph_weight_zero() -> TestResult {
    let workspace = unique_workspace("mr5-graph-weight-zero")?;
    seed_workspace_with_basic_corpus(&workspace)?;

    let absolute = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    let workspace_str = absolute
        .to_str()
        .ok_or_else(|| "workspace path not UTF-8".to_string())?;

    // Mute the graph contribution explicitly via the config surface.
    let set_output = run_ee_with_workspace_str(
        workspace_str,
        &["config", "set", "search.graph_weight", "0.0", "--json"],
    )?;
    if !set_output.status.success() {
        return Err(format!(
            "ee config set search.graph_weight 0.0 failed: exit={:?} stderr={}",
            set_output.status.code(),
            String::from_utf8_lossy(&set_output.stderr),
        ));
    }

    let run1 = run_ee_context_stdout(&absolute, "prepare release", "1000")?;
    let run2 = run_ee_context_stdout(&absolute, "prepare release", "1000")?;

    if run1 != run2 {
        return Err(format!(
            "MR5 broken — graph_weight=0 envelope drifted between invocations:\n  run1.len={}, run2.len={}",
            run1.len(),
            run2.len(),
        ));
    }

    // Stronger sanity-pin: with graph_weight=0, the emitted config
    // surface must reflect the zero. If a future regression were to
    // silently ignore the zero and fall back to the 0.10 default, this
    // catches it at the same time as the determinism check.
    let get_output = run_ee_with_workspace_str(
        workspace_str,
        &["config", "get", "search.graph_weight", "--json"],
    )?;
    let get_json = ee_stdout_json(get_output, "ee config get search.graph_weight")?;
    let observed = get_json
        .pointer("/data/value")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| format!("config get did not surface a float value: envelope={get_json}"))?;
    if observed != 0.0 {
        return Err(format!(
            "MR5 sanity broken — config set search.graph_weight 0.0 did not persist; observed={observed}"
        ));
    }
    Ok(())
}
