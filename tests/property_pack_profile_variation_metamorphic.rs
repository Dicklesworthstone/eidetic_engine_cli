//! bd-1q6r7 — metamorphic relations for `ee pack` / `ee context`
//! assembly determinism across resource-profile and pack-profile
//! variation.
//!
//! Companion to:
//!  - `tests/property_pack_metamorphic.rs` (bd-2m607 — workspace
//!    alias, tag list order, max-tokens idempotency, graph_weight=0).
//!  - `tests/determinism_unit.rs` (pack-hash reproducibility across
//!    three identical invocations).
//!
//! This file pins four MRs the existing harnesses do not cover:
//!
//! - **MR1 — same pack-profile is fully deterministic.** For each
//!   built-in pack profile (`balanced` and `thorough`), running the
//!   same `ee context` query against the same workspace must produce
//!   byte-identical selected memory_id lists across two cold-process
//!   re-runs. Drift here would surface non-determinism in the MMR or
//!   facility-location tiebreakers downstream of the workspace-id
//!   seed.
//!
//! - **MR2 — resource-profile does not change selection.** A
//!   `--resource-profile lean` and `--resource-profile swarm_heavy`
//!   run of the same query (with all other flags equal) must produce
//!   the SAME selected memory_id list and the same pack hash. The
//!   resource profile governs runtime SLOs (cancellation budgets,
//!   reserved memory, candidate-pool caps), not the selection
//!   algorithm — if it leaks into the pack content the determinism
//!   contract is broken.
//!
//! - **MR3 — candidate-pool growth preserves the selected set.** For
//!   a fixed query and budget, increasing `--candidate-pool` from N
//!   to 2N (with the corpus comfortably exceeding 2N candidates) must
//!   not OMIT memories that were selected at pool size N. The
//!   resulting selection set must be a SUPERSET of the smaller pool's
//!   selection (or equal — equality is the common case when the
//!   token budget binds before the pool size). Drift here would
//!   surface a candidate-pool truncation bias that drops near-tie
//!   high-relevance candidates as the pool widens.
//!
//! - **MR4 — `--max-tokens` is the binding budget, idempotent on
//!   re-run.** Two back-to-back invocations with `--max-tokens 4000`
//!   against the same workspace must produce byte-identical packHash
//!   values. This duplicates the spirit of
//!   `tests/determinism_unit.rs:196` but pins the assertion on the
//!   specific `--max-tokens 4000` envelope shape the bd-1q6r7 spec
//!   calls out.
//!
//! Each MR runs `ee` as a child process so cross-process state leaks
//! surface even when single-process library tests would not.

#![forbid(unsafe_code)]
#![allow(clippy::expect_used)]

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
        .join("ee-metamorphic-pack-profile-variation")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;
    Ok(workspace)
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

fn run_ee_json(workspace: &Path, args: &[&str], context: &str) -> Result<JsonValue, String> {
    ee_stdout_json(run_ee(workspace, args)?, context)
}

fn seed_workspace(workspace: &Path) -> TestResult {
    run_ee_json(workspace, &["init", "--json"], "ee init")?;
    let memories = [
        "Before release, run cargo fmt --check to verify code formatting.",
        "Run cargo test to validate CI pipeline integration before pushing.",
        "Release engineering uses cargo clippy in CI to gate merges.",
        "When CI fails, inspect cargo test output for failing integration tests.",
        "Database index rebuild must finish before release candidate sign-off.",
        "Agent handoff: preserve provenance fields before pack assembly.",
        "Performance gates: track p95 latency on the pack-assembly hot path.",
        "Document the release sequence in the runbook before pushing.",
        "Index migration must complete before the next pack-quality eval.",
        "Always re-run the integration suite after a dependency bump.",
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

fn context_json(
    workspace: &Path,
    query: &str,
    profile: &str,
    candidate_pool: &str,
    max_tokens: &str,
    resource_profile: Option<&str>,
) -> Result<JsonValue, String> {
    let mut args: Vec<&str> = vec![
        "context",
        query,
        "--profile",
        profile,
        "--candidate-pool",
        candidate_pool,
        "--max-tokens",
        max_tokens,
        "--json",
    ];
    if let Some(profile) = resource_profile {
        args.push("--resource-profile");
        args.push(profile);
    }
    run_ee_json(workspace, &args, "ee context")
}

fn selected_memory_ids(envelope: &JsonValue) -> Vec<String> {
    envelope
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.pointer("/memory_id")
                        .or_else(|| item.pointer("/memoryId"))
                        .and_then(JsonValue::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pack_hash(envelope: &JsonValue) -> Option<String> {
    envelope
        .pointer("/data/pack/hash")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// MR1 — pack-profile re-run determinism
// ---------------------------------------------------------------------------

#[test]
fn pack_profile_selection_is_deterministic_across_cold_re_run() -> TestResult {
    let workspace = unique_workspace("mr1-pack-profile-rerun")?;
    seed_workspace(&workspace)?;

    for profile in ["balanced", "thorough"] {
        let run_a = context_json(&workspace, "prepare release", profile, "20", "1500", None)?;
        let run_b = context_json(&workspace, "prepare release", profile, "20", "1500", None)?;
        let ids_a = selected_memory_ids(&run_a);
        let ids_b = selected_memory_ids(&run_b);
        if ids_a != ids_b {
            return Err(format!(
                "MR1 broken — `--profile {profile}` re-run produced a different memory_id sequence:\n  run_a={ids_a:?}\n  run_b={ids_b:?}",
            ));
        }
        let hash_a = pack_hash(&run_a);
        let hash_b = pack_hash(&run_b);
        if hash_a != hash_b {
            return Err(format!(
                "MR1 broken — `--profile {profile}` re-run produced a different pack hash:\n  hash_a={hash_a:?}\n  hash_b={hash_b:?}",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR2 — resource-profile invariance over selection
// ---------------------------------------------------------------------------

#[test]
fn resource_profile_does_not_change_pack_selection_or_hash() -> TestResult {
    let workspace = unique_workspace("mr2-resource-profile-invariance")?;
    seed_workspace(&workspace)?;

    let lean = context_json(
        &workspace,
        "prepare release",
        "balanced",
        "20",
        "1500",
        Some("lean"),
    )?;
    let swarm_heavy = context_json(
        &workspace,
        "prepare release",
        "balanced",
        "20",
        "1500",
        Some("swarm_heavy"),
    )?;

    let ids_lean = selected_memory_ids(&lean);
    let ids_swarm = selected_memory_ids(&swarm_heavy);
    if ids_lean != ids_swarm {
        return Err(format!(
            "MR2 broken — resource-profile changes selected memory_id sequence:\n  lean={ids_lean:?}\n  swarm_heavy={ids_swarm:?}",
        ));
    }
    let hash_lean = pack_hash(&lean);
    let hash_swarm = pack_hash(&swarm_heavy);
    if hash_lean != hash_swarm {
        return Err(format!(
            "MR2 broken — resource-profile changes pack hash:\n  lean={hash_lean:?}\n  swarm_heavy={hash_swarm:?}",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR3 — candidate-pool growth preserves (or grows) the selection
// ---------------------------------------------------------------------------

#[test]
fn candidate_pool_growth_does_not_drop_previously_selected_memories() -> TestResult {
    let workspace = unique_workspace("mr3-candidate-pool-monotonic")?;
    seed_workspace(&workspace)?;

    let small = context_json(&workspace, "prepare release", "balanced", "5", "1500", None)?;
    let large = context_json(
        &workspace,
        "prepare release",
        "balanced",
        "10",
        "1500",
        None,
    )?;

    let small_ids: BTreeSet<String> = selected_memory_ids(&small).into_iter().collect();
    let large_ids: BTreeSet<String> = selected_memory_ids(&large).into_iter().collect();

    if small_ids.is_empty() {
        // No selection at the smaller pool — the MR is vacuously
        // preserved (nothing to subset).
        return Ok(());
    }
    let dropped: Vec<&String> = small_ids.difference(&large_ids).collect();
    if !dropped.is_empty() {
        return Err(format!(
            "MR3 broken — growing --candidate-pool dropped previously selected memories:\n  dropped={dropped:?}\n  small_ids={small_ids:?}\n  large_ids={large_ids:?}",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR4 — `--max-tokens 4000` re-run idempotency
// ---------------------------------------------------------------------------

#[test]
fn max_tokens_4000_packhash_is_idempotent_on_re_run() -> TestResult {
    let workspace = unique_workspace("mr4-max-tokens-idempotent")?;
    seed_workspace(&workspace)?;

    let first = context_json(
        &workspace,
        "prepare release",
        "balanced",
        "20",
        "4000",
        None,
    )?;
    let second = context_json(
        &workspace,
        "prepare release",
        "balanced",
        "20",
        "4000",
        None,
    )?;

    let hash_a = pack_hash(&first).ok_or_else(|| {
        format!("MR4 first run envelope missing /data/pack/hash; envelope={first}")
    })?;
    let hash_b = pack_hash(&second).ok_or_else(|| {
        format!("MR4 second run envelope missing /data/pack/hash; envelope={second}")
    })?;

    if hash_a != hash_b {
        return Err(format!(
            "MR4 broken — `--max-tokens 4000` re-run produced a different pack hash:\n  hash_a={hash_a}\n  hash_b={hash_b}",
        ));
    }
    Ok(())
}
