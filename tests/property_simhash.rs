//! Property tests for the 128-bit Charikar SimHash scaffold landed by
//! bd-3goqk (commit 208de5c1). The inline `#[cfg(test)]` cases in
//! `src/search/simhash.rs` pin specific examples; this harness exercises
//! the same contracts over a much wider input distribution so a future
//! tightening or accidental rewrite of the canonicalization or projection
//! cannot silently regress an entire input class.
//!
//! Tracked under bd-2ct3h (follow-up to bd-3goqk under bd-1iltv).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::search::simhash::{
    NearestSimHashCandidate, SimHash128, canonicalize_content_for_simhash,
    confirm_cosine_similarity, cosine_similarity, first_confirmed_simhash_candidate,
    hamming_distance, ranked_simhash_candidates, simhash_128,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

/// Cap the generated string length to keep proptest runs fast under the
/// `cargo test` budget while still exploring meaningful structural
/// variety.
const MAX_CONTENT_LEN: usize = 256;
const MAX_EMBEDDING_DIMENSIONS: usize = 32;

/// Canonicalization treats punctuation and any Unicode whitespace as a
/// token boundary plus a lowercase fold. Restrict the generator to a
/// printable ASCII alphabet so the property tests stay deterministic
/// across operating systems without depending on platform Unicode
/// tables.
fn printable_ascii_content() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<u8>(), 0..MAX_CONTENT_LEN).prop_map(|bytes| {
        bytes
            .into_iter()
            .map(|b| {
                let c = (b % 95) + 32;
                c as char
            })
            .collect::<String>()
    })
}

/// Generator for byte vectors that decode into a SimHash128 through the
/// big-endian byte path. Used by the round-trip property.
fn arbitrary_simhash() -> impl Strategy<Value = SimHash128> {
    proptest::collection::vec(any::<u8>(), 16..=16).prop_map(|bytes| {
        let mut arr = [0_u8; 16];
        arr.copy_from_slice(&bytes);
        SimHash128::from_be_bytes(arr)
    })
}

fn finite_embedding_vector() -> impl Strategy<Value = Vec<f32>> {
    proptest::collection::vec(-1000.0_f32..1000.0, 1..=MAX_EMBEDDING_DIMENSIONS)
}

fn finite_embedding_pair() -> impl Strategy<Value = (Vec<f32>, Vec<f32>)> {
    (1_usize..=MAX_EMBEDDING_DIMENSIONS).prop_flat_map(|dimensions| {
        (
            proptest::collection::vec(-1000.0_f32..1000.0, dimensions),
            proptest::collection::vec(-1000.0_f32..1000.0, dimensions),
        )
    })
}

fn non_finite_f32() -> impl Strategy<Value = f32> {
    prop_oneof![Just(f32::NAN), Just(f32::INFINITY), Just(f32::NEG_INFINITY)]
}

fn has_non_zero_norm(values: &[f32]) -> bool {
    values.iter().any(|value| *value != 0.0)
}

fn reference_cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut left_norm_sq = 0.0_f64;
    let mut right_norm_sq = 0.0_f64;
    for (&left_value, &right_value) in left.iter().zip(right.iter()) {
        if !left_value.is_finite() || !right_value.is_finite() {
            return None;
        }
        let left_value = f64::from(left_value);
        let right_value = f64::from(right_value);
        dot += left_value * right_value;
        left_norm_sq += left_value * left_value;
        right_norm_sq += right_value * right_value;
    }
    if left_norm_sq == 0.0 || right_norm_sq == 0.0 {
        return None;
    }
    let denominator = left_norm_sq.sqrt() * right_norm_sq.sqrt();
    Some((dot / denominator).clamp(-1.0, 1.0) as f32)
}

fn ranked_fixture_candidates(raw: Vec<u128>) -> Vec<(String, SimHash128)> {
    raw.into_iter()
        .enumerate()
        .map(|(index, fingerprint)| {
            (
                format!("mem_{index:03}"),
                SimHash128::from_u128(fingerprint),
            )
        })
        .collect()
}

fn confirmed_fixture_candidates(raw: Vec<(u128, bool)>) -> Vec<(String, SimHash128, [f32; 2])> {
    raw.into_iter()
        .enumerate()
        .map(|(index, (fingerprint, should_confirm))| {
            let embedding = if should_confirm {
                [1.0, 0.0]
            } else {
                [0.0, 1.0]
            };
            (
                format!("mem_{index:03}"),
                SimHash128::from_u128(fingerprint),
                embedding,
            )
        })
        .collect()
}

fn expected_ranked_candidates<'a>(
    query: SimHash128,
    candidates: &'a [(String, SimHash128)],
    max_hamming_distance: u32,
    limit: usize,
) -> Vec<NearestSimHashCandidate<'a>> {
    let mut expected: Vec<_> = candidates
        .iter()
        .filter_map(|(candidate_id, fingerprint)| {
            let hamming_distance = hamming_distance(query, *fingerprint);
            (hamming_distance <= max_hamming_distance).then_some(NearestSimHashCandidate {
                candidate_id: candidate_id.as_str(),
                fingerprint: *fingerprint,
                hamming_distance,
            })
        })
        .collect();
    expected.sort_by(|left, right| {
        left.hamming_distance
            .cmp(&right.hamming_distance)
            .then_with(|| left.candidate_id.cmp(right.candidate_id))
    });
    expected.truncate(limit);
    expected
}

fn expected_first_confirmed_candidate(
    query: SimHash128,
    candidates: &[(String, SimHash128, [f32; 2])],
    max_hamming_distance: u32,
) -> Option<(String, SimHash128, u32)> {
    let mut expected: Vec<_> = candidates
        .iter()
        .filter_map(|(candidate_id, fingerprint, embedding)| {
            let hamming_distance = hamming_distance(query, *fingerprint);
            (hamming_distance <= max_hamming_distance && embedding[0] == 1.0).then_some((
                candidate_id.clone(),
                *fingerprint,
                hamming_distance,
            ))
        })
        .collect();
    expected.sort_by(|left, right| left.2.cmp(&right.2).then_with(|| left.0.cmp(&right.0)));
    expected.into_iter().next()
}

fn config() -> ProptestConfig {
    ProptestConfig::with_cases(128)
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn prop_simhash_is_deterministic(content in printable_ascii_content()) {
        let first = simhash_128(&content);
        let second = simhash_128(&content);
        prop_assert_eq!(first, second);
    }

    #[test]
    fn prop_simhash_is_lowercase_invariant(content in printable_ascii_content()) {
        let lower = content.to_lowercase();
        let upper = content.to_uppercase();
        prop_assert_eq!(simhash_128(&lower), simhash_128(&upper));
    }

    #[test]
    fn prop_simhash_is_whitespace_collapse_invariant(
        tokens in proptest::collection::vec("[a-zA-Z0-9]{1,8}", 0..16)
    ) {
        let single_space = tokens.join(" ");
        let many_spaces = tokens.join("    \t  \n ");
        let surrounded = format!("\t\n  {single_space}   \n");
        let baseline = simhash_128(&single_space);
        prop_assert_eq!(baseline, simhash_128(&many_spaces));
        prop_assert_eq!(baseline, simhash_128(&surrounded));
    }

    #[test]
    fn prop_canonicalize_is_idempotent(content in printable_ascii_content()) {
        let once = canonicalize_content_for_simhash(&content);
        let twice = canonicalize_content_for_simhash(&once);
        prop_assert_eq!(once, twice);
    }

    #[test]
    fn prop_canonicalize_never_introduces_uppercase(content in printable_ascii_content()) {
        let canonical = canonicalize_content_for_simhash(&content);
        for ch in canonical.chars() {
            prop_assert!(
                !ch.is_uppercase(),
                "canonical output must be lowercase; found {ch:?} in {canonical:?}"
            );
        }
    }

    #[test]
    fn prop_canonicalize_never_emits_consecutive_spaces(
        content in printable_ascii_content()
    ) {
        let canonical = canonicalize_content_for_simhash(&content);
        prop_assert!(
            !canonical.contains("  "),
            "canonical output must collapse whitespace; got {canonical:?}"
        );
        prop_assert!(
            !canonical.starts_with(' '),
            "canonical output must not start with space; got {canonical:?}"
        );
        prop_assert!(
            !canonical.ends_with(' '),
            "canonical output must not end with space; got {canonical:?}"
        );
    }

    #[test]
    fn prop_hamming_distance_is_symmetric(
        a in arbitrary_simhash(),
        b in arbitrary_simhash(),
    ) {
        prop_assert_eq!(hamming_distance(a, b), hamming_distance(b, a));
    }

    #[test]
    fn prop_hamming_distance_is_bounded(
        a in arbitrary_simhash(),
        b in arbitrary_simhash(),
    ) {
        let distance = hamming_distance(a, b);
        prop_assert!(distance <= 128, "distance {distance} exceeds 128-bit width");
    }

    #[test]
    fn prop_hamming_distance_self_is_zero(a in arbitrary_simhash()) {
        prop_assert_eq!(hamming_distance(a, a), 0);
    }

    #[test]
    fn prop_serde_round_trip_preserves_value(a in arbitrary_simhash()) {
        let serialized = serde_json::to_string(&a)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let restored: SimHash128 = serde_json::from_str(&serialized)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(a, restored);
    }

    #[test]
    fn prop_be_bytes_round_trip_preserves_value(a in arbitrary_simhash()) {
        let bytes = a.to_be_bytes();
        let restored = SimHash128::from_be_bytes(bytes);
        prop_assert_eq!(a, restored);
    }

    #[test]
    fn prop_u128_round_trip_preserves_value(raw in any::<u128>()) {
        let fingerprint = SimHash128::from_u128(raw);
        prop_assert_eq!(fingerprint.to_u128(), raw);
    }

    #[test]
    fn prop_hamming_distance_obeys_xor_popcount_identity(
        a in arbitrary_simhash(),
        b in arbitrary_simhash(),
    ) {
        let derived = (a.to_u128() ^ b.to_u128()).count_ones();
        prop_assert_eq!(hamming_distance(a, b), derived);
    }

    #[test]
    fn prop_hamming_triangle_inequality(
        a in arbitrary_simhash(),
        b in arbitrary_simhash(),
        c in arbitrary_simhash(),
    ) {
        let ab = hamming_distance(a, b);
        let bc = hamming_distance(b, c);
        let ac = hamming_distance(a, c);
        prop_assert!(
            ac <= ab + bc,
            "triangle inequality violated: d(a,c)={ac} > d(a,b)+d(b,c)={}+{}={}",
            ab,
            bc,
            ab + bc,
        );
    }

    #[test]
    fn prop_empty_or_whitespace_only_content_yields_zero_fingerprint(
        spaces in proptest::collection::vec(prop_oneof![Just(' '), Just('\t'), Just('\n')], 0..32)
    ) {
        let content: String = spaces.into_iter().collect();
        prop_assert_eq!(simhash_128(&content), SimHash128::from_u128(0));
    }

    #[test]
    fn prop_ranked_candidates_match_distance_threshold_limit_and_order(
        query in arbitrary_simhash(),
        raw_candidates in proptest::collection::vec(any::<u128>(), 0..32),
        max_hamming_distance in 0_u32..=128,
        limit in 0_usize..=32,
    ) {
        let candidates = ranked_fixture_candidates(raw_candidates);
        let ranked = ranked_simhash_candidates(
            query,
            candidates
                .iter()
                .map(|(candidate_id, fingerprint)| (candidate_id.as_str(), *fingerprint)),
            max_hamming_distance,
            limit,
        );
        let expected =
            expected_ranked_candidates(query, &candidates, max_hamming_distance, limit);

        prop_assert_eq!(&ranked, &expected);
        prop_assert!(ranked.len() <= limit);
        for candidate in &ranked {
            prop_assert!(candidate.hamming_distance <= max_hamming_distance);
            prop_assert_eq!(
                candidate.hamming_distance,
                hamming_distance(query, candidate.fingerprint),
            );
        }
    }

    #[test]
    fn prop_ranked_candidates_are_independent_of_iteration_order(
        query in arbitrary_simhash(),
        raw_candidates in proptest::collection::vec(any::<u128>(), 0..32),
        max_hamming_distance in 0_u32..=128,
        limit in 0_usize..=32,
    ) {
        let candidates = ranked_fixture_candidates(raw_candidates);
        let forward = ranked_simhash_candidates(
            query,
            candidates
                .iter()
                .map(|(candidate_id, fingerprint)| (candidate_id.as_str(), *fingerprint)),
            max_hamming_distance,
            limit,
        );
        let reverse = ranked_simhash_candidates(
            query,
            candidates
                .iter()
                .rev()
                .map(|(candidate_id, fingerprint)| (candidate_id.as_str(), *fingerprint)),
            max_hamming_distance,
            limit,
        );

        prop_assert_eq!(forward, reverse);
    }

    #[test]
    fn prop_first_confirmed_candidate_matches_reference_model(
        query in arbitrary_simhash(),
        raw_candidates in proptest::collection::vec((any::<u128>(), any::<bool>()), 0..32),
        max_hamming_distance in 0_u32..=128,
    ) {
        let query_embedding = [1.0, 0.0];
        let candidates = confirmed_fixture_candidates(raw_candidates);
        let actual = first_confirmed_simhash_candidate(
            query,
            &query_embedding,
            candidates
                .iter()
                .map(|(candidate_id, fingerprint, embedding)| {
                    (candidate_id.as_str(), *fingerprint, embedding.as_slice())
                }),
            max_hamming_distance,
            0.97,
        )
        .map(|candidate| {
            (
                candidate.candidate_id.to_owned(),
                candidate.fingerprint,
                candidate.hamming_distance,
            )
        });
        let expected =
            expected_first_confirmed_candidate(query, &candidates, max_hamming_distance);

        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn prop_first_confirmed_candidate_is_independent_of_iteration_order(
        query in arbitrary_simhash(),
        raw_candidates in proptest::collection::vec((any::<u128>(), any::<bool>()), 0..32),
        max_hamming_distance in 0_u32..=128,
    ) {
        let query_embedding = [1.0, 0.0];
        let candidates = confirmed_fixture_candidates(raw_candidates);
        let forward = first_confirmed_simhash_candidate(
            query,
            &query_embedding,
            candidates
                .iter()
                .map(|(candidate_id, fingerprint, embedding)| {
                    (candidate_id.as_str(), *fingerprint, embedding.as_slice())
                }),
            max_hamming_distance,
            0.97,
        );
        let reverse = first_confirmed_simhash_candidate(
            query,
            &query_embedding,
            candidates
                .iter()
                .rev()
                .map(|(candidate_id, fingerprint, embedding)| {
                    (candidate_id.as_str(), *fingerprint, embedding.as_slice())
                }),
            max_hamming_distance,
            0.97,
        );

        prop_assert_eq!(forward, reverse);
    }

    #[test]
    fn prop_cosine_similarity_matches_reference(
        (left, right) in finite_embedding_pair(),
    ) {
        let actual = cosine_similarity(&left, &right);
        let expected = reference_cosine_similarity(&left, &right);

        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn prop_cosine_similarity_is_symmetric(
        (left, right) in finite_embedding_pair(),
    ) {
        prop_assert_eq!(
            cosine_similarity(&left, &right),
            cosine_similarity(&right, &left),
        );
    }

    #[test]
    fn prop_cosine_similarity_is_bounded_for_non_zero_vectors(
        (left, right) in finite_embedding_pair(),
    ) {
        prop_assume!(has_non_zero_norm(&left));
        prop_assume!(has_non_zero_norm(&right));

        let similarity = cosine_similarity(&left, &right)
            .ok_or_else(|| TestCaseError::fail("non-zero finite vectors must be comparable"))?;

        prop_assert!(
            (-1.0..=1.0).contains(&similarity),
            "cosine similarity must stay in [-1, 1], got {similarity}",
        );
    }

    #[test]
    fn prop_cosine_confirmation_matches_floor_decision(
        (left, right) in finite_embedding_pair(),
        floor in 0.0_f32..=1.0,
    ) {
        prop_assume!(has_non_zero_norm(&left));
        prop_assume!(has_non_zero_norm(&right));

        let confirmation = confirm_cosine_similarity(&left, &right, floor)
            .ok_or_else(|| TestCaseError::fail("non-zero finite vectors must be confirmable"))?;

        prop_assert_eq!(confirmation.floor, floor);
        prop_assert_eq!(confirmation.confirmed, confirmation.similarity >= floor);
        prop_assert_eq!(confirmation.similarity, cosine_similarity(&left, &right).expect("similarity"));
    }

    #[test]
    fn prop_cosine_similarity_rejects_dimension_mismatches(
        left in finite_embedding_vector(),
        extra in -1000.0_f32..1000.0,
    ) {
        let mut right = left.clone();
        right.push(extra);

        prop_assert_eq!(cosine_similarity(&left, &right), None);
        prop_assert_eq!(confirm_cosine_similarity(&left, &right, 0.0), None);
    }

    #[test]
    fn prop_cosine_similarity_rejects_zero_vectors(
        non_zero in finite_embedding_vector(),
    ) {
        prop_assume!(has_non_zero_norm(&non_zero));
        let zero = vec![0.0; non_zero.len()];

        prop_assert_eq!(cosine_similarity(&zero, &non_zero), None);
        prop_assert_eq!(cosine_similarity(&non_zero, &zero), None);
        prop_assert_eq!(confirm_cosine_similarity(&zero, &non_zero, 0.0), None);
        prop_assert_eq!(confirm_cosine_similarity(&non_zero, &zero, 0.0), None);
    }

    #[test]
    fn prop_cosine_similarity_rejects_non_finite_values(
        finite in finite_embedding_vector(),
        bad_value in non_finite_f32(),
    ) {
        let mut with_bad_value = finite.clone();
        with_bad_value[0] = bad_value;

        prop_assert_eq!(cosine_similarity(&with_bad_value, &finite), None);
        prop_assert_eq!(cosine_similarity(&finite, &with_bad_value), None);
        prop_assert_eq!(confirm_cosine_similarity(&with_bad_value, &finite, 0.0), None);
        prop_assert_eq!(confirm_cosine_similarity(&finite, &with_bad_value, 0.0), None);
    }

    #[test]
    fn prop_cosine_confirmation_rejects_non_finite_floor(
        (left, right) in finite_embedding_pair(),
        floor in non_finite_f32(),
    ) {
        prop_assume!(has_non_zero_norm(&left));
        prop_assume!(has_non_zero_norm(&right));

        prop_assert_eq!(confirm_cosine_similarity(&left, &right, floor), None);
    }
}
