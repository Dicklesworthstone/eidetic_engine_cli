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
    SimHash128, canonicalize_content_for_simhash, hamming_distance, simhash_128,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

/// Cap the generated string length to keep proptest runs fast under the
/// `cargo test` budget while still exploring meaningful structural
/// variety.
const MAX_CONTENT_LEN: usize = 256;

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
}
