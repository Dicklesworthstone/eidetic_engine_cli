#![no_main]

//! Fuzz target for `ee::core::degraded_aggregation::aggregate_degraded`.
//!
//! Drives arbitrary `(source, code, severity, message, repair)`
//! tuples through the aggregation helper and asserts the invariants
//! it promises:
//!
//! 1. **Panic-freedom.** The helper must never panic on any input
//!    drawn from the index tables below; an attacker controlling the
//!    set of emitters and codes (e.g. by tripping every graph
//!    algorithm) must not be able to crash a response renderer.
//! 2. **Truncation cap.** The visible array length never exceeds
//!    `DEGRADED_AGGREGATION_MAX_ENTRIES` (20). When the input would
//!    produce more aggregates, the last entry is the synthetic
//!    truncation trailer with code `degraded_array_truncated`.
//! 3. **Sources sorted and deduped.** Inside every aggregate,
//!    `sources` is in non-decreasing order with no consecutive
//!    duplicates.
//! 4. **Severity escalation correct.** For each aggregate, the
//!    severity rank is the maximum rank observed across emitters of
//!    that code. Unknown severities sort below `info` (rank 0).
//! 5. **Determinism.** Two calls with identical input produce
//!    byte-identical JSON serialization.
//! 6. **Code aggregation.** Each distinct input code appears at
//!    most once across the non-trailer aggregates.
//!
//! Inputs are interpreted as a sequence of 5-byte tuples; each byte
//! indexes into a small fixed table of `&'static str` values so the
//! helper sees realistic shapes (`"ppr"`, `"snapshot_stale"`,
//! `"medium"`, …) instead of arbitrary UTF-8 chaos. This keeps the
//! corpus dense with structurally-valid cases while still
//! exercising every aggregation rule.

use libfuzzer_sys::fuzz_target;

use ee::core::degraded_aggregation::{
    AggregatedDegradation, DEGRADED_AGGREGATION_MAX_ENTRIES, DEGRADED_AGGREGATION_TRUNCATED_CODE,
    aggregate_degraded,
};
use ee::core::status::DegradationReport;

const MAX_INPUT_BYTES: usize = 16 * 1024;
const TUPLE_BYTES: usize = 5;

/// Small fixed table of plausible source names. The index byte is
/// taken `% SOURCES.len()` so any byte value is valid input.
const SOURCES: &[&str] = &[
    "ppr",
    "louvain",
    "voronoi",
    "ego",
    "pack_dna",
    "hits",
    "betweenness",
    "kcore",
    "minhash_rank",
    "bipartite_provenance",
    "snapshot",
    "cache",
];

/// Plausible degraded codes. Includes a wide enough set that the
/// truncation cap fires on long input streams.
const CODES: &[&str] = &[
    "snapshot_stale",
    "snapshot_unavailable",
    "idx_cold",
    "lexical_unavailable",
    "no_relevant_results",
    "weak_query_recall",
    "memory_pressure",
    "algorithm_memory_cap",
    "large_graph_uncached",
    "unexpected_growth",
    "snapshot_approaching_cap",
    "graph_compute_unavailable",
    "single_flight_unavailable",
    "ppr_disabled",
    "louvain_disabled",
    "voronoi_disabled",
    "ego_disabled",
    "pack_dna_disabled",
    "hits_disabled",
    "betweenness_disabled",
    "kcore_disabled",
    "minhash_disabled",
    "bipartite_disabled",
    "cache_corruption",
    "cache_unavailable",
    "snapshot_archived",
    "rch_unavailable",
    "agent_mail_unavailable",
];

/// The six-tier severity vocabulary plus one intentionally invalid
/// string to exercise the "unknown severity sorts below info"
/// branch.
const SEVERITIES: &[&str] = &[
    "info", "low", "warning", "medium", "high", "critical", "criticla",
];

const MESSAGES: &[&str] = &[
    "stale",
    "cold",
    "no results",
    "compute refused",
    "unavailable",
    "denied",
    "disabled",
];

const REPAIRS: &[&str] = &[
    "ee graph snapshot refresh --workspace .",
    "ee index rebuild --workspace .",
    "ee config set graph.feature.ppr.enabled true",
    "wait for an active snapshot to release",
    "ee doctor --json",
    "raise the cap",
];

/// Severity ranks used for the escalation invariant. Anything not
/// in this table maps to rank 0 (matching the helper's
/// implementation, which falls back to 0 for unknown severities).
const SEVERITY_RANKS: &[(&str, u8)] = &[
    ("info", 0),
    ("low", 1),
    ("warning", 2),
    ("medium", 3),
    ("high", 4),
    ("critical", 5),
];

fn severity_rank(severity: &str) -> u8 {
    SEVERITY_RANKS
        .iter()
        .find_map(|(name, rank)| (*name == severity).then_some(*rank))
        .unwrap_or(0)
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Build the input vector. Bytes are interpreted as 5-byte
    // tuples; trailing bytes are dropped. Empty input is allowed
    // and exercises the empty-output path.
    let mut entries: Vec<(&'static str, DegradationReport)> = Vec::new();
    let mut by_code_max_rank: std::collections::BTreeMap<&'static str, u8> =
        std::collections::BTreeMap::new();

    for chunk in data.chunks_exact(TUPLE_BYTES) {
        let source = SOURCES[chunk[0] as usize % SOURCES.len()];
        let code = CODES[chunk[1] as usize % CODES.len()];
        let severity = SEVERITIES[chunk[2] as usize % SEVERITIES.len()];
        let message = MESSAGES[chunk[3] as usize % MESSAGES.len()];
        let repair = REPAIRS[chunk[4] as usize % REPAIRS.len()];

        let rank = severity_rank(severity);
        by_code_max_rank
            .entry(code)
            .and_modify(|current| {
                if rank > *current {
                    *current = rank;
                }
            })
            .or_insert(rank);

        entries.push((
            source,
            DegradationReport {
                code,
                severity,
                message,
                repair,
            },
        ));
    }

    // Cap the input length to keep the per-iteration cost bounded.
    // The truncation cap branch is still exercised because the
    // helper has 20 max aggregates regardless of input length.
    entries.truncate(1024);

    let aggregates = aggregate_degraded(entries.clone());

    // Invariant 1: panic-freedom is implicit if we get here.

    // Invariant 2: truncation cap.
    assert!(
        aggregates.len() <= DEGRADED_AGGREGATION_MAX_ENTRIES,
        "aggregates length {} exceeds cap",
        aggregates.len()
    );

    // Partition aggregates into "real" entries vs the trailer (if
    // present).
    let trailer_position = aggregates
        .iter()
        .position(|entry| entry.code == DEGRADED_AGGREGATION_TRUNCATED_CODE);
    let real_aggregates: &[AggregatedDegradation] = match trailer_position {
        Some(idx) => {
            // Trailer must be the last entry, by contract.
            assert_eq!(
                idx,
                aggregates.len() - 1,
                "truncation trailer must be the last entry"
            );
            &aggregates[..idx]
        }
        None => &aggregates[..],
    };

    // Invariant 3: sources sorted and deduped inside every real
    // aggregate.
    for aggregate in real_aggregates {
        let sources = &aggregate.sources;
        for pair in sources.windows(2) {
            assert!(pair[0] <= pair[1], "sources not sorted: {:?}", sources);
            assert_ne!(pair[0], pair[1], "duplicate source in aggregate");
        }
    }

    // Invariant 4: severity escalation. For each real aggregate,
    // its severity must equal the maximum-rank severity observed
    // across all emitters of that code in the input.
    for aggregate in real_aggregates {
        if let Some(expected_rank) = by_code_max_rank.get(aggregate.code.as_str()) {
            let observed_rank = severity_rank(aggregate.severity.as_str());
            assert_eq!(
                observed_rank, *expected_rank,
                "aggregate {} severity rank {} != expected max rank {}",
                aggregate.code, observed_rank, *expected_rank
            );
        }
    }

    // Invariant 6: each distinct input code appears at most once
    // across the non-trailer aggregates.
    let mut codes_seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for aggregate in real_aggregates {
        assert!(
            codes_seen.insert(aggregate.code.as_str()),
            "code {} appeared more than once across non-trailer aggregates",
            aggregate.code
        );
    }

    // Invariant 5: determinism. Running the helper a second time
    // on the same input must produce byte-identical JSON.
    let aggregates_again = aggregate_degraded(entries);
    let json_first = serde_json::to_string(&aggregates).expect("first serialize");
    let json_second = serde_json::to_string(&aggregates_again).expect("second serialize");
    assert_eq!(
        json_first, json_second,
        "aggregate_degraded output is not deterministic"
    );
});
