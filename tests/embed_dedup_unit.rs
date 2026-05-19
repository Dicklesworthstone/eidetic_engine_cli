//! Contract-style unit coverage for the bd-1iltv insert-time embedding
//! dedup substrate.
//!
//! These tests intentionally exercise only the public SimHash/cosine helpers.
//! The remember-path hook, database `content_simhash` column, env registry, and
//! `ee why` dedupLink output are covered by later slices.

use ee::search::simhash::{
    SimHash128, first_confirmed_simhash_candidate, hamming_distance, simhash_128,
};
use serde_json::Value;

const DEFAULT_HAMMING_K: u32 = 12;
const DEFAULT_COSINE_FLOOR: f32 = 0.97;
const EMBED_DEDUP_CHAIN_GOLDEN: &str = include_str!("golden/embed_dedup_chain.json");

fn assert_close(actual: f32, expected: f32) {
    let delta = (actual - expected).abs();
    assert!(
        delta <= f32::EPSILON,
        "expected {actual} to equal {expected} within f32 epsilon; delta={delta}"
    );
}

#[test]
fn golden_dedup_chain_pins_public_dedup_link_shape() {
    let golden: Value = serde_json::from_str(EMBED_DEDUP_CHAIN_GOLDEN)
        .expect("embed_dedup_chain golden must be valid JSON");
    assert_eq!(
        golden.get("schema").and_then(Value::as_str),
        Some("ee.embed_dedup.chain_golden.v1")
    );

    let remember_results = golden
        .get("rememberResults")
        .and_then(Value::as_array)
        .expect("golden must include rememberResults");
    let exact_duplicate = remember_results
        .iter()
        .find(|case| {
            case.get("case").and_then(Value::as_str) == Some("exact_or_canonical_duplicate")
        })
        .expect("golden must include exact duplicate case");
    let exact_link = exact_duplicate
        .get("dedupLink")
        .and_then(Value::as_object)
        .expect("exact duplicate case must include dedupLink");

    for field in [
        "targetMemoryId",
        "relationship",
        "hammingDistance",
        "cosineSimilarity",
        "cosineFloor",
        "decision",
    ] {
        assert!(exact_link.contains_key(field), "dedupLink missing {field}");
    }
    assert_eq!(
        exact_link.get("targetMemoryId").and_then(Value::as_str),
        Some("mem_embed_dedup_source")
    );
    assert_eq!(
        exact_link.get("relationship").and_then(Value::as_str),
        Some("embedding_reuse")
    );
    assert_eq!(
        exact_link.get("hammingDistance").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        exact_link.get("cosineSimilarity").and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(
        exact_link.get("cosineFloor").and_then(Value::as_f64),
        Some(0.97)
    );
    assert_eq!(
        exact_link.get("decision").and_then(Value::as_str),
        Some("reuse")
    );

    let false_positive = remember_results
        .iter()
        .find(|case| case.get("case").and_then(Value::as_str) == Some("simhash_false_positive"))
        .expect("golden must include SimHash false-positive case");
    assert!(
        false_positive.get("dedupLink").is_some_and(Value::is_null),
        "cosine rejection must not emit a dedupLink"
    );
    assert_eq!(
        false_positive
            .pointer("/dedupDecision/reason")
            .and_then(Value::as_str),
        Some("cosine_under_floor")
    );

    let why_results = golden
        .get("whyResults")
        .and_then(Value::as_array)
        .expect("golden must include whyResults");
    let why_exact = why_results
        .iter()
        .find(|case| case.get("memoryId").and_then(Value::as_str) == Some("mem_embed_dedup_exact"))
        .expect("golden must include why output for reused embedding");
    assert_eq!(
        why_exact
            .pointer("/dedupLink/targetMemoryId")
            .and_then(Value::as_str),
        Some("mem_embed_dedup_source")
    );
}

#[test]
fn exact_content_reuses_existing_embedding() {
    let content = "Run cargo fmt --check before release verification.";
    let query_fingerprint = simhash_128(content);
    let query_embedding = [1.0, 0.0, 0.0, 0.0];
    let existing_embedding = [1.0, 0.0, 0.0, 0.0];
    let candidates = [(
        "mem_existing",
        query_fingerprint,
        existing_embedding.as_slice(),
    )];

    let selected = first_confirmed_simhash_candidate(
        query_fingerprint,
        &query_embedding,
        candidates,
        DEFAULT_HAMMING_K,
        DEFAULT_COSINE_FLOOR,
    );
    assert!(selected.is_some(), "expected a confirmed dedup candidate");
    let selected = match selected {
        Some(selected) => selected,
        None => return,
    };

    assert_eq!(selected.candidate_id, "mem_existing");
    assert_eq!(selected.fingerprint, query_fingerprint);
    assert_eq!(selected.hamming_distance, 0);
    assert!(selected.cosine.confirmed);
    assert_close(selected.cosine.similarity, 1.0);
    assert_close(selected.cosine.floor, DEFAULT_COSINE_FLOOR);
}

#[test]
fn whitespace_and_case_variant_reuses_existing_embedding() {
    let stored = "agents must route cargo verification through rch";
    let query = "  AGENTS   must\troute\ncargo verification through RCH  ";
    let stored_fingerprint = simhash_128(stored);
    let query_fingerprint = simhash_128(query);
    let query_embedding = [0.8, 0.2, 0.0];
    let existing_embedding = [0.8, 0.2, 0.0];
    let candidates = [(
        "mem_whitespace_case",
        stored_fingerprint,
        existing_embedding.as_slice(),
    )];

    assert_eq!(hamming_distance(query_fingerprint, stored_fingerprint), 0);

    let selected = first_confirmed_simhash_candidate(
        query_fingerprint,
        &query_embedding,
        candidates,
        DEFAULT_HAMMING_K,
        DEFAULT_COSINE_FLOOR,
    );
    assert!(selected.is_some(), "expected a confirmed dedup candidate");
    let selected = match selected {
        Some(selected) => selected,
        None => return,
    };

    assert_eq!(selected.candidate_id, "mem_whitespace_case");
    assert_eq!(selected.hamming_distance, 0);
    assert!(selected.cosine.confirmed);
}

#[test]
fn simhash_false_positive_under_cosine_floor_does_not_reuse() {
    let query_fingerprint = simhash_128("same cheap gate, different meaning");
    let query_embedding = [1.0, 0.0, 0.0];
    let false_positive_embedding = [0.0, 1.0, 0.0];
    let candidates = [(
        "mem_false_positive",
        query_fingerprint,
        false_positive_embedding.as_slice(),
    )];

    let selected = first_confirmed_simhash_candidate(
        query_fingerprint,
        &query_embedding,
        candidates,
        DEFAULT_HAMMING_K,
        DEFAULT_COSINE_FLOOR,
    );

    assert_eq!(selected, None);
}

#[test]
fn cosine_rejection_does_not_block_later_confirmed_candidate() {
    let query_fingerprint = SimHash128::from_u128(0);
    let query_embedding = [1.0, 0.0, 0.0];
    let rejected_embedding = [0.0, 1.0, 0.0];
    let confirmed_embedding = [1.0, 0.0, 0.0];
    let candidates = [
        (
            "mem_nearest_but_rejected",
            SimHash128::from_u128(0b0001),
            rejected_embedding.as_slice(),
        ),
        (
            "mem_farther_confirmed",
            SimHash128::from_u128(0b0011),
            confirmed_embedding.as_slice(),
        ),
    ];

    let selected = first_confirmed_simhash_candidate(
        query_fingerprint,
        &query_embedding,
        candidates,
        DEFAULT_HAMMING_K,
        DEFAULT_COSINE_FLOOR,
    );
    assert!(selected.is_some(), "expected a confirmed dedup candidate");
    let selected = match selected {
        Some(selected) => selected,
        None => return,
    };

    assert_eq!(selected.candidate_id, "mem_farther_confirmed");
    assert_eq!(selected.hamming_distance, 2);
    assert!(selected.cosine.confirmed);
}

#[test]
fn candidates_outside_default_hamming_threshold_do_not_reuse() {
    let query_fingerprint = SimHash128::from_u128(0);
    let far_fingerprint = SimHash128::from_u128(0x1fff);
    let query_embedding = [1.0, 0.0];
    let far_embedding = [1.0, 0.0];
    let candidates = [(
        "mem_outside_hamming_k",
        far_fingerprint,
        far_embedding.as_slice(),
    )];

    assert!(hamming_distance(query_fingerprint, far_fingerprint) > DEFAULT_HAMMING_K);

    let selected = first_confirmed_simhash_candidate(
        query_fingerprint,
        &query_embedding,
        candidates,
        DEFAULT_HAMMING_K,
        DEFAULT_COSINE_FLOOR,
    );

    assert_eq!(selected, None);
}

#[test]
fn content_simhash_storage_encoding_is_exactly_sixteen_big_endian_bytes() {
    let fingerprint = simhash_128("content_simhash must be stored as BLOB(16)");
    let encoded = fingerprint.to_be_bytes();
    let decoded = SimHash128::from_be_bytes(encoded);

    assert_eq!(encoded.len(), 16);
    assert_eq!(decoded, fingerprint);
    assert_eq!(decoded.to_u128(), u128::from_be_bytes(encoded));
}
