//! Property / contract tests for the explicit-evidence contradiction detector
//! (bd-1n0np.7.6) over the landed pub core in
//! `ee::core::contradiction_detect::detect_explicit_contradictions`
//! (bd-1n0np.7.2 detection core, d010a42f).
//!
//! The in-module tests cover canonical-pair normalization, a reversed-order
//! determinism point, and the fuzzy-skip flag on empty input. These lock the
//! load-bearing input contract more broadly, independent of whether k-truss +
//! Louvain forms a cluster for any given small input:
//! - empty input yields an empty, non-failing report;
//! - `explicit_edge_count` counts DISTINCT canonical pairs (undirected dedup,
//!   multi-signal collapse);
//! - self-loops and blank endpoints are dropped, never counted;
//! - the fuzzy near-conflict pass is reported skipped, never silently widened;
//! - the whole report is deterministic across edge orderings;
//! - ranked clusters (when any form) are ordered most-urgent-first.

use std::fs;
use std::path::{Path, PathBuf};

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, run_context_pack};
use ee::core::contradiction_detect::{
    ConflictEdge, ContradictionDetectionConfig, ExplicitConflictSignal,
    detect_explicit_contradictions,
};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource};
use ee::models::MemoryScope;
use ee::search::SpeedMode;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, String>;

fn db_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("ee.db")
}

fn edge(a: &str, b: &str, signal: ExplicitConflictSignal) -> ConflictEdge {
    ConflictEdge::new(a, b, signal)
}

fn remember_pack_guard_fixture(
    workspace_path: &Path,
    database_path: &Path,
    content: &str,
) -> TestResult<String> {
    let report = remember_memory(&RememberMemoryOptions {
        workspace_path,
        database_path: Some(database_path),
        content,
        workflow_id: None,
        level: "semantic",
        kind: "note",
        tags: Some("contradiction,pack-guard"),
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

fn insert_contradiction_link(database_path: &Path, first: &str, second: &str) -> TestResult {
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000070009",
            &CreateMemoryLinkInput {
                src_memory_id: first.to_owned(),
                dst_memory_id: second.to_owned(),
                relation: MemoryLinkRelation::Contradicts,
                weight: 1.0,
                confidence: 1.0,
                directed: false,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: Some("bd-1n0np.7.9-test".to_owned()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())
}

fn pack_guard_options(
    workspace_path: &Path,
    database_path: &Path,
    task: &str,
) -> ContextPackOptions {
    ContextPackOptions {
        task_lens: None,
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(database_path.to_path_buf()),
        index_dir: None,
        query: task.to_owned(),
        speed: SpeedMode::Default,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(2000),
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
        baseline_write: None,
        no_lod: false,
        require_fresh_sentinels: false,
    }
}

#[test]
fn production_pack_suppresses_one_side_of_explicit_contradiction() -> TestResult {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path().to_path_buf();
    let database_path = db_path(&workspace_path);
    fs::create_dir_all(database_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;

    let first = remember_pack_guard_fixture(
        &workspace_path,
        &database_path,
        "Network retry policy: always retry transient failed network calls exactly three times before returning an error.",
    )?;
    let second = remember_pack_guard_fixture(
        &workspace_path,
        &database_path,
        "Network retry policy: never retry transient failed network calls; fail fast immediately instead.",
    )?;
    insert_contradiction_link(&database_path, &first, &second)?;

    let response = run_context_pack(&pack_guard_options(
        &workspace_path,
        &database_path,
        "network retry policy transient failed network calls",
    ))
    .map_err(|error| format!("run_context_pack failed: {error:?}"))?;
    let selected_conflict_sides = response
        .data
        .pack
        .items
        .iter()
        .filter(|item| {
            let memory_id = item.memory_id.to_string();
            memory_id == first || memory_id == second
        })
        .count();
    assert_eq!(
        selected_conflict_sides, 1,
        "production pack should retain exactly one unresolved contradiction side"
    );
    let suppressed = response.data.pack.omitted.iter().find(|omission| {
        let memory_id = omission.memory_id.to_string();
        (memory_id == first || memory_id == second)
            && omission.reason.as_str() == "contradiction_suppressed"
    });
    assert!(
        suppressed.is_some(),
        "production pack should record the suppressed side as contradiction_suppressed"
    );
    assert_eq!(
        response.data.pack.used_tokens,
        response
            .data
            .pack
            .items
            .iter()
            .map(|item| item.estimated_tokens)
            .sum::<u32>(),
        "production pack token accounting must reflect post-guard selected items"
    );
    Ok(())
}

#[test]
fn empty_edges_yield_an_empty_non_failing_report() {
    let report = detect_explicit_contradictions(&[], ContradictionDetectionConfig::default());
    assert!(report.clusters.is_empty());
    assert_eq!(report.explicit_edge_count, 0);
    assert!(!report.fuzzy_near_conflict_skipped);
}

#[test]
fn explicit_edge_count_dedups_undirected_and_multi_signal_pairs() {
    // (a,b) appears three ways — reversed direction and a second signal — but is
    // one undirected pair. (c,d) is a distinct pair. Count must be 2.
    let edges = vec![
        edge("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
        edge("mem_b", "mem_a", ExplicitConflictSignal::ContradictionLink),
        edge("mem_a", "mem_b", ExplicitConflictSignal::Supersession),
        edge("mem_c", "mem_d", ExplicitConflictSignal::TrustOutcomeSplit),
    ];
    let report = detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    assert_eq!(
        report.explicit_edge_count, 2,
        "two distinct undirected pairs after dedup"
    );
}

#[test]
fn self_loops_and_blank_endpoints_are_dropped() {
    let edges = vec![
        edge("mem_a", "mem_a", ExplicitConflictSignal::ContradictionLink), // self-loop
        edge("   ", "mem_b", ExplicitConflictSignal::ContradictionLink),   // blank a
        edge("mem_x", "", ExplicitConflictSignal::Supersession),           // blank b
        edge(
            "  mem_a  ",
            "mem_b",
            ExplicitConflictSignal::ContradictionLink,
        ), // trims to (a,b)
    ];
    let report = detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    assert_eq!(
        report.explicit_edge_count, 1,
        "only the trimmed (mem_a, mem_b) pair survives"
    );
}

#[test]
fn fuzzy_pass_is_reported_skipped_never_silently_widened() {
    let edges = vec![edge(
        "mem_a",
        "mem_b",
        ExplicitConflictSignal::ContradictionLink,
    )];
    let with_fuzzy = ContradictionDetectionConfig {
        include_fuzzy_near_conflict: true,
        ..ContradictionDetectionConfig::default()
    };
    let report = detect_explicit_contradictions(&edges, with_fuzzy);
    assert!(
        report.fuzzy_near_conflict_skipped,
        "opting into fuzzy must surface a skipped flag (v1 defers it), never widen silently"
    );

    let report_default =
        detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    assert!(!report_default.fuzzy_near_conflict_skipped);
}

#[test]
fn report_is_deterministic_across_edge_orderings() {
    let base = vec![
        edge("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
        edge("mem_b", "mem_c", ExplicitConflictSignal::ContradictionLink),
        edge("mem_a", "mem_c", ExplicitConflictSignal::Supersession),
        edge("mem_c", "mem_d", ExplicitConflictSignal::DuplicateDivergent),
    ];
    let mut reversed = base.clone();
    reversed.reverse();
    let mut rotated = base.clone();
    rotated.rotate_left(2);

    let config = ContradictionDetectionConfig::default();
    let canonical = detect_explicit_contradictions(&base, config);
    assert_eq!(
        canonical,
        detect_explicit_contradictions(&reversed, config),
        "reversed edge order must yield an identical report"
    );
    assert_eq!(
        canonical,
        detect_explicit_contradictions(&rotated, config),
        "rotated edge order must yield an identical report"
    );
}

#[test]
fn ranked_clusters_are_ordered_most_urgent_first() {
    // A small densely-linked set; whatever clusters form must be sorted by
    // non-increasing rank_score (deterministic urgency ordering).
    let edges = vec![
        edge("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
        edge("mem_b", "mem_c", ExplicitConflictSignal::ContradictionLink),
        edge("mem_a", "mem_c", ExplicitConflictSignal::ContradictionLink),
        edge("mem_c", "mem_d", ExplicitConflictSignal::ContradictionLink),
        edge("mem_d", "mem_e", ExplicitConflictSignal::ContradictionLink),
        edge("mem_c", "mem_e", ExplicitConflictSignal::ContradictionLink),
    ];
    let report = detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    for pair in report.clusters.windows(2) {
        assert!(
            pair[0].rank_score >= pair[1].rank_score,
            "clusters must be sorted most-urgent (highest rank_score) first"
        );
    }
}
