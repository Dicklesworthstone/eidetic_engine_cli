//! G8 audit event coverage contract test (eidetic_engine_cli bd-17c65.7.7).
//!
//! Asserts that each read surface (ee search, ee context, ee why, ee memory
//! show) writes the expected audit_log rows on a successful invocation. The
//! test runs the real surfaces against a real DbConnection so producer-side
//! instrumentation is checked end-to-end, not via mock.
//!
//! Privacy contract: every audit row written for a read surface stores a
//! BLAKE3 query_hash (or `surface` tag for whys/shows), NEVER the raw query
//! text or memory content. The test reads the `details` column and asserts
//! it contains a `queryHash` field for query-bearing surfaces and never
//! the raw query string.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::core::context::{ContextPackOptions, run_context_pack};
use ee::core::index::{IndexRebuildOptions, rebuild_index};
use ee::core::memory::{
    GetMemoryOptions, RememberMemoryOptions, get_memory_details, remember_memory,
};
use ee::core::search::{SearchOptions, run_search};
use ee::core::why::{WhyOptions, explain_memory};
use ee::db::{DbConnection, audit_actions};
use ee::search::scoring::SpeedMode;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn build_workspace() -> Result<(TempDir, PathBuf, PathBuf, String), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir failed: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let database = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(database.parent().expect("db parent"))
        .map_err(|error| format!("mkdir parent failed: {error}"))?;
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

#[test]
fn ee_search_writes_search_executed_and_returned_mem_rows() -> TestResult {
    let (_dir, workspace, database, _memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let report = run_search(&SearchOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(workspace.join(".ee").join("index")),
        query: "cargo fmt release".to_owned(),
        limit: 10,
        speed: SpeedMode::Default,
        explain: false,
        relevance_floor: Some(0.0),
    })
    .map_err(|error| format!("run_search: {error:?}"))?;
    if report.results.is_empty() {
        return Err(format!(
            "fixture expected at least one search hit, got status={:?}",
            report.status
        ));
    }

    let audit = audit_actions_for(&database);
    let executed_count = count_action(&audit, audit_actions::SEARCH_EXECUTED);
    let returned_count = count_action(&audit, audit_actions::SEARCH_RETURNED_MEM);
    if executed_count != 1 {
        return Err(format!(
            "expected exactly one search.executed row, got {executed_count}"
        ));
    }
    if returned_count != report.results.len() {
        return Err(format!(
            "expected {} search.returned_mem rows (one per result), got {}",
            report.results.len(),
            returned_count
        ));
    }
    Ok(())
}

#[test]
fn ee_search_audit_row_carries_query_hash_not_raw_query() -> TestResult {
    let (_dir, workspace, database, _memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let raw_query = "secret-flag-marker-text-for-coverage";
    let _ = run_search(&SearchOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(workspace.join(".ee").join("index")),
        query: raw_query.to_owned(),
        limit: 10,
        speed: SpeedMode::Default,
        explain: false,
        relevance_floor: Some(0.0),
    })
    .map_err(|error| format!("run_search: {error:?}"))?;

    let audit = audit_actions_for(&database);
    let executed_details = audit
        .iter()
        .find(|(a, _)| a == audit_actions::SEARCH_EXECUTED)
        .and_then(|(_, d)| d.clone())
        .ok_or_else(|| "missing search.executed row".to_string())?;
    if executed_details.contains(raw_query) {
        return Err(format!(
            "audit details leak raw query text: {executed_details}"
        ));
    }
    if !executed_details.contains("queryHash") || !executed_details.contains("blake3:") {
        return Err(format!(
            "audit details missing queryHash field: {executed_details}"
        ));
    }
    Ok(())
}

#[test]
fn ee_context_writes_pack_assembled_and_included_mem_rows() -> TestResult {
    let (_dir, workspace, database, _memory_id) =
        build_workspace().map_err(|error| format!("setup: {error}"))?;
    let response = run_context_pack(&ContextPackOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(workspace.join(".ee").join("index")),
        query: "cargo fmt release".to_owned(),
        profile: None,
        max_tokens: Some(2000),
        candidate_pool: Some(10),
        max_results: None,
        speed: SpeedMode::Default,
        filters: Default::default(),
        pagination: None,
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
    Ok(())
}

#[test]
fn ee_why_writes_why_inspected_row() -> TestResult {
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
    if why_count != 1 {
        return Err(format!(
            "expected one why.inspected row after ee why, got {why_count}"
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
