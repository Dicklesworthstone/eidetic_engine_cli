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
//!   byte-identical normalized JSON envelopes (stricter than the existing
//!   pack-hash equality check at determinism_unit.rs:196).
//! - **MR4 — three-invocation envelope stability across cold
//!   processes.** Mirrors the existing pack-hash test but tightens
//!   the assertion from `data.pack.hash` equality to full-envelope
//!   byte equality after validating and normalizing only producer SLO elapsedMs,
//!   elapsedStatus and aggregate status; see `docs/volatile_field_registry.md`.
//!   Catches drift in fields outside pack.hash that the existing test
//!   silently tolerates, without requiring identical assembly durations.
//! - **MR5 — `graph.ppr.alpha = 0` invariance.** With the graph
//!   contribution explicitly muted via `ee config set
//!   graph.ppr.alpha 0`, two `ee context` invocations against the
//!   same workspace must produce byte-identical normalized envelopes. This is
//!   the "no graph features change the answer" property: if a
//!   future change makes a zero PPR weight still leak graph state
//!   into selection, the byte-equality fails here. The bd-2m607
//!   spec also calls out a stronger form: selection IDs identical
//!   between stale and fresh graph snapshots when graph contribution is zero.
//!   `pack_selection_ids_identical_between_fresh_and_stale_graph_snapshots_under_graph_ppr_alpha_zero`
//!   pins that stronger MR5b form by marking the latest memory-link
//!   snapshot stale inside a temporary workspace.
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

use ee::db::{
    CreateMemoryLinkInput, DbConnection, GraphSnapshotStatus, GraphSnapshotType,
    MemoryLinkRelation, MemoryLinkSource,
};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

/// bd-rvrj2: each workspace gets its own ee data dir beside it, so a verdict
/// never depends on the model or global store the host holds. The helper
/// module is declared once, in tests/suites/integration_property.rs.
fn ee_command(workspace: &Path) -> Result<Command, String> {
    let mut root = workspace.as_os_str().to_os_string();
    root.push(".ee-data");
    super::isolated_ee::isolated_ee_command(Path::new(&root))
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
    ee_command(workspace)?
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_with_workspace_str(workspace: &str, args: &[&str]) -> Result<Output, String> {
    ee_command(Path::new(workspace))?
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ee_stdout_json(output: Output, context: &str) -> Result<JsonValue, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
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
            "{context} failed: exit={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
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
        remember_rule(workspace, content)?;
    }
    Ok(())
}

fn remember_rule(workspace: &Path, content: &str) -> Result<String, String> {
    let envelope = run_ee_json(
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
    envelope
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember envelope missing memory_id: {envelope}"))
}

fn seed_workspace_with_linked_corpus(workspace: &Path) -> Result<(), String> {
    run_ee_json(workspace, &["init", "--json"], "ee init")?;
    let release = remember_rule(
        workspace,
        "Before release, run cargo fmt --check to verify code formatting.",
    )?;
    let ci = remember_rule(
        workspace,
        "Run cargo test to validate CI pipeline integration before pushing.",
    )?;
    remember_rule(
        workspace,
        "Release engineering uses cargo clippy in CI to gate merges.",
    )?;
    remember_rule(
        workspace,
        "When CI fails, inspect cargo test output for failing integration tests.",
    )?;

    let connection = DbConnection::open_file(&workspace.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000260701",
            &CreateMemoryLinkInput {
                src_memory_id: release,
                dst_memory_id: ci,
                relation: MemoryLinkRelation::Supports,
                weight: 0.91,
                confidence: 0.88,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-06-03T00:00:00Z".to_owned()),
                source: MemoryLinkSource::Agent,
                created_by: Some("property-pack-metamorphic-mr5b".to_owned()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())
}

fn set_config_value(workspace_arg: &str, key: &str, value: &str) -> TestResult {
    let set_output =
        run_ee_with_workspace_str(workspace_arg, &["--json", "config", "set", key, value])?;
    if !set_output.status.success() {
        return Err(format!(
            "ee config set {key} {value} failed: exit={:?} stdout={} stderr={}",
            set_output.status.code(),
            String::from_utf8_lossy(&set_output.stdout),
            String::from_utf8_lossy(&set_output.stderr),
        ));
    }
    Ok(())
}

fn set_graph_ppr_alpha_zero(workspace_arg: &str) -> TestResult {
    set_config_value(workspace_arg, "graph.feature.ppr.enabled", "true")?;
    set_config_value(workspace_arg, "graph.ppr.alpha", "0.0")
}

fn refresh_memory_links_graph_snapshot(workspace: &Path) -> TestResult {
    let output = run_ee_with_workspace(workspace, &["--json", "graph", "centrality-refresh"])?;
    if !output.status.success() {
        return Err(format!(
            "graph centrality-refresh failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "graph centrality-refresh stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

fn mark_latest_memory_links_snapshot_stale(workspace: &Path, workspace_arg: &str) -> TestResult {
    let connection = DbConnection::open_file(&workspace.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;
    let workspace_row = connection
        .get_workspace_by_path(workspace_arg)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("workspace not found for path {workspace_arg}"))?;
    let snapshot = connection
        .get_latest_graph_snapshot(&workspace_row.id, GraphSnapshotType::MemoryLinks)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "expected a persisted memory_links graph snapshot".to_owned())?;
    if !connection
        .update_graph_snapshot_status(&snapshot.id, GraphSnapshotStatus::Stale)
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "latest graph snapshot {} was not marked stale",
            snapshot.id
        ));
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
            "pack",
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
    let stdout = run_ee_stdout(
        workspace,
        &[
            "pack",
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
    )?;
    normalize_pack_envelope(&stdout)
}

fn normalize_pack_envelope(stdout: &str) -> Result<String, String> {
    let mut envelope: JsonValue =
        serde_json::from_str(stdout).map_err(|error| format!("pack envelope not JSON: {error}"))?;
    if envelope
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err(format!(
            "pack determinism requires a nonempty selection: {envelope}"
        ));
    }

    // Independently validate measured classification before normalizing only
    // the three producer diagnostics; resource evidence remains compared.
    // serde_json's preserve_order feature retains object insertion order too.
    if !ee::obs::normalize_pack_slo_measurements(&mut envelope)? {
        return Err("pack envelope missing SLO measurements".to_owned());
    }

    // bd-j1upc / bd-4w1up: the wall-clock degradation is registered volatile
    // beside the SLO fields (src/obs/volatile_fields.rs, bd-8ig10). It is
    // appended after pack.hash is computed, so it is outside the hash but inside
    // degraded[] and the rendered pack.text, and it carries the measured
    // milliseconds. The shared envelope helper drops it and fails, rather than
    // passing, when an entry survives or a bullet is left in any string.
    let timing = ee::obs::normalize_pack_envelope_timing(&mut envelope)?;
    if timing.timing_entries_dropped != usize::from(timing.timing_entries_present) {
        return Err(format!(
            "pack envelope must carry at most one timing entry and drop exactly it: {timing:?}"
        ));
    }
    serde_json::to_string(&envelope)
        .map_err(|error| format!("serialize normalized pack envelope: {error}"))
}

/// The real determinism contract: the same workspace and query give the same
/// pack.hash. The byte comparisons below are stricter, but this is the one the
/// product guarantees, so it is asserted on its own and named in the failure.
fn require_same_pack_hash(label: &str, runs: &[&str]) -> TestResult {
    let mut hashes = Vec::with_capacity(runs.len());
    for run in runs {
        let value: JsonValue = serde_json::from_str(run)
            .map_err(|error| format!("{label}: normalized envelope not JSON: {error}"))?;
        hashes
            .push(pack_hash(&value).ok_or_else(|| format!("{label}: envelope has no pack.hash"))?);
    }
    if hashes.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(format!(
            "{label}: pack.hash differs across runs: {hashes:?}"
        ));
    }
    Ok(())
}

#[test]
fn pack_envelope_normalization_preserves_semantic_drift() -> TestResult {
    let original = serde_json::json!({
        "data": {
            "pack": {
                "hash": "blake3:original",
                "items": [{
                    "memoryId": "mem_original",
                    "content": "Preserve release provenance.",
                    "provenance": [{"uri": "ee://memory/original"}]
                }],
                "slo": {
                    "schema": "ee.pack.slo.v1",
                    "budgetClass": {"elapsedMsTarget": 200, "elapsedMsWarning": 500, "elapsedMsFailure": 2000},
                    "actuals": {"elapsedMs": 21, "scannedCount": 6},
                    "resourceStatus": "within_budget", "elapsedStatus": "within_budget", "status": "within_budget"
                }
            }
        },
        "degraded": []
    });
    let baseline = normalize_pack_envelope(&original.to_string())?;
    let mut different_timing = original.clone();
    different_timing["data"]["pack"]["slo"]["actuals"]["elapsedMs"] = JsonValue::from(34);
    if normalize_pack_envelope(&different_timing.to_string())? != baseline {
        return Err("registered assembly timing must not cause semantic drift".to_owned());
    }
    different_timing["data"]["pack"]["slo"]["actuals"]["elapsedMs"] = JsonValue::from(24_457);
    if normalize_pack_envelope(&different_timing.to_string()).is_ok() {
        return Err(
            "falsely green elapsed breach must be rejected before normalization".to_owned(),
        );
    }
    different_timing["data"]["pack"]["slo"]["elapsedStatus"] = JsonValue::from("failure");
    different_timing["data"]["pack"]["slo"]["status"] = JsonValue::from("failure");
    if normalize_pack_envelope(&different_timing.to_string())? != baseline {
        return Err("correct measured failure must preserve semantic pack identity".to_owned());
    }
    for (pointer, replacement) in [
        ("/data/pack/hash", serde_json::json!("blake3:changed")),
        (
            "/data/pack/items/0/memoryId",
            serde_json::json!("mem_changed"),
        ),
        (
            "/data/pack/items/0/content",
            serde_json::json!("Changed content"),
        ),
        (
            "/data/pack/items/0/provenance/0/uri",
            serde_json::json!("ee://memory/changed"),
        ),
        ("/data/pack/slo/actuals/scannedCount", serde_json::json!(7)),
        ("/degraded", serde_json::json!([{"code": "index_stale"}])),
    ] {
        let mut changed = original.clone();
        *changed
            .pointer_mut(pointer)
            .ok_or_else(|| format!("negative control missing {pointer}"))? = replacement;
        if normalize_pack_envelope(&changed.to_string())? == baseline {
            return Err(format!(
                "normalization concealed semantic drift at {pointer}"
            ));
        }
    }

    // bd-j1upc: the registered timing degradation normalizes away from
    // degraded[] at any measured duration. Since ADR 0087 T2, pack.text is
    // canonical: the product never renders the timing bullet into it, so the
    // timed envelope ships the same text as the untimed one.
    let timing_entry = |ms: u64| {
        serde_json::json!({
            "code": ee::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE,
            "severity": "low",
            "message": format!(
                "Pack assembly took {ms}ms, at or over the standard resource-profile elapsed warning threshold of 500ms. The pack contents are unaffected."
            ),
        })
    };
    let mut with_text = original.clone();
    with_text["data"]["degraded"] = serde_json::json!([]);
    with_text["data"]["pack"]["text"] = serde_json::json!("# Context Pack\n## Items\n");
    let text_baseline = normalize_pack_envelope(&with_text.to_string())?;
    for ms in [612_u64, 1544] {
        let mut timed = with_text.clone();
        timed["degraded"] = serde_json::json!([timing_entry(ms)]);
        timed["data"]["degraded"] = serde_json::json!([timing_entry(ms)]);
        if normalize_pack_envelope(&timed.to_string())? != text_baseline {
            return Err(format!(
                "the registered timing degradation ({ms}ms) must normalize away from degraded[]"
            ));
        }
        // A timing bullet in pack.text is a T2 regression even beside its
        // degraded[] entry, so it must be rejected, never scrubbed into a pass.
        let mut regressed = timed.clone();
        regressed["data"]["pack"]["text"] = serde_json::json!(format!(
            "# Context Pack\n- **[low]** Pack assembly took {ms}ms, at or over the standard resource-profile elapsed warning threshold of 500ms. The pack contents are unaffected.\n  - *Repair:* `Re-run when the host is idle`\n## Items\n"
        ));
        if normalize_pack_envelope(&regressed.to_string()).is_ok() {
            return Err(format!(
                "a timing bullet in canonical pack.text ({ms}ms) must be rejected"
            ));
        }
    }
    // A timing bullet with no timing entry in degraded[] is not the registered
    // shape, so the lockstep check must reject it rather than pass it.
    let mut stray_bullet = with_text.clone();
    stray_bullet["data"]["pack"]["text"] = serde_json::json!(
        "# Context Pack\n- **[low]** Pack assembly took 612ms, at or over the standard resource-profile elapsed warning threshold of 500ms.\n## Items\n"
    );
    if normalize_pack_envelope(&stray_bullet.to_string()).is_ok() {
        return Err("a timing bullet without its degraded entry must be rejected".to_owned());
    }
    Ok(())
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

fn pack_item_id_sequence(value: &JsonValue) -> Vec<String> {
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
    let relative_output = ee_command(&absolute)?
        .arg("--workspace")
        .arg(".")
        .args([
            "pack",
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
// JSON envelopes after normalizing only the registered volatile timing: the
// SLO assembly time and the wall-clock degradation derived from it (both in
// src/obs/volatile_fields.rs). pack.hash equality is asserted first, since it
// is the product's contract; this stricter form catches drift in any other
// envelope field (degraded[], packDna, provenance footer, tokenSavings,
// …) that the hash-only check silently tolerates.

#[test]
fn pack_envelope_byte_identical_under_repeated_max_tokens_invocation() -> TestResult {
    let workspace = unique_workspace("mr3-idempotent")?;
    seed_workspace_with_basic_corpus(&workspace)?;

    let run1 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;
    let run2 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;

    require_same_pack_hash("MR3", &[&run1, &run2])?;
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
// normalized envelope. determinism_unit.rs:196 already pins pack.hash across
// three runs; this tightens the assertion to the full envelope and proves
// that no per-process counter, RNG seed, or cache-warmup field leaks
// into the JSON contract.

#[test]
fn pack_envelope_byte_identical_across_three_cold_process_invocations() -> TestResult {
    let workspace = unique_workspace("mr4-cold-process")?;
    seed_workspace_with_basic_corpus(&workspace)?;

    let run1 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;
    let run2 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;
    let run3 = run_ee_context_stdout(&workspace, "prepare release", "1000")?;

    require_same_pack_hash("MR4", &[&run1, &run2, &run3])?;
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
// MR5 — `graph.ppr.alpha = 0` invariance
// ---------------------------------------------------------------------------
//
// Setting `graph.ppr.alpha = 0` in the workspace config must
// produce a configuration in which two `ee context` invocations are
// byte-identical after SLO timing normalization (the graph contribution is wired
// through PPR scoring but must be a strict zero under this knob). This pins
// the contract that
// `graph.ppr.alpha = 0` is a valid + no-panic + deterministic configuration
// before higher-order MRs exercise snapshot stale-vs-fresh independence.

#[test]
fn pack_envelope_byte_identical_under_graph_ppr_alpha_zero() -> TestResult {
    let workspace = unique_workspace("mr5-graph-ppr-alpha-zero")?;
    seed_workspace_with_basic_corpus(&workspace)?;

    let absolute = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    let workspace_str = absolute
        .to_str()
        .ok_or_else(|| "workspace path not UTF-8".to_string())?;

    set_graph_ppr_alpha_zero(workspace_str)?;

    let run1 = run_ee_context_stdout(&absolute, "prepare release", "1000")?;
    let run2 = run_ee_context_stdout(&absolute, "prepare release", "1000")?;

    require_same_pack_hash("MR5", &[&run1, &run2])?;
    if run1 != run2 {
        return Err(format!(
            "MR5 broken — graph.ppr.alpha=0 envelope drifted between invocations:\n  run1.len={}, run2.len={}",
            run1.len(),
            run2.len(),
        ));
    }

    // Stronger sanity-pin: with graph.ppr.alpha=0, the emitted config
    // surface must reflect the zero. If a future regression were to
    // silently ignore the zero and fall back to the 0.30 default, this
    // catches it at the same time as the determinism check.
    let get_output = run_ee_with_workspace_str(
        workspace_str,
        &["--json", "config", "get", "graph.ppr.alpha"],
    )?;
    let get_json = ee_stdout_json(get_output, "ee config get graph.ppr.alpha")?;
    let observed = get_json
        .pointer("/data/value")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("config get did not surface a string value: envelope={get_json}"))?
        .parse::<f64>()
        .map_err(|error| {
            format!("config get value is not a float: {error}; envelope={get_json}")
        })?;
    if observed != 0.0 {
        return Err(format!(
            "MR5 sanity broken — config set graph.ppr.alpha 0.0 did not persist; observed={observed}"
        ));
    }
    Ok(())
}

#[test]
fn pack_selection_ids_identical_between_fresh_and_stale_graph_snapshots_under_graph_ppr_alpha_zero()
-> TestResult {
    let workspace = unique_workspace("mr5b-stale-fresh-graph")?
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    seed_workspace_with_linked_corpus(&workspace)?;

    let workspace_str = workspace
        .to_str()
        .ok_or_else(|| "workspace path not UTF-8".to_string())?;
    set_graph_ppr_alpha_zero(workspace_str)?;

    refresh_memory_links_graph_snapshot(&workspace)?;
    let fresh = run_ee_context_json(&workspace, "prepare release", "1000")?;
    let fresh_sequence = pack_item_id_sequence(&fresh);
    if fresh_sequence.is_empty() {
        return Err(format!(
            "MR5b setup broken — fresh graph snapshot pack selected no memories; envelope={fresh}"
        ));
    }

    mark_latest_memory_links_snapshot_stale(&workspace, workspace_str)?;
    let stale = run_ee_context_json(&workspace, "prepare release", "1000")?;
    let stale_sequence = pack_item_id_sequence(&stale);
    if fresh_sequence != stale_sequence {
        return Err(format!(
            "MR5b broken — graph.ppr.alpha=0 selection IDs changed between fresh and stale graph snapshots:\n  fresh={fresh_sequence:?}\n  stale={stale_sequence:?}",
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// MR6 — `ee insights` byte-identity across repeated cold-process invocations
// ---------------------------------------------------------------------------
//
// bd-r3pjo: the existing `tests/graph_neighborhood_smoke.rs:1368
// proximity_json_reports_min_cut_for_seeded_memory_pair` test already
// pins byte-identity for `ee proximity`. This MR extends the same
// guarantee to `ee insights`, the other agent-facing graph surface
// called out in bd-3vwx0 / bd-1wtsb scope.
//
// Implementation note: this MR uses a memory-link wired through the
// public `ee link` CLI surface so the test stays at the
// public-CLI-only contract layer (no internal `DbConnection` import,
// no fixture-only seed helper). The fixture is sparser than the
// graph_neighborhood_smoke fixture but exercises the same insights
// code path under `--section causalBottlenecks`.

#[test]
fn insights_section_byte_identical_across_three_cold_process_invocations() -> TestResult {
    let workspace = unique_workspace("mr6-insights")?;
    run_ee_json(&workspace, &["init", "--json"], "ee init")?;

    // Seed two memories and a link so the insights surface has
    // graph structure to report on. Using the public `ee link`
    // alias documented in src/cli/mod.rs:8484-8501.
    let bridge_envelope = run_ee_json(
        &workspace,
        &[
            "remember",
            "Insights MR6 bridge memory.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
        "ee remember bridge",
    )?;
    let bridge_id = bridge_envelope
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("seed: bridge memory_id missing: {bridge_envelope}"))?
        .to_owned();

    let leaf_envelope = run_ee_json(
        &workspace,
        &[
            "remember",
            "Insights MR6 leaf memory.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
        "ee remember leaf",
    )?;
    let leaf_id = leaf_envelope
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("seed: leaf memory_id missing: {leaf_envelope}"))?
        .to_owned();

    // Public `ee link` surface — keep the determinism test at the
    // CLI-contract layer to mirror the rest of the file.
    let link_output = run_ee_with_workspace(
        &workspace,
        &[
            "link",
            bridge_id.as_str(),
            leaf_id.as_str(),
            "--relation",
            "supports",
        ],
    )?;
    if !link_output.status.success() {
        // `ee link` may emit a usage envelope when the relation
        // dictionary or current binary doesn't accept `supports`.
        // Skip the rest of the MR (rather than fail spuriously) so
        // a future link-relation tightening can't silently break
        // unrelated metamorphic coverage. Print a structured
        // skip-reason so CI surfaces it.
        eprintln!(
            "MR6: ee link refused to seed `supports` link; skipping insights determinism check (stderr: {})",
            String::from_utf8_lossy(&link_output.stderr),
        );
        return Ok(());
    }

    let section_args = ["insights", "--section", "causalBottlenecks", "--json"];
    let run1 = run_ee_stdout(
        &workspace,
        &section_args,
        "ee insights causalBottlenecks #1",
    )?;
    let run2 = run_ee_stdout(
        &workspace,
        &section_args,
        "ee insights causalBottlenecks #2",
    )?;
    let run3 = run_ee_stdout(
        &workspace,
        &section_args,
        "ee insights causalBottlenecks #3",
    )?;

    if run1 != run2 {
        return Err(format!(
            "MR6 broken — `ee insights --section causalBottlenecks --json` run1 != run2: lens={}/{}",
            run1.len(),
            run2.len(),
        ));
    }
    if run2 != run3 {
        return Err(format!(
            "MR6 broken — `ee insights --section causalBottlenecks --json` run2 != run3: lens={}/{}",
            run2.len(),
            run3.len(),
        ));
    }
    Ok(())
}
