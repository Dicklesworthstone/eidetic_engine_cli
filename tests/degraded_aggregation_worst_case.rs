//! Worst-case multi-emitter aggregation contract (bd-2kj2x.1).
//!
//! The inline tests in `src/core/degraded_aggregation.rs` each cover
//! one rule of the aggregation contract in isolation: same-code
//! collapse, severity escalation, truncation trailer, byte-stable
//! ordering. This integration test drives the helper with a single
//! input that triggers ALL of those rules at once, the way `ee
//! context` does in production when multiple algorithms emit the same
//! stale-snapshot signal alongside unrelated low-recall, index, and
//! cache degradations. Failing one assertion here flags a regression
//! that the per-rule tests might mask by passing in their own narrow
//! scenarios.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::core::degraded_aggregation::{
    AggregatedDegradation, DEGRADED_AGGREGATION_MAX_ENTRIES, DEGRADED_AGGREGATION_TRUNCATED_CODE,
    DegradationAggregationInput, aggregate_degraded_entries,
};

/// Build a deterministic worst-case input that:
///
/// - Hits the same `snapshot_stale` code from 8 graph algorithms at
///   mixed severities so duplicate-code collapse, severity escalation,
///   and source-sort assertions all apply.
/// - Includes 21 unrelated low-severity codes from various subsystems
///   so the truncation trailer fires (1 stale-snapshot aggregate +
///   21 unique = 22 > MAX 20).
/// - Repeats one of the unrelated codes from a different source to
///   exercise dedup-source-list under truncation pressure.
fn worst_case_input() -> Vec<DegradationAggregationInput> {
    let mut entries: Vec<DegradationAggregationInput> = Vec::new();

    let snapshot_emitters: &[(&str, &str, &str, &str)] = &[
        (
            "ppr",
            "low",
            "ppr saw stale snapshot",
            "ee graph snapshot refresh",
        ),
        (
            "pack_dna",
            "medium",
            "pack DNA saw stale snapshot",
            "ee graph snapshot refresh",
        ),
        (
            "voronoi",
            "warning",
            "voronoi saw stale snapshot",
            "ee graph snapshot refresh",
        ),
        (
            "louvain",
            "high",
            "louvain saw stale snapshot",
            "ee graph snapshot refresh --graph louvain",
        ),
        (
            "ego",
            "low",
            "ego saw stale snapshot",
            "ee graph snapshot refresh",
        ),
        (
            "hits",
            "medium",
            "hits saw stale snapshot",
            "ee graph snapshot refresh",
        ),
        (
            "betweenness",
            "info",
            "betweenness saw stale snapshot",
            "ee graph snapshot refresh",
        ),
        (
            "kcore",
            "low",
            "kcore saw stale snapshot",
            "ee graph snapshot refresh",
        ),
    ];
    for (source, severity, message, repair) in snapshot_emitters {
        entries.push(DegradationAggregationInput::new(
            *source,
            "snapshot_stale",
            *severity,
            *message,
            *repair,
        ));
    }

    // 21 unique unrelated codes — pushes the aggregate count over the
    // MAX 20 cap so truncation must fire.
    for n in 0..21 {
        let code = format!("noise_code_{n:02}");
        entries.push(DegradationAggregationInput::new(
            "noise_emitter",
            code,
            "low",
            "irrelevant",
            "no remediation",
        ));
    }

    // One of the noise codes re-emitted from a second source. After
    // aggregation the entry should still survive truncation iff it
    // makes the top-20 cut, and `sources` should carry both emitters
    // alphabetically sorted regardless of input order.
    entries.push(DegradationAggregationInput::new(
        "echo_emitter",
        "noise_code_00",
        "low",
        "irrelevant from echo",
        "no remediation",
    ));

    entries
}

fn find<'a>(
    aggregates: &'a [AggregatedDegradation],
    code: &str,
) -> Option<&'a AggregatedDegradation> {
    aggregates.iter().find(|entry| entry.code == code)
}

#[test]
fn worst_case_collapses_duplicates_escalates_severity_and_truncates_with_trailer() {
    let entries = worst_case_input();
    let aggregates = aggregate_degraded_entries(entries);

    // Cap enforcement: the visible array length is exactly the
    // documented cap when input would have produced > MAX aggregates.
    assert_eq!(
        aggregates.len(),
        DEGRADED_AGGREGATION_MAX_ENTRIES,
        "truncated array length must equal DEGRADED_AGGREGATION_MAX_ENTRIES"
    );

    // 1) Duplicate-code collapse: the eight stale-snapshot emitters
    // produce ONE aggregate with sources sorted alphabetically and
    // deduplicated.
    let snapshot = find(&aggregates, "snapshot_stale").expect("snapshot_stale aggregate present");
    assert_eq!(snapshot.sources.len(), 8);
    let mut expected_sources: Vec<&'static str> = vec![
        "betweenness",
        "ego",
        "hits",
        "kcore",
        "louvain",
        "pack_dna",
        "ppr",
        "voronoi",
    ];
    expected_sources.sort();
    let actual_sources: Vec<&str> = snapshot.sources.iter().map(String::as_str).collect();
    assert_eq!(
        actual_sources, expected_sources,
        "sources must be sorted ascending"
    );

    // 2) Severity escalation: the highest emitter (`louvain`, high)
    // wins; its repair hint travels with it (load-bearing remediation).
    assert_eq!(snapshot.severity, "high");
    assert_eq!(snapshot.repair, "ee graph snapshot refresh --graph louvain");
    assert_eq!(snapshot.message, "louvain saw stale snapshot");

    // 3) Ordering: stale-snapshot (high) is the first entry; subsequent
    // entries are sorted by descending severity then code. The trailer
    // is always last.
    assert_eq!(
        aggregates[0].code, "snapshot_stale",
        "highest severity first"
    );
    let trailer = aggregates.last().expect("trailer slot");
    assert_eq!(trailer.code, DEGRADED_AGGREGATION_TRUNCATED_CODE);
    assert_eq!(trailer.severity, "info");

    // 4) Trailer must carry the dropped codes in `sources` and report
    // the dropped count in its message. Input had 22 distinct
    // aggregates (1 snapshot_stale + 21 noise); MAX-1 = 19 visible
    // aggregates plus 1 trailer = 20 entries, so the trailer reports
    // 22 - 19 = 3 dropped codes.
    let expected_dropped = 22 - (DEGRADED_AGGREGATION_MAX_ENTRIES - 1);
    assert!(
        trailer
            .message
            .contains(&format!("{expected_dropped} additional")),
        "trailer message must report dropped count, got: {}",
        trailer.message
    );
    assert_eq!(
        trailer.sources.len(),
        expected_dropped,
        "trailer.sources must enumerate exactly the dropped codes"
    );
    for dropped_code in &trailer.sources {
        assert!(
            dropped_code.starts_with("noise_code_"),
            "dropped codes should be noise variants, got: {dropped_code}"
        );
        // Each dropped code must NOT also appear in the visible
        // aggregates.
        assert!(
            find(&aggregates[..aggregates.len() - 1], dropped_code).is_none(),
            "dropped code {dropped_code} must not survive in the visible array"
        );
    }

    // 5) Source dedup under truncation: the doubled noise_code_00
    // emission must collapse into one aggregate with sources sorted
    // alphabetically — IF that code survives the cap. With 22 distinct
    // codes and MAX=20, the trailer drops 3 entries by code-sort
    // tiebreak among the 21 low-severity noise codes (codes are
    // already sorted ascending by zero-padded number), so
    // `noise_code_00` survives and its source list should have BOTH
    // emitters.
    if let Some(echoed) = find(&aggregates, "noise_code_00") {
        assert_eq!(echoed.sources, vec!["echo_emitter", "noise_emitter"]);
    }
}

#[test]
fn worst_case_output_is_byte_stable_across_shuffled_input() {
    let mut entries_a = worst_case_input();
    let mut entries_b = entries_a.clone();
    // Reverse one and rotate the other so neither matches the natural
    // construction order.
    entries_b.reverse();
    entries_a.rotate_left(7);

    let agg_a = aggregate_degraded_entries(entries_a);
    let agg_b = aggregate_degraded_entries(entries_b);

    let json_a = serde_json::to_string(&agg_a).expect("agg A serializes");
    let json_b = serde_json::to_string(&agg_b).expect("agg B serializes");
    assert_eq!(
        json_a, json_b,
        "worst-case aggregation must be invariant under input shuffling"
    );
}

#[test]
fn worst_case_trailer_sources_are_deterministic_across_runs() {
    let agg_a = aggregate_degraded_entries(worst_case_input());
    let agg_b = aggregate_degraded_entries(worst_case_input());
    let trailer_a = agg_a.last().expect("trailer present").clone();
    let trailer_b = agg_b.last().expect("trailer present").clone();
    assert_eq!(trailer_a.sources, trailer_b.sources);
    assert_eq!(trailer_a.message, trailer_b.message);
}
