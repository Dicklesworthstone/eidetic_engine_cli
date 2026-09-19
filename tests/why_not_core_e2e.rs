//! Integration coverage for the `ee why-not` core entry point
//! (`ee::core::context::explain_why_not_default`), bd-1n0np.1.5.
//!
//! The library-level contract for `explain_why_not_selected` is covered by
//! `tests/contracts/why_not_selected_schema.rs`. These tests exercise the
//! *candidate-resolution* path added for the CLI surface (bd-1n0np.1.2): a real
//! temp workspace + DB, a stored memory, and the deterministic lexical fallback
//! that mirrors what `ee pack` would actually retrieve. They assert the
//! bd-1n0np.1.4 honesty contract end-to-end: a memory that the task retrieves is
//! explained `authoritative`, while a stored-but-unretrieved memory is
//! explained `reconstructed`.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, explain_why_not_default};
use ee::core::init::{InitOptions, init_workspace};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::models::{MemoryId, MemoryScope};
use ee::search::SpeedMode;
use serde_json::Value;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, String>;

fn db_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("ee.db")
}

/// Create the store these fixtures write into.
///
/// `ee remember` has not created a store since `91cf7bcbd` ("fix(storage):
/// reject storeless write and search addresses", 2026-08-11), which made
/// ordinary write surfaces preflight the addressed path via
/// `core::ensure_addressed_database_exists` so a mistyped `--workspace` cannot
/// plant a new store. `ee init` owns store creation.
///
/// This file predates that change (2026-06-07) and relied on the write path
/// migrating a database into existence — the seed comment in
/// `why_not_missing_memory_id_errors` said so in as many words. The sibling
/// fixtures sharing this helper's shape were repaired the same way:
/// `tests/ppr_context_pack.rs` in `be14b998a` and
/// `tests/contradiction_detect_properties.rs` in `429a44576`.
fn init_fixture_workspace(workspace_path: &Path) -> TestResult {
    let report = init_workspace(&InitOptions {
        workspace_path: workspace_path.to_path_buf(),
        dry_run: false,
        repair_plan: false,
        force: false,
        allow_symlink: false,
        skip_boilerplate: true,
    });
    if !report.status.is_success() {
        return Err(format!(
            "initialize why-not fixture workspace failed: {report:?}"
        ));
    }
    Ok(())
}

fn remember_fixture(workspace_path: &Path, db_path: &Path, content: &str) -> TestResult<String> {
    let report = remember_memory(&RememberMemoryOptions {
        workspace_path,
        database_path: Some(db_path),
        content,
        workflow_id: None,
        level: "semantic",
        kind: "note",
        tags: Some("why-not,e2e"),
        confidence: 0.9,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
        allow_secret_mention: false,
    })
    .map_err(|error| format!("remember fixture memory failed: {error:?}"))?;
    Ok(report.memory_id.to_string())
}

fn why_not_options(workspace_path: &Path, db_path: &Path, task: &str) -> ContextPackOptions {
    ContextPackOptions {
        task_lens: None,
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: None,
        query: task.to_owned(),
        speed: SpeedMode::Default,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(1000),
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
        // why-not is read-only and must never persist a pack record.
        persist_pack: false,
        baseline_write: None,
        no_lod: false,
        require_fresh_sentinels: false,
    }
}

fn setup(content: &str, task: &str) -> TestResult<(TempDir, Value, String)> {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    init_fixture_workspace(&workspace_path)?;

    let memory_id_raw = remember_fixture(&workspace_path, &database_path, content)?;
    let memory_id = MemoryId::from_str(&memory_id_raw).map_err(|error| format!("{error:?}"))?;

    let report = explain_why_not_default(
        &why_not_options(&workspace_path, &database_path, task),
        memory_id,
    )
    .map_err(|error| format!("explain_why_not_default failed: {error:?}"))?;
    let json = serde_json::to_value(&report).map_err(|error| error.to_string())?;
    Ok((temp_dir, json, memory_id_raw))
}

#[test]
fn why_not_retrieved_memory_is_authoritative() -> TestResult {
    // Task shares "release" + "verification" with the memory, so the lexical
    // fallback retrieves it into the candidate pool -> authoritative reason.
    let (_temp, json, memory_id_raw) = setup(
        "Run cargo fmt --check before the release verification step.",
        "prepare release verification",
    )?;

    if json["memoryId"] != Value::String(memory_id_raw.clone()) {
        return Err(format!(
            "report should target the stored memory; got {}",
            json["memoryId"]
        ));
    }
    if json["schema"] != "ee.why_not_selected.v1" {
        return Err(format!("unexpected report schema {}", json["schema"]));
    }
    if json["reasonSource"] != "authoritative" {
        return Err(format!(
            "a retrieved memory must be explained authoritatively, got reasonSource={}, primaryReason={}",
            json["reasonSource"], json["primaryReason"]
        ));
    }
    if json["primaryReason"] == "not_retrieved"
        || json["primaryReason"] == "not_retrieved_due_to_degraded_index"
    {
        return Err(format!(
            "a retrieved memory must not report a retrieval miss; got {}",
            json["primaryReason"]
        ));
    }
    Ok(())
}

#[test]
fn why_not_unretrieved_memory_is_reconstructed() -> TestResult {
    // The memory shares no terms with the task, so it never enters the candidate
    // pool; why-not must honestly mark the reason reconstructed (not authoritative).
    let (_temp, json, memory_id_raw) = setup(
        "Banana mango smoothie recipe with crushed ice.",
        "prepare release verification gate",
    )?;

    if json["memoryId"] != Value::String(memory_id_raw) {
        return Err(format!(
            "report should target the stored memory; got {}",
            json["memoryId"]
        ));
    }
    if json["selected"] != Value::Bool(false) {
        // Print what was actually there, and say so when the field is ABSENT.
        // `json["selected"]` yields Null for a missing key, which also fails
        // this comparison -- so the old bare message, "an unretrieved memory
        // cannot be selected", asserted that the memory WAS selected even when
        // the real defect was a renamed or dropped field. A failure message
        // that narrates a cause it did not observe is worse than a silent one,
        // because it sends the next reader somewhere specific and wrong.
        let observed = &json["selected"];
        return Err(format!(
            "an unretrieved memory must report selected=false; got {observed} ({})",
            if observed.is_null() {
                "field ABSENT from the report, not merely false"
            } else {
                "field present with an unexpected value"
            }
        ));
    }
    if json["primaryReason"] != "not_retrieved" {
        return Err(format!(
            "an unretrieved memory should report not_retrieved, got {}",
            json["primaryReason"]
        ));
    }
    if json["reasonSource"] != "reconstructed" {
        return Err(format!(
            "a retrieval miss must be reconstructed, not authoritative; got {}",
            json["reasonSource"]
        ));
    }
    Ok(())
}

#[test]
fn why_not_missing_memory_id_errors() -> TestResult {
    // A memory id with no backing row cannot be reconstructed; the core returns
    // an error rather than fabricating a report.
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    init_fixture_workspace(&workspace_path)?;
    // Seed one unrelated memory so the lookup below has a populated store.
    let _ = remember_fixture(&workspace_path, &database_path, "unrelated seed memory")?;

    let absent = MemoryId::from_uuid(uuid::Uuid::from_u128(0x5151_5151));
    let result = explain_why_not_default(
        &why_not_options(&workspace_path, &database_path, "any task"),
        absent,
    );
    if let Ok(report) = &result {
        // Surface the report it wrongly produced. "must error" tells the next
        // reader the contract and nothing about what happened instead, and the
        // interesting case -- a report fabricated for an id that is not in the
        // store -- is exactly the one the bare message hid.
        return Err(format!(
            "explain_why_not_default must error for an absent memory id, but it \
             returned a report: {report:?}"
        ));
    }
    Ok(())
}

#[test]
fn why_not_is_deterministic_across_runs() -> TestResult {
    // Same DB + options + target must produce byte-identical why-not JSON
    // (the determinism contract; explain_why_not_default uses a fixed seed).
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    init_fixture_workspace(&workspace_path)?;

    let memory_id_raw = remember_fixture(
        &workspace_path,
        &database_path,
        "Run cargo fmt --check before the release verification step.",
    )?;
    let memory_id = MemoryId::from_str(&memory_id_raw).map_err(|error| format!("{error:?}"))?;
    let task = "prepare release verification";

    let first = explain_why_not_default(
        &why_not_options(&workspace_path, &database_path, task),
        memory_id,
    )
    .map_err(|error| format!("first run failed: {error:?}"))?;
    let second = explain_why_not_default(
        &why_not_options(&workspace_path, &database_path, task),
        memory_id,
    )
    .map_err(|error| format!("second run failed: {error:?}"))?;

    let first_json = serde_json::to_string(&first).map_err(|error| error.to_string())?;
    let second_json = serde_json::to_string(&second).map_err(|error| error.to_string())?;
    if first_json != second_json {
        return Err(format!(
            "why-not output is not deterministic:\nfirst:  {first_json}\nsecond: {second_json}"
        ));
    }
    Ok(())
}
