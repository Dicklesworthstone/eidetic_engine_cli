#![no_main]

//! Fuzz target for `ee::core::graph_telemetry` emission helpers.
//!
//! The seven `emit_*` helpers shipped in commit `b2b2611` are
//! side-effecting wrappers over `tracing::{info,debug,warn,trace}!`
//! at well-known target names. Without a `tracing-subscriber`
//! attached the events are dropped, but the helpers must still
//! never panic, no matter what byte input shaped the payload.
//!
//! This fuzz target asserts the invariants that don't depend on
//! a subscriber:
//!
//! 1. **Panic-freedom across all seven `emit_*` helpers** on
//!    arbitrary inputs, including empty strings, all-ASCII
//!    strings, and `u64::MAX` / `usize::MAX` numeric extremes.
//! 2. **`ALL_GRAPH_TELEMETRY_EVENTS` is the canonical sweep
//!    slice.** Every constant exported by the module is present
//!    in the slice, with no duplicates and the documented
//!    `ee.graph.` prefix.
//! 3. **`CacheEvictReason::as_str` produces stable
//!    snake_case wire strings.** Each variant maps to a unique,
//!    non-empty, lowercase + underscore-only string. Query
//!    languages rely on this for case-insensitive matching.
//!
//! The subscriber-side invariants (target name + level + field
//! set per helper) are already covered by the inline tests in
//! `src/core/graph_telemetry.rs::tests`; replicating them here
//! would require pulling `tracing-subscriber` into the fuzz
//! dependency tree, which would balloon the fuzz target build
//! without exercising any new branch.
//!
//! Keeping the fuzz target side-effect-only also makes it
//! libfuzzer-friendly: no per-iteration subscriber setup +
//! teardown, no global state to leak between runs.

use libfuzzer_sys::fuzz_target;

use ee::core::graph_telemetry::{
    ALGORITHM_CANCELLED_EVENT, ALGORITHM_COMPUTE_EVENT, ALGORITHM_TIMEOUT_EVENT,
    ALL_GRAPH_TELEMETRY_EVENTS, AlgorithmCancelledEvent, AlgorithmComputeEvent,
    AlgorithmTimeoutEvent, CACHE_EVICT_EVENT, CACHE_HIT_EVENT, CACHE_MISS_EVENT, CacheEvictEvent,
    CacheEvictReason, CacheOutcomeEvent, SNAPSHOT_REFRESH_EVENT, SnapshotRefreshEvent,
    emit_algorithm_cancelled, emit_algorithm_compute, emit_algorithm_timeout, emit_cache_evict,
    emit_cache_hit, emit_cache_miss, emit_snapshot_refresh,
};

const MAX_INPUT_BYTES: usize = 512;

const GRAPH_TYPES: &[&str] = &[
    "memory_links",
    "causal_evidence",
    "revision_dag",
    "rule_provenance_bipartite",
    "contradictions",
    "",
];

const ALGORITHMS: &[&str] = &[
    "personalized_pagerank",
    "louvain",
    "voronoi",
    "ego_expand",
    "pack_dna",
    "hits",
    "betweenness",
    "kcore",
    "minhash_rank",
    "bipartite_provenance",
    "ad_hoc_ppr",
    "",
];

const SNAPSHOT_IDS: &[&str] = &[
    "snap_01HX",
    "snap_old",
    "snap_active",
    "snap_archived",
    "snap_drift",
    "",
];

const PARAMS_HASHES: &[&str] = &[
    "blake3:cafe",
    "blake3:beef",
    "blake3:0000",
    "blake3:000102030405060708090a0b0c0d0e0f",
    "",
];

fn pick<'a>(table: &'a [&'a str], byte: u8) -> &'a str {
    table[byte as usize % table.len()]
}

fn extreme_u64(byte: u8) -> u64 {
    // Map the byte through five extreme buckets so the corpus
    // regularly hits boundary values without random byte chaos
    // having to land on them.
    match byte % 5 {
        0 => 0,
        1 => u64::from(byte),
        2 => u64::from(byte).saturating_mul(1_000_000),
        3 => u64::MAX / 2,
        _ => u64::MAX,
    }
}

fn extreme_usize(byte: u8) -> usize {
    match byte % 5 {
        0 => 0,
        1 => usize::from(byte),
        2 => usize::from(byte).saturating_mul(1_000_000),
        3 => usize::MAX / 2,
        _ => usize::MAX,
    }
}

fn pick_evict_reason(byte: u8) -> CacheEvictReason {
    match byte % 3 {
        0 => CacheEvictReason::TtlExpired,
        1 => CacheEvictReason::SnapshotArchived,
        _ => CacheEvictReason::OperatorRequest,
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Pad to a fixed buffer so indexing is bounds-free.
    let mut padded = [0u8; 32];
    let copy_len = data.len().min(padded.len());
    padded[..copy_len].copy_from_slice(&data[..copy_len]);

    // --- Invariant 1: panic-freedom across all seven helpers. ---

    emit_snapshot_refresh(SnapshotRefreshEvent {
        graph_type: pick(GRAPH_TYPES, padded[0]),
        snapshot_version: extreme_u64(padded[1]),
        build_ms: extreme_u64(padded[2]),
        node_count: extreme_usize(padded[3]),
        edge_count: extreme_usize(padded[4]),
        lock_wait_ms: extreme_u64(padded[5]),
    });

    emit_algorithm_compute(AlgorithmComputeEvent {
        algorithm: pick(ALGORITHMS, padded[6]),
        snapshot_id: pick(SNAPSHOT_IDS, padded[7]),
        params_hash: pick(PARAMS_HASHES, padded[8]),
        elapsed_ms: extreme_u64(padded[9]),
        cache_hit: padded[10] & 1 == 1,
        sampling_used: padded[10] & 2 == 2,
    });

    emit_algorithm_timeout(AlgorithmTimeoutEvent {
        algorithm: pick(ALGORITHMS, padded[11]),
        snapshot_id: pick(SNAPSHOT_IDS, padded[12]),
        budget_ms: extreme_u64(padded[13]),
        elapsed_ms: extreme_u64(padded[14]),
    });

    emit_algorithm_cancelled(AlgorithmCancelledEvent {
        algorithm: pick(ALGORITHMS, padded[15]),
        elapsed_ms: extreme_u64(padded[16]),
    });

    emit_cache_hit(CacheOutcomeEvent {
        algorithm: pick(ALGORITHMS, padded[17]),
        params_hash: pick(PARAMS_HASHES, padded[18]),
    });

    emit_cache_miss(CacheOutcomeEvent {
        algorithm: pick(ALGORITHMS, padded[19]),
        params_hash: pick(PARAMS_HASHES, padded[20]),
    });

    emit_cache_evict(CacheEvictEvent {
        reason: pick_evict_reason(padded[21]),
        count: u32::from(padded[22]),
    });

    // --- Invariant 2: ALL_GRAPH_TELEMETRY_EVENTS sweep slice. ---

    // Sweep slice must contain every module-level constant exactly
    // once, in some order, with the documented prefix.
    let canonical: std::collections::BTreeSet<&'static str> = [
        SNAPSHOT_REFRESH_EVENT,
        ALGORITHM_COMPUTE_EVENT,
        ALGORITHM_TIMEOUT_EVENT,
        ALGORITHM_CANCELLED_EVENT,
        CACHE_HIT_EVENT,
        CACHE_MISS_EVENT,
        CACHE_EVICT_EVENT,
    ]
    .into_iter()
    .collect();
    let from_sweep: std::collections::BTreeSet<&'static str> =
        ALL_GRAPH_TELEMETRY_EVENTS.iter().copied().collect();
    assert_eq!(
        canonical, from_sweep,
        "ALL_GRAPH_TELEMETRY_EVENTS drifted from the module constants"
    );
    assert_eq!(
        ALL_GRAPH_TELEMETRY_EVENTS.len(),
        canonical.len(),
        "ALL_GRAPH_TELEMETRY_EVENTS contains duplicates"
    );
    for target in ALL_GRAPH_TELEMETRY_EVENTS {
        assert!(
            target.starts_with("ee.graph."),
            "{target} must use the documented ee.graph. prefix"
        );
        assert!(!target.is_empty());
    }

    // --- Invariant 3: CacheEvictReason wire-string properties. ---

    let reasons = [
        CacheEvictReason::TtlExpired,
        CacheEvictReason::SnapshotArchived,
        CacheEvictReason::OperatorRequest,
    ];
    let mut seen: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for reason in reasons {
        let s = reason.as_str();
        assert!(!s.is_empty(), "wire string must be non-empty");
        assert!(
            s.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{s} must be lower_snake_case so query languages can match without case folding"
        );
        assert!(
            !s.starts_with('_') && !s.ends_with('_'),
            "{s} must not have leading/trailing underscores"
        );
        assert!(!s.contains("__"), "{s} must not have double underscores");
        assert!(
            seen.insert(s),
            "wire string {s} is not unique across CacheEvictReason"
        );
    }
    assert_eq!(
        seen.len(),
        reasons.len(),
        "every reason must have a unique wire string"
    );
});
