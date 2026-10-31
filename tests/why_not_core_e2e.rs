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
        bead_id: None,
        task_paths: Vec::new(),
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

/// Remove the wall-clock-derived entries from a why-not report, returning the
/// scrubbed report and the messages that were dropped.
///
/// `pack::PackBudgetAudit::timing_degradations` exists precisely because these
/// entries are not reproducible, and its doc comment says so: they "must never
/// enter the pack hash ... wall-clock time is not reproducible — folding a
/// timing entry into `self.degradations` would make the same query over the
/// same store hash differently on a loaded machine, breaking the determinism
/// contract in AGENTS.md". Callers append them to a response *after* the hash
/// is computed. The why-not report carries no hash, so it receives them
/// inline — in `filtersApplied`, in `degraded`, and as a `counterfactualHints`
/// entry whose `rationale` repeats the message.
///
/// A byte-identity assertion over a report still containing them therefore
/// measures machine load rather than determinism. Observed on the pinned lane
/// 2026-10-31: two back-to-back runs of this fixture differed in exactly one
/// place — `Pack assembly took 29435ms, at or over the ... failure threshold
/// of 2000ms` at severity `medium`, against `took 1875ms, at or over the ...
/// warning threshold of 500ms` at severity `low`. Both readings are honest;
/// the elapsed figure, the threshold it is compared against and the resulting
/// severity all move with load. So this test draws the same line for its
/// comparison that the product draws for its hash.
fn without_timing_entries(report: &Value) -> (Value, Vec<String>) {
    let mut scrubbed = report.clone();
    let mut removed = Vec::new();

    for array_key in ["filtersApplied", "degraded"] {
        if let Some(entries) = scrubbed.get_mut(array_key).and_then(Value::as_array_mut) {
            entries.retain(|entry| {
                let timing = entry.get("code").and_then(Value::as_str)
                    == Some(ee::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE);
                if timing {
                    for text_key in ["message", "detail"] {
                        if let Some(text) = entry.get(text_key).and_then(Value::as_str) {
                            removed.push(text.to_owned());
                        }
                    }
                }
                !timing
            });
        }
    }

    // The hints carry no `code`, so they are matched by the rationale they
    // copied from the degradation that was just removed. The other hints on
    // this surface (the two `graph_feature_disabled` repairs) are config-
    // derived and must survive.
    if let Some(hints) = scrubbed
        .get_mut("counterfactualHints")
        .and_then(Value::as_array_mut)
    {
        hints.retain(|hint| {
            hint.get("rationale")
                .and_then(Value::as_str)
                .is_none_or(|rationale| !removed.iter().any(|message| message == rationale))
        });
    }

    (scrubbed, removed)
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
        // ...and carry the fields that say WHY, not just that it happened.
        //
        // bd-hwye2: this row reported `got true (field present with an
        // unexpected value)` and nothing more, which states that the premise
        // broke without saying how. The premise is that a memory sharing no
        // terms with the task never enters the candidate pool -- and setup()
        // stores exactly ONE memory, so with a budget and no competitor the
        // only thing keeping it out is an admission or relevance decision.
        //
        // This call runs IN PROCESS and controls no embedder environment
        // (EE_EMBED* appears nowhere in this file), so whether an embedder
        // resolves is a property of the machine. That matters: the same
        // scenario probed with embeddings off reports stage `degraded_index`
        // and selected=false -- agreeing with the assertion for a reason that
        // has nothing to do with term overlap. retrievalStageReached is what
        // separates those cases, so a failure has to print it.
        let observed = &json["selected"];
        return Err(format!(
            "an unretrieved memory must report selected=false; got {observed} ({}). \
             stage={}, primaryReason={}, reasonSource={}, scores={}. If the stage \
             shows it reaching candidate admission, the fixture premise (one \
             memory, no shared terms, therefore never a candidate) no longer holds \
             and the fixture needs a competitor or a tighter budget; if the stage \
             shows it excluded while selected is true, those two disagree and that \
             is a product defect.",
            if observed.is_null() {
                "field ABSENT from the report, not merely false"
            } else {
                "field present with an unexpected value"
            },
            json["retrievalStageReached"],
            json["primaryReason"],
            json["reasonSource"],
            json["scores"]
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

    let first_value = serde_json::to_value(&first).map_err(|error| error.to_string())?;
    let second_value = serde_json::to_value(&second).map_err(|error| error.to_string())?;
    let (first_json, first_removed) = without_timing_entries(&first_value);
    let (second_json, second_removed) = without_timing_entries(&second_value);

    // Vacuity guard. The scrub above is narrow by construction, but a scrub
    // that emptied the report would make the comparison below trivially true
    // and this test permanently, silently green. The selection verdict is not
    // timing-derived, so it has to survive the scrub.
    if first_json["schema"] != "ee.why_not_selected.v1" {
        return Err(format!(
            "scrubbing timing entries lost the report schema, so the comparison would be \
             vacuous: {first_json}"
        ));
    }
    let surviving_filters = first_json["filtersApplied"].as_array().map_or(0, Vec::len);
    if surviving_filters == 0 {
        return Err(format!(
            "scrubbing timing entries emptied filtersApplied, so the comparison would be \
             vacuous; removed {first_removed:?}"
        ));
    }

    if first_json != second_json {
        return Err(format!(
            "why-not output is not deterministic (timing-derived entries already excluded; \
             first removed {first_removed:?}, second removed {second_removed:?}):\n\
             first:  {first_json}\nsecond: {second_json}"
        ));
    }
    Ok(())
}
