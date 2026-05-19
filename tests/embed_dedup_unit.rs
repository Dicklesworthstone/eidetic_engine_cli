//! Contract-style unit coverage for the bd-1iltv insert-time embedding
//! dedup substrate.
//!
//! These tests intentionally exercise only the public SimHash/cosine helpers.
//! The remember-path hook, database `content_simhash` column, env registry, and
//! `ee why` dedupLink output are covered by later slices.

use ee::search::simhash::{
    SimHash128, first_confirmed_simhash_candidate, hamming_distance, simhash_128,
};

const DEFAULT_HAMMING_K: u32 = 12;
const DEFAULT_COSINE_FLOOR: f32 = 0.97;

fn assert_close(actual: f32, expected: f32) {
    let delta = (actual - expected).abs();
    assert!(
        delta <= f32::EPSILON,
        "expected {actual} to equal {expected} within f32 epsilon; delta={delta}"
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
