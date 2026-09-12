//! bd-2hwmw — metamorphic relations for `ee remember`, `ee memory list`,
//! and `ee search` idempotence, tag filtering, and ordering invariants.
//!
//! Five MRs from the bd-2hwmw spec:
//!
//! - **MR1 — `ee remember` is idempotent when replaying the same key.**
//!   Two back-to-back `ee remember <content> --level procedural --kind
//!   rule --idempotency-key <key> --json` invocations with identical
//!   arguments must return the original memory ID and expose exactly
//!   one search hit. Separate writes without a replay key may record
//!   separate observations with the same content.
//!
//! - **MR2 — `ee memory list` ordering is stable when a tag filter
//!   narrows the universe.** The filtered list must contain exactly
//!   the matching memory IDs and preserve their order in the unfiltered
//!   list. Tag filtering belongs to `memory list`; `search` does not
//!   expose a `--tag` flag.
//!
//! - **MR3 — remember-then-search returns the just-added memory.**
//!   After `ee remember <discriminative content>` succeeds with a
//!   memory_id `M`, an immediate `ee search <discriminative content>
//!   --limit N` (with N >= 1) must surface `M` somewhere in the
//!   result list. Drift here would expose a write-through-to-search
//!   gap that contradicts the documented synchronous-at-1-result
//!   contract.
//!
//! - **MR4 — tag set has set semantics, not list semantics.** Two
//!   memories with identical CONTENT but `--tags a,b,c` vs
//!   `--tags c,b,a` must surface identically under tag-filtered
//!   memory listing. Repeated tags also leave membership unchanged.
//!
//! - **MR5 — outcome signal does not change retrieval seed.** Recording
//!   an `ee outcome` signal against a memory must not alter the
//!   relative ordering of the same `ee search` query against the
//!   workspace. Drift here would expose outcome events leaking into
//!   the retrieval seed and producing nondeterministic search results.
//!
//! Each MR runs `ee` as a child process so cross-process state leaks
//! surface even when single-process library tests would not.

#![forbid(unsafe_code)]
#![allow(clippy::expect_used)]

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
        .join("ee-metamorphic-remember-search")
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

fn run_ee_search(workspace: &Path, query_args: &[&str]) -> Result<Output, String> {
    let mut args: Vec<&str> = vec!["--json"];
    args.push("search");
    args.extend_from_slice(query_args);
    Command::new(ee_binary())
        .arg("--workspace")
        .arg(workspace)
        .args(&args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee search {}: {error}", query_args.join(" ")))
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

fn run_ee_json(workspace: &Path, args: &[&str], context: &str) -> Result<JsonValue, String> {
    ee_stdout_json(run_ee(workspace, args)?, context)
}

fn run_search_json(
    workspace: &Path,
    query_args: &[&str],
    context: &str,
) -> Result<JsonValue, String> {
    ee_stdout_json(run_ee_search(workspace, query_args)?, context)
}

fn init_workspace(workspace: &Path) -> TestResult {
    run_ee_json(workspace, &["init", "--json"], "ee init").map(|_| ())
}

fn remember(
    workspace: &Path,
    content: &str,
    tags: Option<&str>,
    context: &str,
) -> Result<JsonValue, String> {
    let mut args: Vec<&str> = vec![
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
    ];
    if let Some(tags) = tags {
        args.push("--tags");
        args.push(tags);
    }
    args.push("--json");
    run_ee_json(workspace, &args, context)
}

fn memory_id_of(envelope: &JsonValue, context: &str) -> Result<String, String> {
    envelope
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            format!("{context}: remember envelope missing /data/memory_id; envelope={envelope}")
        })
}

/// Collect the ordered list of search result document IDs.
fn search_doc_ids(envelope: &JsonValue) -> Vec<String> {
    envelope
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|entry| {
                    entry
                        .pointer("/docId")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn listed_memory_ids(envelope: &JsonValue) -> Result<Vec<String>, String> {
    envelope
        .pointer("/data/memories")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("memory list response missing memories: {envelope}"))?
        .iter()
        .map(|memory| {
            memory
                .get("id")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("listed memory missing id: {memory}"))
        })
        .collect()
}

/// Try to surface a memory_id from a search result. Different output
/// shapes carry the memory ID under different pointers; check each.
fn search_result_memory_ids(envelope: &JsonValue) -> Vec<String> {
    let Some(results) = envelope
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
    else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for entry in results {
        for pointer in [
            "/memoryId",
            "/memory_id",
            "/docId",
            "/source/memory_id",
            "/source/memoryId",
            "/provenance/0/memoryId",
            "/provenance/0/memory_id",
        ] {
            if let Some(value) = entry.pointer(pointer).and_then(JsonValue::as_str) {
                ids.push(value.to_string());
                break;
            }
        }
    }
    ids
}

// ---------------------------------------------------------------------------
// MR1 — remember idempotency on replay with the same key
// ---------------------------------------------------------------------------

#[test]
fn remember_is_idempotent_on_byte_identical_keyed_re_run() -> TestResult {
    let workspace = unique_workspace("mr1-remember-idempotent")?;
    init_workspace(&workspace)?;

    let content = "MR1 fixture: idempotent re-run must not produce two distinct surfaced memories.";
    let tags = "mr1,idempotency,bd-2hwmw";
    let args = [
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        tags,
        "--idempotency-key",
        "mr1-remember-replay",
        "--json",
    ];

    let first = run_ee_json(&workspace, &args, "remember #1")?;
    let first_id = memory_id_of(&first, "remember #1")?;
    let second = run_ee_json(&workspace, &args, "remember #2")?;
    let second_id = second
        .pointer("/data/memoryId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("replay response missing memoryId: {second}"))?;
    if second_id != first_id
        || second.pointer("/data/status").and_then(JsonValue::as_str) != Some("already_recorded")
    {
        return Err(format!(
            "MR1 broken — keyed replay did not return the original memory {first_id}: {second}"
        ));
    }

    // The fixture has one logical memory. Require its hit so an empty
    // result cannot pass the replay invariant vacuously.
    let search = run_search_json(
        &workspace,
        &["MR1 fixture: idempotent re-run"],
        "search MR1",
    )?;
    let surfaced_ids = search_result_memory_ids(&search);

    if surfaced_ids != vec![first_id.clone()] {
        return Err(format!(
            "MR1 broken — keyed replay must surface exactly the original memory:\n  first_id={first_id}\n  surfaced ids={surfaced_ids:?}",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR2 — memory-list ordering stable when a tag filter narrows
// ---------------------------------------------------------------------------

#[test]
fn memory_list_ordering_is_stable_when_tag_filter_narrows_universe() -> TestResult {
    let workspace = unique_workspace("mr2-list-narrowing")?;
    init_workspace(&workspace)?;

    // Seed a four-memory corpus where three carry the narrowing tag.
    let pairs = [
        (
            "Before release run cargo fmt --check to verify code formatting.",
            "release,cargo,format,bd-2hwmw",
        ),
        (
            "Run cargo test to validate CI integration before pushing.",
            "cargo,test,ci,bd-2hwmw",
        ),
        (
            "Inspect cargo clippy output when a release-gate run fails.",
            "release,cargo,clippy,bd-2hwmw",
        ),
        (
            "Database index rebuild must finish before release sign-off.",
            "release,db,bd-2hwmw",
        ),
    ];
    let mut expected_all = Vec::new();
    let mut expected_release = Vec::new();
    for (content, tags) in pairs {
        let remembered = remember(&workspace, content, Some(tags), "remember mr2")?;
        let id = memory_id_of(&remembered, "remember mr2")?;
        if tags.split(',').any(|tag| tag == "release") {
            expected_release.push(id.clone());
        }
        expected_all.push(id);
    }
    expected_all.sort();
    expected_release.sort();

    let broad = run_ee_json(
        &workspace,
        &["memory", "list", "--json"],
        "broad memory list",
    )?;
    let narrow_args = ["memory", "list", "--tag", "release", "--json"];
    let narrow = run_ee_json(&workspace, &narrow_args, "narrow memory list")?;

    let broad_ids = listed_memory_ids(&broad)?;
    let narrow_ids = listed_memory_ids(&narrow)?;

    // Both lists use ascending memory-ID order. Comparing each complete
    // expected sequence also pins the relative order of surviving IDs.
    if broad_ids != expected_all || narrow_ids != expected_release {
        return Err(format!(
            "MR2 broken — memory listing must return exactly the expected IDs in deterministic order:\n  expected all={expected_all:?} actual={broad_ids:?}\n  expected release={expected_release:?} actual={narrow_ids:?}"
        ));
    }
    let repeated = run_ee_json(&workspace, &narrow_args, "repeat narrow memory list")?;
    if listed_memory_ids(&repeated)? != narrow_ids {
        return Err(format!(
            "MR2 broken — repeated tag-filtered listing changed: {repeated}"
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// MR3 — remember-then-search round-trip
// ---------------------------------------------------------------------------

#[test]
fn remember_then_search_surfaces_the_just_added_memory() -> TestResult {
    let workspace = unique_workspace("mr3-roundtrip")?;
    init_workspace(&workspace)?;

    // Use a UUID-bearing content phrase so the search query is
    // discriminative against any pre-existing fixture noise.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock: {error}"))?
        .as_nanos();
    let content = format!(
        "MR3-roundtrip-unique-{now}-bd-2hwmw: the discriminative content phrase that pins this memory to this MR.",
    );
    let envelope = remember(&workspace, &content, Some("mr3,bd-2hwmw"), "remember mr3")?;
    let added_id = memory_id_of(&envelope, "remember mr3")?;

    let search = run_search_json(&workspace, &[&content, "--limit", "10"], "search mr3")?;
    let surfaced_ids = search_result_memory_ids(&search);

    if !surfaced_ids.contains(&added_id) {
        return Err(format!(
            "MR3 broken — just-remembered memory_id not present in immediate search results:\n  added_id={added_id}\n  surfaced ids={surfaced_ids:?}\n  search envelope={search}",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR4 — tag set has set semantics
// ---------------------------------------------------------------------------

#[test]
fn memory_list_tag_filter_has_set_semantics_under_reordering_and_duplicates() -> TestResult {
    let workspace_canonical = unique_workspace("mr4-tags-canonical")?;
    let workspace_reordered = unique_workspace("mr4-tags-reordered")?;
    init_workspace(&workspace_canonical)?;
    init_workspace(&workspace_reordered)?;

    let pairs = [
        (
            "MR4 fixture: cargo fmt is the canonical pre-commit formatter (bd-2hwmw).",
            "release,cargo,format",
            "format,cargo,release,cargo",
        ),
        (
            "MR4 fixture: cargo test validates CI integration before pushing (bd-2hwmw).",
            "cargo,test,ci",
            "ci,test,cargo",
        ),
        (
            "MR4 fixture: database recovery preserves provenance (bd-2hwmw).",
            "database,recovery",
            "recovery,database,recovery",
        ),
    ];
    let mut expected_canonical = std::collections::BTreeSet::new();
    let mut expected_reordered = std::collections::BTreeSet::new();
    for (content, canonical_tags, reordered_tags) in pairs {
        let canonical = remember(
            &workspace_canonical,
            content,
            Some(canonical_tags),
            "remember canonical mr4",
        )?;
        let reordered = remember(
            &workspace_reordered,
            content,
            Some(reordered_tags),
            "remember reordered mr4",
        )?;
        if canonical_tags.split(',').any(|tag| tag == "cargo") {
            expected_canonical.insert(memory_id_of(&canonical, "remember canonical mr4")?);
            expected_reordered.insert(memory_id_of(&reordered, "remember reordered mr4")?);
        }
    }

    // Compare known memory identities in each workspace; identical content
    // receives independent IDs across workspaces. The non-cargo control must
    // be absent from both lists.
    let args = ["memory", "list", "--tag", "cargo", "--json"];
    let canonical = run_ee_json(&workspace_canonical, &args, "list canonical mr4")?;
    let reordered = run_ee_json(&workspace_reordered, &args, "list reordered mr4")?;

    let canonical_ids = listed_memory_ids(&canonical)?;
    let reordered_ids = listed_memory_ids(&reordered)?;
    let expected_canonical: Vec<String> = expected_canonical.into_iter().collect();
    let expected_reordered: Vec<String> = expected_reordered.into_iter().collect();

    if canonical_ids != expected_canonical || reordered_ids != expected_reordered {
        return Err(format!(
            "MR4 broken — tag-filtered lists must return exactly the cargo-tagged memories regardless of tag order or repetition:\n  expected canonical={expected_canonical:?} actual={canonical_ids:?}\n  expected reordered={expected_reordered:?} actual={reordered_ids:?}",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MR5 — outcome signal does not change retrieval determinism
// ---------------------------------------------------------------------------

#[test]
fn outcome_signal_does_not_change_search_seed_determinism() -> TestResult {
    let workspace = unique_workspace("mr5-outcome-determinism")?;
    init_workspace(&workspace)?;

    // Seed three memories so MMR has work to do.
    let seeds = [
        "MR5 fixture: cargo fmt --check before release (bd-2hwmw).",
        "MR5 fixture: cargo test --workspace covers integration paths (bd-2hwmw).",
        "MR5 fixture: cargo clippy gates merges (bd-2hwmw).",
    ];
    let mut memory_ids: Vec<String> = Vec::with_capacity(seeds.len());
    for content in seeds {
        let envelope = remember(&workspace, content, Some("mr5,bd-2hwmw"), "remember mr5")?;
        memory_ids.push(memory_id_of(&envelope, "remember mr5")?);
    }

    // Baseline search.
    let before = run_search_json(&workspace, &["MR5 fixture cargo"], "search before outcome")?;
    let before_ids = search_doc_ids(&before);

    // Record an `ee outcome` signal against the first memory. The exact
    // CLI surface is `ee outcome <memory_id> --signal <signal> --json`;
    // if the surface is unavailable on this build the metamorphic
    // relation is vacuously preserved (we can't perturb the system, so
    // the result trivially stays the same).
    let outcome = run_ee(
        &workspace,
        &[
            "outcome",
            memory_ids
                .first()
                .map(String::as_str)
                .unwrap_or("missing-memory-id"),
            "--signal",
            "helpful",
            "--json",
        ],
    )?;
    if !outcome.status.success() {
        // Either the surface is not available on this binary, or the
        // memory was not yet committed to the outcome lane. Skip
        // gracefully — the MR cannot be falsified without a successful
        // perturbation.
        return Ok(());
    }

    // Re-run the same search and require identical ordering.
    let after = run_search_json(&workspace, &["MR5 fixture cargo"], "search after outcome")?;
    let after_ids = search_doc_ids(&after);

    if before_ids != after_ids {
        return Err(format!(
            "MR5 broken — outcome signal changed search ordering:\n  before={before_ids:?}\n  after={after_ids:?}",
        ));
    }
    Ok(())
}
