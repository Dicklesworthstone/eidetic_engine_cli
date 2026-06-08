//! Integration coverage for Telescoping Level-of-Detail (LOD) packing,
//! bd-1n0np.5 (5.1 tiered partition + 5.2 deterministic previews).
//!
//! LOD is on by default in the context pack path. These tests drive a real
//! temp workspace + DB through `run_context_pack` with a tight token budget so
//! the Full / Truncated_Preview tiers both engage, and assert the two
//! load-bearing invariants of the feature:
//!   - a candidate too large for the Full tier is rendered as a DETERMINISTIC
//!     extractive preview (a shorter prefix of its content), while a small
//!     candidate keeps full content;
//!   - the pack hash is byte-stable across identical runs (the #1 LOD caveat:
//!     off-by-one-free accounting must not perturb the pack hash).

use std::fs;
use std::path::{Path, PathBuf};

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, run_context_pack};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::models::MemoryScope;
use ee::pack::ContextResponse;
use ee::search::SpeedMode;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, String>;

const QUERY: &str = "lodfixture release verification";

fn db_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("ee.db")
}

fn remember_fixture(workspace_path: &Path, db_path: &Path, content: &str) -> TestResult<String> {
    let report = remember_memory(&RememberMemoryOptions {
        workspace_path,
        database_path: Some(db_path),
        content,
        workflow_id: None,
        level: "semantic",
        kind: "note",
        tags: Some("lod,e2e"),
        confidence: 0.9,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
        allow_secret_mention: false,
    })
    .map_err(|error| format!("remember fixture failed: {error:?}"))?;
    Ok(report.memory_id.to_string())
}

fn lod_options(workspace_path: &Path, db_path: &Path) -> ContextPackOptions {
    ContextPackOptions {
        task_lens: None,
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: None,
        query: QUERY.to_owned(),
        speed: SpeedMode::Default,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        filters: Default::default(),
        profile: None,
        // Tight budget so the Full tier (~70%) cannot hold the large memory and
        // the preview tier engages.
        max_tokens: Some(100),
        candidate_pool: Some(20),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: ee::models::RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
        persist_pack: false,
        require_fresh_sentinels: false,
        no_lod: false,
    }
}

fn item_content_for(response: &ContextResponse, memory_id: &str) -> Option<String> {
    response
        .data
        .pack
        .items
        .iter()
        .find(|item| item.memory_id.to_string() == memory_id)
        .map(|item| item.content.clone())
}

fn setup() -> TestResult<(TempDir, String, String, String, ContextResponse)> {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    fs::create_dir_all(database_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;

    // Large memory: ~180 distinct tokens, far beyond the Full tier share.
    let large_body = (0..180)
        .map(|index| format!("w{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let large_content = format!("lodfixture release verification {large_body}");
    // Small memory: a handful of tokens that comfortably fit the Full tier.
    let small_content = "lodfixture release verification compact rule";

    let large_id = remember_fixture(&workspace_path, &database_path, &large_content)?;
    let small_id = remember_fixture(&workspace_path, &database_path, small_content)?;

    let response = run_context_pack(&lod_options(&workspace_path, &database_path))
        .map_err(|error| format!("run_context_pack failed: {error:?}"))?;
    Ok((
        temp_dir,
        large_id,
        small_id,
        small_content.to_owned(),
        response,
    ))
}

#[test]
fn lod_pack_renders_full_and_preview_tiers() -> TestResult {
    let (_temp, large_id, small_id, small_content, response) = setup()?;

    let large_item = item_content_for(&response, &large_id)
        .ok_or_else(|| "large memory should be included via the preview tier".to_owned())?;
    let small_item = item_content_for(&response, &small_id)
        .ok_or_else(|| "small memory should be included via the full tier".to_owned())?;

    // The large candidate must be compressed (a shorter prefix), not full.
    if !large_item.ends_with(" ...") {
        return Err(format!(
            "large memory should be rendered as a truncated preview, got: {large_item}"
        ));
    }
    // The preview must be a strict prefix of the source body (extractive), so it
    // omits the tail tokens of the 180-word body.
    if large_item.contains("w179") {
        return Err("preview should omit the tail of the source body".to_owned());
    }
    // The small candidate keeps its full content.
    if small_item != small_content {
        return Err(format!(
            "small memory should keep full content; got {small_item}, expected {small_content}"
        ));
    }
    // Budget respected (off-by-one-free accounting).
    if response.data.pack.used_tokens > 100 {
        return Err(format!(
            "pack used {} tokens, exceeding the 100-token budget",
            response.data.pack.used_tokens
        ));
    }
    Ok(())
}

#[test]
fn lod_pack_hash_is_byte_stable_across_runs() -> TestResult {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    fs::create_dir_all(database_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;

    let large_body = (0..180)
        .map(|index| format!("w{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let _ = remember_fixture(
        &workspace_path,
        &database_path,
        &format!("lodfixture release verification {large_body}"),
    )?;
    let _ = remember_fixture(
        &workspace_path,
        &database_path,
        "lodfixture release verification compact rule",
    )?;

    let first = run_context_pack(&lod_options(&workspace_path, &database_path))
        .map_err(|error| format!("first pack failed: {error:?}"))?;
    let second = run_context_pack(&lod_options(&workspace_path, &database_path))
        .map_err(|error| format!("second pack failed: {error:?}"))?;

    let first_hash = first
        .data
        .pack
        .hash
        .clone()
        .ok_or_else(|| "LOD pack must produce a pack hash".to_owned())?;
    let second_hash = second
        .data
        .pack
        .hash
        .clone()
        .ok_or_else(|| "LOD pack must produce a pack hash".to_owned())?;
    if first_hash != second_hash {
        return Err(format!(
            "LOD pack hash is not byte-stable: {first_hash} vs {second_hash}"
        ));
    }
    Ok(())
}

/// bd-1n0np.5.8 acceptance gate: when the token budget is generous enough that
/// every candidate fits the Full tier (nothing needs compressing), LOD assembly
/// (`no_lod = false`) must produce a pack BYTE-IDENTICAL to the LOD-disabled path
/// (`no_lod = true`). This is the "all-Full == byte-identical-to-pre-LOD" invariant
/// the LOD feature owes (5.8): the tiering machinery must be a no-op — not even a
/// hash-perturbing annotation — when no candidate exceeds the Full share. It also
/// exercises the `--no-lod` escape hatch end-to-end through `ContextPackOptions`.
#[test]
fn lod_all_full_is_byte_identical_to_no_lod() -> TestResult {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    fs::create_dir_all(database_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;

    // Small candidates that comfortably fit the Full tier under a generous budget,
    // so the preview / link tiers never engage and LOD has nothing to compress.
    let _ = remember_fixture(
        &workspace_path,
        &database_path,
        "lodfixture release verification compact rule one",
    )?;
    let _ = remember_fixture(
        &workspace_path,
        &database_path,
        "lodfixture release verification compact rule two",
    )?;

    // Generous budget: every candidate fits the Full tier in both runs.
    let mut lod_on = lod_options(&workspace_path, &database_path);
    lod_on.max_tokens = Some(100_000);
    lod_on.no_lod = false;
    let mut lod_off = lod_options(&workspace_path, &database_path);
    lod_off.max_tokens = Some(100_000);
    lod_off.no_lod = true;

    let on = run_context_pack(&lod_on).map_err(|error| format!("lod-on pack failed: {error:?}"))?;
    let off =
        run_context_pack(&lod_off).map_err(|error| format!("lod-off pack failed: {error:?}"))?;

    let on_hash = on
        .data
        .pack
        .hash
        .clone()
        .ok_or_else(|| "all-Full LOD pack must produce a pack hash".to_owned())?;
    let off_hash = off
        .data
        .pack
        .hash
        .clone()
        .ok_or_else(|| "no-LOD pack must produce a pack hash".to_owned())?;
    if on_hash != off_hash {
        return Err(format!(
            "all-Full LOD pack is not byte-identical to the no-LOD pack: \
             {on_hash} (no_lod=false) vs {off_hash} (no_lod=true)"
        ));
    }
    Ok(())
}
