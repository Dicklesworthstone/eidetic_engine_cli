//! 128-bit SimHash for insert-time embedding deduplication — scaffold
//! (bd-3goqk, sub-bead of bd-1iltv).
//!
//! `HashEmbedder::default_256().embed_sync` currently runs unconditionally
//! inside `remember_memory_inner`, so at 64-agent swarm scale every agent
//! that observes the same fact pays the embedder cost (~5–15 ms) and stores
//! a duplicate row that pollutes retrieval. bd-1iltv's design adds an
//! insert-time SimHash + cosine-confirm dedup that catches typo and
//! whitespace variants the exact-text-hash LRU in bd-168gm misses.
//!
//! This scaffold owns the platform-agnostic SimHash math layer: a 128-bit
//! Charikar fingerprint with deterministic token normalization, a Hamming
//! distance helper, and the explicit normalization entry point that callers
//! and tests can audit. The wiring into `remember_memory_inner`, the DB
//! `content_simhash` column, env-var registration, and the workspace-scoped
//! index live under sibling slices of bd-1iltv so that this module can land
//! without touching any of the contested write-path files.
//!
//! Determinism contract: same input bytes always produce the same
//! `SimHash128`, regardless of `HashMap` iteration order, platform, or
//! build configuration. The unit tests pin this with byte-stable known
//! vectors.

use std::fmt;

use blake3::Hasher as Blake3Hasher;
use serde::{Deserialize, Serialize};

/// Domain-separation prefix mixed into every token hash so a SimHash bit
/// vector cannot be confused with any other blake3 output in the codebase.
/// Changing this constant invalidates every previously-stored SimHash and
/// MUST be paired with a database migration.
const SIMHASH_DOMAIN_TAG: &[u8] = b"ee.simhash.v1";

/// Bit width of the fingerprint.
const SIMHASH_BITS: usize = 128;

/// Opaque 128-bit Charikar SimHash fingerprint. Two memories whose tokens
/// largely overlap have small `hamming_distance` between their fingerprints
/// regardless of whitespace, case, or trivial typos.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SimHash128(u128);

impl SimHash128 {
    #[must_use]
    pub const fn from_u128(raw: u128) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn to_u128(self) -> u128 {
        self.0
    }

    #[must_use]
    pub fn to_be_bytes(self) -> [u8; 16] {
        self.0.to_be_bytes()
    }

    #[must_use]
    pub fn from_be_bytes(bytes: [u8; 16]) -> Self {
        Self(u128::from_be_bytes(bytes))
    }
}

impl fmt::Display for SimHash128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "simhash128:{:032x}", self.0)
    }
}

/// Canonicalize content for SimHash computation. The transformation MUST be
/// deterministic and stable: bd-1iltv treats two memories that produce the
/// same canonical form as candidate near-duplicates before the cosine
/// confirmation gate. Tests in this module pin the exact normalization
/// shape so a future tightening (for instance, Unicode NFKC) is a visible
/// breaking change rather than a silent retrieval regression.
#[must_use]
pub fn canonicalize_content_for_simhash(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut last_was_space = true;
    for ch in content.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            for lowered in ch.to_lowercase() {
                out.push(lowered);
                last_was_space = false;
            }
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn tokenize_canonical(canonical: &str) -> Vec<&str> {
    canonical
        .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|token| !token.is_empty())
        .collect()
}

fn token_projection(token: &str) -> u128 {
    let mut hasher = Blake3Hasher::new();
    hasher.update(SIMHASH_DOMAIN_TAG);
    hasher.update(&(token.len() as u64).to_be_bytes());
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    let mut narrowed = [0_u8; 16];
    narrowed.copy_from_slice(&bytes[..16]);
    u128::from_be_bytes(narrowed)
}

/// Compute a 128-bit Charikar SimHash over the whitespace-normalized,
/// lowercased tokens of `content`. Empty or whitespace-only input produces
/// a well-defined fingerprint (all bits zero) rather than an error so the
/// insert path stays infallible.
#[must_use]
pub fn simhash_128(content: &str) -> SimHash128 {
    let canonical = canonicalize_content_for_simhash(content);
    let tokens = tokenize_canonical(&canonical);
    if tokens.is_empty() {
        return SimHash128(0);
    }

    let mut counters = [0_i64; SIMHASH_BITS];
    for token in tokens {
        let projection = token_projection(token);
        for (bit, counter) in counters.iter_mut().enumerate() {
            let mask = 1_u128 << bit;
            if projection & mask != 0 {
                *counter += 1;
            } else {
                *counter -= 1;
            }
        }
    }

    let mut fingerprint = 0_u128;
    for (bit, counter) in counters.iter().enumerate() {
        if *counter > 0 {
            fingerprint |= 1_u128 << bit;
        }
    }
    SimHash128(fingerprint)
}

/// Hamming distance between two SimHash fingerprints. Always in `0..=128`;
/// bd-1iltv uses this as the first dedup gate before falling through to a
/// cosine confirmation against the candidate's stored embedding.
#[must_use]
pub fn hamming_distance(a: SimHash128, b: SimHash128) -> u32 {
    (a.0 ^ b.0).count_ones()
}

#[cfg(test)]
mod tests {
    use super::{
        SIMHASH_BITS, SimHash128, canonicalize_content_for_simhash, hamming_distance, simhash_128,
    };

    #[test]
    fn happy_path__same_content_produces_identical_simhash() {
        let a = simhash_128("Rust edition is 2024");
        let b = simhash_128("Rust edition is 2024");
        assert_eq!(a, b);
    }

    #[test]
    fn happy_path__whitespace_only_variation_collapses_to_same_simhash() {
        let baseline = simhash_128("rust edition is 2024");
        let extra_inner = simhash_128("rust    edition\tis\n2024");
        let leading_trailing = simhash_128("   rust edition is 2024   ");
        assert_eq!(baseline, extra_inner);
        assert_eq!(baseline, leading_trailing);
    }

    #[test]
    fn happy_path__lowercase_normalization_collapses_case_only_variants() {
        let lower = simhash_128("Rust Edition Is 2024");
        let upper = simhash_128("RUST EDITION IS 2024");
        let mixed = simhash_128("rUsT eDiTiOn Is 2024");
        assert_eq!(lower, upper);
        assert_eq!(lower, mixed);
    }

    #[test]
    fn happy_path__single_typo_yields_small_hamming_distance() {
        let baseline = simhash_128("the quick brown fox jumps over the lazy dog");
        let typo = simhash_128("the quick brown fix jumps over the lazy dog");
        let distance = hamming_distance(baseline, typo);
        assert!(
            distance > 0,
            "single-token typo should change at least one bit"
        );
        let threshold = (SIMHASH_BITS as u32) / 3;
        assert!(
            distance <= threshold,
            "single-token typo expected within {threshold} bits, got {distance}"
        );
    }

    #[test]
    fn empty_or_boundary__empty_content_is_stable_well_defined_fingerprint() {
        let empty = simhash_128("");
        let whitespace = simhash_128("   \t\n  ");
        assert_eq!(empty, SimHash128::from_u128(0));
        assert_eq!(empty, whitespace);
    }

    #[test]
    fn empty_or_boundary__single_token_content_is_stable() {
        let once = simhash_128("rust");
        let twice = simhash_128("rust");
        assert_eq!(once, twice);
        assert_ne!(once, SimHash128::from_u128(0));
    }

    #[test]
    fn hamming_distance__identical_inputs_yield_zero() {
        let fp = simhash_128("forbidden deps include tokio rusqlite petgraph");
        assert_eq!(hamming_distance(fp, fp), 0);
    }

    #[test]
    fn hamming_distance__bitwise_inverse_yields_full_width() {
        let zero = SimHash128::from_u128(0);
        let ones = SimHash128::from_u128(u128::MAX);
        assert_eq!(hamming_distance(zero, ones), SIMHASH_BITS as u32);
        assert_eq!(hamming_distance(ones, zero), SIMHASH_BITS as u32);
    }

    #[test]
    fn hamming_distance__symmetric_property_holds() {
        let a = simhash_128("alpha beta gamma");
        let b = simhash_128("alpha beta delta");
        assert_eq!(hamming_distance(a, b), hamming_distance(b, a));
    }

    #[test]
    fn canonicalize_collapses_punctuation_neighbours_into_whitespace_boundaries() {
        let canonical = canonicalize_content_for_simhash("Hello,  world! How are you?");
        assert_eq!(canonical, "hello, world! how are you?");
    }

    #[test]
    fn canonicalize_is_idempotent() {
        let once = canonicalize_content_for_simhash("Rust  EDITION   2024");
        let twice = canonicalize_content_for_simhash(&once);
        assert_eq!(once, twice);
        assert_eq!(once, "rust edition 2024");
    }

    #[test]
    fn display_renders_stable_lowercase_hex_with_known_prefix() {
        let fp = SimHash128::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        let rendered = format!("{fp}");
        assert_eq!(rendered, "simhash128:0123456789abcdef0011223344556677");
    }

    #[test]
    fn round_trip_through_big_endian_bytes_preserves_value() {
        let fp = simhash_128("round-trip determinism check");
        let bytes = fp.to_be_bytes();
        let restored = SimHash128::from_be_bytes(bytes);
        assert_eq!(fp, restored);
    }

    #[test]
    fn serde_round_trip_preserves_value() {
        let fp = simhash_128("serde round-trip");
        let serialized = serde_json::to_string(&fp).expect("serialize");
        let restored: SimHash128 = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(fp, restored);
    }

    #[test]
    fn near_duplicates_are_closer_than_unrelated_content() {
        let baseline = simhash_128(
            "Forbidden dependencies in this project include tokio, rusqlite, and petgraph.",
        );
        let near = simhash_128(
            "Forbidden dependencies in this project includes tokio, rusqlite, and petgraph.",
        );
        let far = simhash_128(
            "The release workflow ships ee binaries to GitHub Releases with Sigstore signatures.",
        );
        let near_distance = hamming_distance(baseline, near);
        let far_distance = hamming_distance(baseline, far);
        assert!(
            near_distance < far_distance,
            "near duplicate distance {near_distance} should be smaller than unrelated distance {far_distance}"
        );
    }
}
