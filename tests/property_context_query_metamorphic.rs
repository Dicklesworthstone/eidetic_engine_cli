//! bd-28bdg — metamorphic relation tests for `ee context` query phrasing.
//!
//! These pin the property the existing determinism harnesses
//! (`tests/determinism_unit.rs:196`, `tests/e2e_pack_determinism.rs`)
//! cannot pin because they fix the query string: token-equivalent
//! phrasings of the same intent should select the same memory SET
//! against the same workspace.
//!
//! The two MRs we assert here are the strongest that can pass without
//! tolerance bands on top of the hash-embedder fallback:
//!
//! - **MR1 (whitespace invariance)** — `Q`, `" {Q} "`, and `Q` with
//!   collapsed double spaces must select the same memory-id SET.
//! - **MR2 (case invariance)** — `Q`, `Q.to_uppercase()`, and a
//!   mixed-case variant must select the same memory-id SET.
//!
//! Paraphrase invariance (e.g. `"fix release CI"` vs
//! `"fix release pipeline"`) is a weaker, embedder-dependent
//! property; once an MR3 paraphrase fixture lands it can extend
//! this module under documented Jaccard bounds.
//!
//! The tests shell out to the real `ee` binary, seed memories via
//! `ee remember`, and let the search engine fall back to the
//! deterministic hash embedder (no real embedder feature flag set)
//! — same pattern as `tests/search_deterministic_golden.rs`.

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
        .join("ee-metamorphic-context")
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

fn run_ee_json(workspace: &Path, args: &[&str], context: &str) -> Result<JsonValue, String> {
    let output = run_ee(workspace, args)?;
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

/// Seeds the metamorphic-test workspace via the real `ee` binary so
/// the test depends only on the public CLI contract, not on the
/// internal DbConnection schema. Memory content shares the token
/// vocabulary used by the assertions ("release", "ci", "cargo")
/// so the pack actually selects them under any phrasing variant.
fn seed_metamorphic_workspace(workspace: &Path) -> TestResult {
    run_ee_json(workspace, &["init", "--json"], "ee init")?;

    let memories = [
        "Before release, run cargo fmt --check to verify code formatting.",
        "Run cargo test to validate CI pipeline integration before pushing.",
        "Release engineering uses cargo clippy in CI to gate merges.",
        "When CI fails, inspect cargo test output for failing integration tests.",
        "Agent handoff: preserve provenance fields before pack assembly.",
        "Database index rebuild must finish before release candidate sign-off.",
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

/// Runs `ee context <query>` with a generous budget so the pack
/// can include the full seeded fixture if the engine selects it.
/// `--profile thorough` minimizes MMR diversity pressure that would
/// otherwise let rank shuffling shrink the selected SET.
fn run_ee_context(workspace: &Path, query: &str) -> Result<JsonValue, String> {
    run_ee_json(
        workspace,
        &[
            "context",
            query,
            "--max-tokens",
            "4000",
            "--candidate-pool",
            "20",
            "--profile",
            "thorough",
            "--json",
        ],
        &format!("ee context {query:?}"),
    )
}

fn pack_item_ids(value: &JsonValue) -> Vec<String> {
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

fn id_set(value: &JsonValue) -> BTreeSet<String> {
    pack_item_ids(value).into_iter().collect()
}

/// MR1 — leading/trailing whitespace and collapsed inner whitespace
/// must not change which memories the pack selects. FTS5 / BM25
/// tokenize on whitespace, and the hash embedder runs over the same
/// tokenized stream, so any selection drift here exposes a tokenizer
/// or canonicalization regression upstream of search.
#[test]
fn ee_context_whitespace_normalization_preserves_memory_id_set() -> TestResult {
    let workspace = unique_workspace("mr1-whitespace")?;
    seed_metamorphic_workspace(&workspace)?;

    let baseline = run_ee_context(&workspace, "release CI cargo test")?;
    let padded = run_ee_context(&workspace, "   release CI cargo test   ")?;
    let double_spaced = run_ee_context(&workspace, "release  CI  cargo  test")?;
    let tab_indented = run_ee_context(&workspace, "\trelease CI cargo test")?;

    let baseline_ids = pack_item_ids(&baseline);
    if baseline_ids.is_empty() {
        return Err(format!(
            "metamorphic baseline pack was empty; fixture too sparse to test (baseline JSON: {baseline})"
        ));
    }

    let baseline_set = id_set(&baseline);
    let variants = [
        ("padded", &padded),
        ("double_spaced", &double_spaced),
        ("tab_indented", &tab_indented),
    ];
    for (label, variant) in variants {
        let variant_set = id_set(variant);
        if baseline_set != variant_set {
            return Err(format!(
                "MR1 broken — `{label}` variant selected a different memory SET:\n  baseline (sorted): {:?}\n  variant (sorted):  {:?}",
                baseline_set.iter().collect::<Vec<_>>(),
                variant_set.iter().collect::<Vec<_>>(),
            ));
        }
    }
    Ok(())
}

/// MR2 — letter case must not change which memories the pack
/// selects. FTS5 is case-insensitive by default, so a regression
/// here points at either the analyzer config, the hash embedder
/// case-normalization pipeline, or an MMR tie-break that is
/// silently case-sensitive.
#[test]
fn ee_context_case_normalization_preserves_memory_id_set() -> TestResult {
    let workspace = unique_workspace("mr2-case")?;
    seed_metamorphic_workspace(&workspace)?;

    let lower = run_ee_context(&workspace, "release ci cargo test")?;
    let upper = run_ee_context(&workspace, "RELEASE CI CARGO TEST")?;
    let mixed = run_ee_context(&workspace, "Release Ci Cargo Test")?;

    let lower_ids = pack_item_ids(&lower);
    if lower_ids.is_empty() {
        return Err(format!(
            "metamorphic baseline pack was empty; fixture too sparse to test (baseline JSON: {lower})"
        ));
    }

    let lower_set = id_set(&lower);
    let variants = [("upper", &upper), ("mixed", &mixed)];
    for (label, variant) in variants {
        let variant_set = id_set(variant);
        if lower_set != variant_set {
            return Err(format!(
                "MR2 broken — `{label}` variant selected a different memory SET:\n  lower (sorted): {:?}\n  variant (sorted): {:?}",
                lower_set.iter().collect::<Vec<_>>(),
                variant_set.iter().collect::<Vec<_>>(),
            ));
        }
    }
    Ok(())
}
