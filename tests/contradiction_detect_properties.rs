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

use ee::core::contradiction_detect::{
    ConflictEdge, ContradictionDetectionConfig, ExplicitConflictSignal,
    detect_explicit_contradictions,
};

fn edge(a: &str, b: &str, signal: ExplicitConflictSignal) -> ConflictEdge {
    ConflictEdge::new(a, b, signal)
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
