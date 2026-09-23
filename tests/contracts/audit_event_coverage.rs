//! G8 audit event coverage contract test (eidetic_engine_cli bd-17c65.7.7).
//!
//! Asserts each surface's declared audit coverage against a real
//! `DbConnection`: `ee why` appends no row, `ee search` appends hash-only
//! retrieval rows, and the mutating surfaces (`ee context` with pack
//! persistence, `ee memory show`) append theirs.
//!
//! The `ee search` half was rewritten for bd-g3yh5. It previously asserted
//! that search wrote *nothing*, contradicting the privacy contract stated
//! below in this very comment — which describes what a read surface's audit
//! row must contain, and so presumes the row exists. ADR 0070 (outcome-tuned
//! retrieval weights) and ADR 0071 (memory debt, which defines
//! `never_retrieved` by the absence of these rows) are the deciding
//! authorities; `src/core/shadow_tuning.rs` is the live consumer.
//!
//! Privacy contract: every audit row written for a read surface stores a
//! BLAKE3 query_hash (or `surface` tag for whys/shows), NEVER the raw query
//! text or memory content. The test reads the `details` column and asserts
//! it contains a `queryHash` field for query-bearing surfaces and never
//! the raw query string.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "ask_large_corpus.rs"]
mod ask_large_corpus;

#[path = "ask_lifecycle.rs"]
mod ask_lifecycle;

use std::path::PathBuf;

use ee::core::context::{ContextPackOptions, run_context_pack};
use ee::core::index::{IndexRebuildOptions, rebuild_index};
use ee::core::init::{InitOptions, InitStatus, init_workspace};
use ee::core::memory::{
    GetMemoryOptions, RememberMemoryOptions, get_memory_details, remember_memory,
};
use ee::core::search::{SearchOptions, run_search};
use ee::core::why::{WhyOptions, explain_memory};
use ee::db::{DbConnection, audit_actions};
use ee::models::MemoryScope;
use ee::obs::audit_events::query_hash;
use ee::search::scoring::SpeedMode;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn build_workspace() -> Result<(TempDir, PathBuf, PathBuf, String), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir failed: {error}"))?;
    let workspace = dir
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize temp workspace failed: {error}"))?;
    // `ee init` owns workspace-row creation (91cf7bcbd); create_dir_all is not
    // enough. Without that row `search_audit_workspace_persisted`
    // (src/core/search.rs:7320) fails its foreign-key preflight and SUPPRESSES
    // the retrieval audit rows this file asserts. The bd-g3yh5 rewrite counts
    // those rows, so a fixture that skips init reports zero and reads as a lib
    // defect when it is really an unrepresentative workspace.
    let init = init_workspace(&InitOptions {
        workspace_path: workspace.clone(),
        dry_run: false,
        repair_plan: false,
        force: false,
        allow_symlink: false,
        skip_boilerplate: true,
    });
    if !matches!(init.status, InitStatus::Created | InitStatus::AlreadyExists) {
        return Err(format!(
            "init_workspace must persist the workspace row: status={:?} errors={:?}",
            init.status, init.action_errors
        ));
    }
    let database = init.database_path.clone();
    let conn = DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: &workspace,
        database_path: Some(&database),
        content: "Run cargo fmt --check before cutting a release.",
        workflow_id: None,
        level: "procedural",
        kind: "rule",
        tags: Some("release"),
        confidence: 0.9,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .map_err(|error| format!("remember: {error:?}"))?;

    let index_dir = workspace.join(".ee").join("index");
    rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .map_err(|error| format!("rebuild_index: {error:?}"))?;

    let memory_id = remembered.memory_id.to_string();
    Ok((dir, workspace, database, memory_id))
}

fn audit_actions_for(database_path: &std::path::Path) -> Vec<(String, Option<String>)> {
    let conn = DbConnection::open_file(database_path).expect("open db for audit query");
    let entries = conn
        .list_audit_entries(None, Some(1000))
        .expect("query audit_log");
    entries
        .into_iter()
        .map(|entry| (entry.action, entry.details))
        .collect()
}

fn count_action(audit: &[(String, Option<String>)], action: &str) -> usize {
    audit.iter().filter(|(a, _)| a == action).count()
}

/// `ee search` records retrieval in the audit log, one `search.executed` row
/// plus one `search.returned_mem` row per returned hit
/// (bd-g3yh5).
///
/// This test previously asserted both counts were **zero**, under a
/// pre-ADR-0070 reading in which a read surface wrote nothing at all. That
/// contract was deliberately superseded: ADR 0070 (outcome-tuned retrieval
/// weights) consumes the per-memory `search.returned_mem` row, and ADR 0071
/// (memory debt) *defines* `never_retrieved` as the absence of a
/// `search.returned_mem` / `pack.included_mem` row inside the retrieval
/// window. `src/core/shadow_tuning.rs` reads those rows today. Asserting they
/// are absent contradicted the module's own documented privacy contract
/// above, which describes what a read surface's audit row must *contain*.
///
/// Counts are measured as a delta around the search so fixture setup —
/// `remember_memory` runs with `auto_link`, which may itself retrieve —
/// cannot inflate or mask the result.
#[test]
fn ee_search_writes_hash_only_retrieval_audit_rows() -> TestResult {
    let (_dir, workspace, database, _memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let before = audit_actions_for(&database);
    let executed_before = count_action(&before, audit_actions::SEARCH_EXECUTED);
    let returned_before = count_action(&before, audit_actions::SEARCH_RETURNED_MEM);
    let report = run_search(&SearchOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(workspace.join(".ee").join("index")),
        query: "cargo fmt release".to_owned(),
        limit: 10,
        speed: SpeedMode::Default,
        explain: false,
        as_of: None,
        include_tombstoned: false,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: Some(0.0),
        dedup_mode: ee::core::search::SearchDedupMode::DocId,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
    })
    .map_err(|error| format!("run_search: {error:?}"))?;
    if report.results.is_empty() {
        return Err(format!(
            "fixture expected at least one search hit, got status={:?}",
            report.status
        ));
    }

    let audit = audit_actions_for(&database);
    let executed_delta =
        count_action(&audit, audit_actions::SEARCH_EXECUTED).saturating_sub(executed_before);
    let returned_delta =
        count_action(&audit, audit_actions::SEARCH_RETURNED_MEM).saturating_sub(returned_before);
    if executed_delta != 1 {
        return Err(format!(
            "one search must append exactly one search.executed row, got {executed_delta}"
        ));
    }
    if returned_delta != report.results.len() {
        return Err(format!(
            "search must append one search.returned_mem row per returned hit: {} hits, {returned_delta} rows",
            report.results.len()
        ));
    }
    Ok(())
}

/// A search audit payload carries the BLAKE3 query hash and never the query
/// text (bd-g3yh5).
///
/// Renamed from `ee_search_does_not_persist_raw_or_hashed_query_audit_payloads`,
/// which had become half-false: the "raw" half is the live privacy contract and
/// still holds, but the "or hashed" half asserted that no search audit payload
/// persists at all, which ADR 0070/0071 superseded (see the sibling test
/// above). Keeping a name that forbids hashed payloads would have documented a
/// guarantee the product deliberately does not make.
///
/// The raw-leak guard is kept and strengthened: the old version only rejected
/// the query as one exact substring, so any partial or re-tokenized leak passed.
/// It now also rejects every distinctive token of the query, and pins the hash
/// to the exact value of the canonical `query_hash` helper — which proves the
/// stored digest really is derived from this query while remaining one-way.
#[test]
fn ee_search_audit_payloads_carry_hashed_never_raw_query_text() -> TestResult {
    let (_dir, workspace, database, _memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    // Deliberately nonce-like: every token below is distinctive enough that its
    // appearance in an audit payload is a real leak, not a collision with a
    // field name such as `status`, `reason`, `sampling` or `redaction`.
    let raw_query = "zzqx-sentinel-secret-marker-coverage";
    let _ = run_search(&SearchOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(workspace.join(".ee").join("index")),
        query: raw_query.to_owned(),
        limit: 10,
        speed: SpeedMode::Default,
        explain: false,
        as_of: None,
        include_tombstoned: false,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: Some(0.0),
        dedup_mode: ee::core::search::SearchDedupMode::DocId,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
    })
    .map_err(|error| format!("run_search: {error:?}"))?;

    let audit = audit_actions_for(&database);
    let search_details = audit
        .iter()
        .filter(|(action, _)| {
            action == audit_actions::SEARCH_EXECUTED
                || action == audit_actions::SEARCH_MISS_RECORDED
                || action == audit_actions::SEARCH_RETURNED_MEM
        })
        .filter_map(|(_, details)| details.as_deref())
        .collect::<Vec<_>>();
    // (1) The live privacy contract, unchanged in intent and widened in reach:
    // neither the whole query nor any distinctive token of it may appear.
    if search_details
        .iter()
        .any(|details| details.contains(raw_query))
    {
        return Err(format!(
            "search audit details leak raw query text: {search_details:?}"
        ));
    }
    for token in raw_query.split('-').filter(|token| token.len() >= 5) {
        if let Some(details) = search_details
            .iter()
            .find(|details| details.contains(token))
        {
            return Err(format!(
                "search audit details leak query token {token:?}: {details}"
            ));
        }
    }

    // (2) The payload must actually carry the canonical hash. Without this the
    // test would pass on an audit row that recorded nothing at all, which is
    // how the superseded assertion hid the real contract.
    let expected_hash = query_hash(raw_query);
    if !expected_hash.starts_with("blake3:") {
        return Err(format!(
            "query_hash must stay a prefixed digest, got {expected_hash}"
        ));
    }
    if search_details.is_empty() {
        return Err(
            "search must record retrieval audit payloads for ADR 0070/0071 consumers".to_owned(),
        );
    }
    // EVERY search audit row must carry this query's hash, because in this
    // workspace the only retrieval anyone performed is the one above.
    //
    // I weakened this to an existence check in cc3714bc8, when 20e2de5e7 made
    // `remember`'s auto_link neighbour probe record rows of its own and a foreign
    // hash appeared here. That was backwards: the assertion was not accidentally
    // strict, it was stating that the retrieval audit contains only genuine
    // retrievals -- exactly the property ADR 0071 depends on. The test was right
    // and the fix was wrong, so the fix moved (run_search_unaudited) and the
    // assertion came back.
    for details in &search_details {
        let parsed: serde_json::Value = serde_json::from_str(details)
            .map_err(|error| format!("search audit details must be JSON: {error}: {details}"))?;
        let recorded = parsed
            .get("queryHash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("search audit payload is missing queryHash: {details}"))?;
        if recorded != expected_hash {
            return Err(format!(
                "search audit payload must store the canonical query hash {expected_hash}, got {recorded}"
            ));
        }
    }
    Ok(())
}

#[test]
fn ee_context_writes_pack_assembled_and_included_mem_rows() -> TestResult {
    let (_dir, workspace, database, _memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let response = run_context_pack(&ContextPackOptions {
        task_paths: Vec::new(),
        task_lens: None,
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(workspace.join(".ee").join("index")),
        query: "cargo fmt release".to_owned(),
        profile: None,
        max_tokens: Some(2000),
        candidate_pool: Some(10),
        max_results: None,
        speed: SpeedMode::Default,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        filters: Default::default(),
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
        output_options: Default::default(),
        persist_pack: true,
        baseline_write: None,
        no_lod: false,
        require_fresh_sentinels: false,
    })
    .map_err(|error| format!("run_context_pack: {error:?}"))?;
    let included = response.data.pack.items.len();
    if included == 0 {
        return Err("fixture expected at least one pack item".to_string());
    }

    let audit = audit_actions_for(&database);
    let assembled = count_action(&audit, audit_actions::PACK_ASSEMBLED);
    let included_audit = count_action(&audit, audit_actions::PACK_INCLUDED_MEM);
    if assembled != 1 {
        return Err(format!("expected one pack.assembled row, got {assembled}"));
    }
    if included_audit != included {
        return Err(format!(
            "expected {included} pack.included_mem rows, got {included_audit}"
        ));
    }
    let assembled_details = audit
        .iter()
        .find(|(a, _)| a == audit_actions::PACK_ASSEMBLED)
        .and_then(|(_, d)| d.clone())
        .ok_or_else(|| "missing pack.assembled details".to_string())?;
    let details_json: serde_json::Value = serde_json::from_str(&assembled_details)
        .map_err(|error| format!("pack.assembled details should parse: {error}"))?;
    for key in [
        "algorithm_id",
        "algorithmDescription",
        "items_selected",
        "items_skipped",
        "objective_value",
    ] {
        if details_json.get(key).is_none() {
            return Err(format!(
                "pack.assembled details missing {key}: {assembled_details}"
            ));
        }
    }
    if details_json
        .get("algorithm_id")
        .and_then(serde_json::Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(format!(
            "pack.assembled details missing non-empty algorithm_id: {assembled_details}"
        ));
    }
    Ok(())
}

#[test]
fn ee_why_does_not_write_why_inspected_row() -> TestResult {
    let (_dir, workspace, database, memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let _ = &workspace; // suppress unused
    let _report = explain_memory(&WhyOptions {
        database_path: &database,
        memory_id: &memory_id,
        confidence_threshold: 0.5,
    });

    let audit = audit_actions_for(&database);
    let why_count = count_action(&audit, audit_actions::WHY_INSPECTED);
    if why_count != 0 {
        return Err(format!(
            "read-only why wrote {why_count} why.inspected rows"
        ));
    }
    Ok(())
}

#[test]
fn ee_memory_show_writes_memory_show_row() -> TestResult {
    let (_dir, _workspace, database, memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let report = get_memory_details(&GetMemoryOptions {
        database_path: &database,
        memory_id: &memory_id,
        include_tombstoned: false,
    });
    if !report.found {
        return Err(format!(
            "memory show fixture expected found=true, got {:?}",
            report.found
        ));
    }

    let audit = audit_actions_for(&database);
    let show_count = count_action(&audit, audit_actions::MEMORY_SHOW);
    if show_count != 1 {
        return Err(format!(
            "expected one memory.show row after ee memory show, got {show_count}"
        ));
    }
    Ok(())
}

/// An initialised workspace with NO memories, so `ee ask` has nothing to cite
/// and must abstain.
///
/// Deliberately not `build_workspace()` with an off-topic question: that would
/// make abstention depend on a relevance threshold, and a scoring change would
/// then silently convert the paired negative below into a vacuous pass. An
/// empty corpus cannot produce a candidate, so the abstention is structural.
fn build_empty_workspace() -> Result<(TempDir, PathBuf, PathBuf), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir failed: {error}"))?;
    let workspace = dir
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize temp workspace failed: {error}"))?;
    let init = init_workspace(&InitOptions {
        workspace_path: workspace.clone(),
        dry_run: false,
        repair_plan: false,
        force: false,
        allow_symlink: false,
        skip_boilerplate: true,
    });
    if !matches!(init.status, InitStatus::Created | InitStatus::AlreadyExists) {
        return Err(format!(
            "init_workspace must persist the workspace row: status={:?} errors={:?}",
            init.status, init.action_errors
        ));
    }
    let database = init.database_path.clone();
    let conn = DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);
    Ok((dir, workspace, database))
}

/// `ee ask` appends a `search.returned_mem` row for each memory it CITES, so
/// ADR 0071 stops classifying answered-with memories as `never_retrieved`
/// (bd-b9dmp).
///
/// Spawns the real binary deliberately. `record_ask_retrieval_best_effort`
/// lives in `src/core/ask.rs` but is CALLED from the CLI handler
/// (`src/cli/mod.rs:50936`, inside `handle_ask`), so an in-process test of the
/// core ask API — which is how every other row in this file drives its surface
/// — would exercise the recorder while leaving the wiring unproven. The defect
/// this pins is precisely a missing call, so the call site has to be in scope.
///
/// NO-CLAIM: green here does NOT prove ADR 0071 reclassifies the memory. It
/// proves the row is written with the right action and origin. Whether the debt
/// query reads it inside its retrieval window is `src/core/shadow_tuning.rs`'s
/// contract and is not asserted here.
#[test]
fn ee_ask_writes_returned_mem_rows_for_cited_memories() -> TestResult {
    let (_dir, workspace, database, _memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let before = audit_actions_for(&database);
    let returned_before = count_action(&before, audit_actions::SEARCH_RETURNED_MEM);
    let miss_before = count_action(&before, audit_actions::SEARCH_MISS_RECORDED);

    let output = crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .arg("--workspace")
            .arg(&workspace)
            .arg("ask")
            .arg("What should I run before cutting a release?");
    })
    .map_err(|error| format!("spawn ee ask: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ee ask must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let audit = audit_actions_for(&database);
    let returned_delta =
        count_action(&audit, audit_actions::SEARCH_RETURNED_MEM).saturating_sub(returned_before);
    let miss_delta =
        count_action(&audit, audit_actions::SEARCH_MISS_RECORDED).saturating_sub(miss_before);

    if returned_delta == 0 {
        // Separate a broken recorder from an unrepresentative fixture. An
        // abstaining ask writes a miss row and cites nothing, so there is
        // legitimately nothing to record; a silent recorder writes neither.
        if miss_delta > 0 {
            return Err(
                "fixture problem, not a contract failure: ee ask ABSTAINED, so it cited \
                 nothing to record. Seed a memory the question actually matches."
                    .to_owned(),
            );
        }
        return Err(
            "ee ask appended neither a retrieval row nor a miss row: its audit path did not run"
                .to_owned(),
        );
    }

    let ask_sourced = audit
        .iter()
        .filter(|(action, details)| {
            action == audit_actions::SEARCH_RETURNED_MEM
                && details
                    .as_deref()
                    .is_some_and(|value| value.contains("\"source\":\"ask\""))
        })
        .count();
    if ask_sourced == 0 {
        return Err(format!(
            "ee ask's retrieval rows must carry source=\"ask\" so ADR 0071 can attribute them: \
             {returned_delta} row(s) appended, none sourced to ask"
        ));
    }
    Ok(())
}

/// An ABSTAINING `ee ask` appends no retrieval row (bd-b9dmp).
///
/// This pins the `report.abstained || report.citations.is_empty()` guard in
/// `record_ask_retrieval_best_effort`. Without it, ask would record the corpus
/// it SCANNED — it reads the store directly via `list_memories` — which would
/// mark every memory retrieved and make ADR 0071's `never_retrieved` set
/// permanently empty. That is a worse failure than the one this bead fixed,
/// because it destroys the signal rather than under-reporting it.
///
/// PAIRED, not refusal-only: the zero is asserted alongside a POSITIVE
/// observable — the `search.miss_recorded` row abstention does write. Without
/// that anchor this row would pass just as happily if `ee ask` never ran at
/// all, which is the vacuity mode this suite has been repeatedly bitten by.
///
/// NO-CLAIM: green here does NOT prove ask abstains correctly, only that when
/// it does abstain it records no retrieval. The abstention decision itself is
/// `evaluate_ask`'s contract.
#[test]
fn ee_ask_abstention_appends_no_returned_mem_row() -> TestResult {
    let (_dir, workspace, database) =
        build_empty_workspace().map_err(|error| format!("setup: {error}"))?;
    let before = audit_actions_for(&database);
    let returned_before = count_action(&before, audit_actions::SEARCH_RETURNED_MEM);
    let miss_before = count_action(&before, audit_actions::SEARCH_MISS_RECORDED);

    // Not asserting success: abstention is permitted to carry its own exit
    // code, and the audit rows are appended before that decision is made.
    let output = crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .arg("--workspace")
            .arg(&workspace)
            .arg("ask")
            .arg("What should I run before cutting a release?");
    })
    .map_err(|error| format!("spawn ee ask: {error}"))?;

    let audit = audit_actions_for(&database);
    let returned_delta =
        count_action(&audit, audit_actions::SEARCH_RETURNED_MEM).saturating_sub(returned_before);
    let miss_delta =
        count_action(&audit, audit_actions::SEARCH_MISS_RECORDED).saturating_sub(miss_before);

    if miss_delta == 0 {
        return Err(format!(
            "ask on an empty corpus must abstain and append a search.miss_recorded row; none \
             appeared, so the zero retrieval rows below prove nothing (exit={:?}, stderr: {})",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if returned_delta != 0 {
        return Err(format!(
            "an abstaining ee ask must append no search.returned_mem row, got {returned_delta}: \
             the citations/abstained guard in record_ask_retrieval_best_effort is not holding"
        ));
    }
    Ok(())
}
