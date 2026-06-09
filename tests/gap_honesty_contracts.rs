//! bd-1n0np.6.5 — consolidated gap-honesty contract tests.
//!
//! Pins the *public* contracts of the gap-honesty surfaces that shipped under
//! epic bd-1n0np.6 (blind-spot map, query-miss clustering, knowledge_gap
//! candidates, swarm-brief surfacing). The per-module unit tests already cover
//! internals (blind-spot set-arithmetic in `cli::insights`, miss-detection
//! thresholds in `core::search`, clustering determinism in
//! `core::query_miss_cluster`); these tests assert the cross-cutting, surface-
//! level invariants a consumer relies on:
//!   * knowledge_gap clustering is deterministic + threshold-honest (advisory,
//!     never fabricated from a single miss);
//!   * the redaction-driven hash-only clustering path agrees with the threshold;
//!   * the published `ee.swarm.brief.v1` schema documents the `knowledgeGaps`
//!     surface that `swarm_brief` now emits (schema-vs-surface drift guard).
//!
//! Goldens for `ee.insights.blind_spots.v1` + the knowledge_gap candidate JSON
//! body are RCH-remote-regen only (Mac-local UPDATE_GOLDEN injects `/Users`
//! paths + version drift that break server CI) and are owed there.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use ee::core::query_miss_cluster::{
    KNOWLEDGE_GAP_MIN_CLUSTER_MISSES, MissAuditObservation, QueryMissObservation,
    cluster_query_misses, cluster_repeated_misses, query_cluster_key,
};

fn miss(query: &str, count: u32) -> QueryMissObservation {
    QueryMissObservation {
        query: query.to_string(),
        miss_count: count,
    }
}

fn miss_audit(query_hash: &str, reason: &str) -> MissAuditObservation {
    MissAuditObservation {
        query_hash: query_hash.to_string(),
        reason: reason.to_string(),
    }
}

#[test]
fn knowledge_gap_threshold_is_honest_no_single_miss_becomes_a_gap() {
    // A one-off miss is noise, not a knowledge gap: below the threshold yields
    // nothing, at/above it yields exactly one candidate.
    let one_off = vec![miss("kubernetes pod eviction", 1)];
    assert!(
        cluster_query_misses(&one_off, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES).is_empty(),
        "a single miss must never be promoted to a knowledge gap"
    );

    let repeated = vec![miss(
        "kubernetes pod eviction",
        KNOWLEDGE_GAP_MIN_CLUSTER_MISSES,
    )];
    let gaps = cluster_query_misses(&repeated, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].miss_count, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES);
}

#[test]
fn knowledge_gap_clustering_is_order_independent() {
    let forward = vec![
        miss("flaky socket timeout", 2),
        miss("socket timeout flaky", 2),
        miss("unrelated build error", 3),
    ];
    let mut reversed = forward.clone();
    reversed.reverse();
    assert_eq!(
        cluster_query_misses(&forward, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES),
        cluster_query_misses(&reversed, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES),
        "knowledge_gap candidates must be independent of observation order"
    );
}

#[test]
fn cluster_key_collapses_paraphrases_but_separates_topics() {
    assert_eq!(
        query_cluster_key("Restart the FAILED pod"),
        query_cluster_key("failed pod restart the")
    );
    assert_ne!(
        query_cluster_key("restart failed pod"),
        query_cluster_key("delete failed pod")
    );
}

#[test]
fn redacted_hash_path_clusters_only_repeated_misses() {
    // When the query text is redacted (bd-1n0np.6.3) the only honest signal is
    // an exact query hash recurring at/above the threshold.
    let mut observations = Vec::new();
    for _ in 0..KNOWLEDGE_GAP_MIN_CLUSTER_MISSES {
        observations.push(miss_audit("blake3:repeated", "no_relevant_results"));
    }
    observations.push(miss_audit("blake3:lonely", "no_relevant_results"));

    let gaps = cluster_repeated_misses(&observations, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES);
    assert_eq!(
        gaps.len(),
        1,
        "only the repeated hash crosses the threshold"
    );
    assert_eq!(gaps[0].query_hash, "blake3:repeated");
    assert_eq!(gaps[0].miss_count, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES);
}

fn schema_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn swarm_brief_schema_documents_knowledge_gaps_surface() {
    // Drift guard: swarm_brief now emits `knowledgeGaps`; the published schema
    // (additionalProperties:false) must document it or a real brief with gaps
    // would fail conformance.
    let raw = fs::read_to_string(schema_path("docs/schemas/swarm/ee.swarm.brief.v1.json"))
        .expect("read ee.swarm.brief.v1.json");
    let schema: serde_json::Value = serde_json::from_str(&raw).expect("valid swarm brief schema");

    let knowledge_gaps = &schema["properties"]["knowledgeGaps"];
    assert_eq!(
        knowledge_gaps["type"], "array",
        "knowledgeGaps must be a documented array property"
    );
    let item_required = &knowledge_gaps["items"]["required"];
    let required: Vec<&str> = item_required
        .as_array()
        .expect("knowledgeGaps item required array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    for field in ["queryHash", "missCount", "reasons"] {
        assert!(
            required.contains(&field),
            "knowledgeGaps item schema must require `{field}`"
        );
    }

    // The surface is optional (advisory / skip-when-empty), so it must NOT be in
    // the top-level required set — briefs without misses keep their shape.
    let top_required: Vec<&str> = schema["required"]
        .as_array()
        .expect("schema required array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        !top_required.contains(&"knowledgeGaps"),
        "knowledgeGaps must stay optional, not top-level required"
    );
}

#[test]
fn insights_blind_spots_schema_is_published_and_wellformed() {
    let raw = fs::read_to_string(schema_path("docs/schemas/ee.insights.v1.json"))
        .expect("read ee.insights.v1.json");
    let schema: serde_json::Value = serde_json::from_str(&raw).expect("valid insights schema");
    let serialized = serde_json::to_string(&schema).expect("reserialize");
    assert!(
        serialized.contains("blindSpots") || serialized.contains("blind_spots"),
        "insights schema must document the blindSpots section"
    );
}
