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

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, explain_why_not_default};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::models::{MemoryId, MemoryScope};
use ee::search::SpeedMode;
use serde_json::Value;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, String>;

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
    }
}

fn setup(content: &str, task: &str) -> TestResult<(TempDir, Value, String)> {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    fs::create_dir_all(database_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;

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
        return Err("an unretrieved memory cannot be selected".to_owned());
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
    fs::create_dir_all(database_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;
    // Seed one unrelated memory so the DB exists and migrates.
    let _ = remember_fixture(&workspace_path, &database_path, "unrelated seed memory")?;

    let absent = MemoryId::from_uuid(uuid::Uuid::from_u128(0x5151_5151));
    let result = explain_why_not_default(
        &why_not_options(&workspace_path, &database_path, "any task"),
        absent,
    );
    if result.is_ok() {
        return Err("explain_why_not_default must error for an absent memory id".to_owned());
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
    fs::create_dir_all(database_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;

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
