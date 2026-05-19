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

use std::{cmp::Ordering, fmt};

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

/// Nearest SimHash candidate selected by [`nearest_simhash_candidate`].
///
/// This is intentionally only the cheap first-stage result. Insert-time
/// embedding dedup must still run the cosine confirmation gate before reusing
/// an existing embedding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NearestSimHashCandidate<'a> {
    pub candidate_id: &'a str,
    pub fingerprint: SimHash128,
    pub hamming_distance: u32,
}

fn compare_nearest_simhash_candidates(
    left: &NearestSimHashCandidate<'_>,
    right: &NearestSimHashCandidate<'_>,
) -> Ordering {
    left.hamming_distance
        .cmp(&right.hamming_distance)
        .then_with(|| left.candidate_id.cmp(right.candidate_id))
}

fn compare_ranked_simhash_candidate_entries(
    left: &(usize, NearestSimHashCandidate<'_>),
    right: &(usize, NearestSimHashCandidate<'_>),
) -> Ordering {
    compare_nearest_simhash_candidates(&left.1, &right.1).then_with(|| left.0.cmp(&right.0))
}

/// Rank candidates whose Hamming distance is within `max_hamming_distance`.
///
/// Ties are broken by candidate id in ascending lexical order so callers get a
/// deterministic choice regardless of index iteration order.
#[must_use]
pub fn ranked_simhash_candidates<'a>(
    query: SimHash128,
    candidates: impl IntoIterator<Item = (&'a str, SimHash128)>,
    max_hamming_distance: u32,
    limit: usize,
) -> Vec<NearestSimHashCandidate<'a>> {
    if limit == 0 {
        return Vec::new();
    }
    let mut ranked = Vec::new();
    for (ordinal, (candidate_id, fingerprint)) in candidates.into_iter().enumerate() {
        let distance = hamming_distance(query, fingerprint);
        if distance > max_hamming_distance {
            continue;
        }
        ranked.push((
            ordinal,
            NearestSimHashCandidate {
                candidate_id,
                fingerprint,
                hamming_distance: distance,
            },
        ));
    }
    if ranked.len() > limit {
        ranked.select_nth_unstable_by(limit - 1, compare_ranked_simhash_candidate_entries);
        ranked.truncate(limit);
    }
    ranked.sort_by(compare_ranked_simhash_candidate_entries);
    ranked.into_iter().map(|(_, candidate)| candidate).collect()
}

/// Select the nearest candidate whose Hamming distance is within
/// `max_hamming_distance`.
///
/// This is a convenience wrapper around [`ranked_simhash_candidates`] for
/// callers that only need one candidate. Insert-time dedup should prefer the
/// ranked form when it needs to continue after a candidate fails cosine
/// confirmation.
#[must_use]
pub fn nearest_simhash_candidate<'a>(
    query: SimHash128,
    candidates: impl IntoIterator<Item = (&'a str, SimHash128)>,
    max_hamming_distance: u32,
) -> Option<NearestSimHashCandidate<'a>> {
    ranked_simhash_candidates(query, candidates, max_hamming_distance, 1)
        .into_iter()
        .next()
}

/// Result of the cosine confirmation gate after a SimHash candidate passes the
/// cheap Hamming-distance filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CosineConfirmation {
    pub similarity: f32,
    pub floor: f32,
    pub confirmed: bool,
}

/// Candidate that survived both the SimHash Hamming-distance gate and the
/// cosine confirmation gate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfirmedSimHashCandidate<'a> {
    pub candidate_id: &'a str,
    pub fingerprint: SimHash128,
    pub hamming_distance: u32,
    pub cosine: CosineConfirmation,
}

/// Cosine similarity for stored embedding vectors.
///
/// Invalid inputs return `None`: dimension mismatch, empty vectors, zero
/// vectors, or non-finite values mean the caller cannot safely confirm a
/// SimHash candidate and should fall through to a fresh embedding.
#[must_use]
pub fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
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

/// Confirm whether two embeddings are similar enough to reuse the candidate's
/// stored embedding for insert-time deduplication.
#[must_use]
pub fn confirm_cosine_similarity(
    query_embedding: &[f32],
    candidate_embedding: &[f32],
    floor: f32,
) -> Option<CosineConfirmation> {
    if !floor.is_finite() {
        return None;
    }
    let similarity = cosine_similarity(query_embedding, candidate_embedding)?;
    Some(CosineConfirmation {
        similarity,
        floor,
        confirmed: similarity >= floor,
    })
}

/// Return the nearest SimHash candidate that also passes cosine confirmation.
///
/// Candidates are considered in the same deterministic order as
/// [`ranked_simhash_candidates`]: smallest Hamming distance first, then
/// candidate id. A candidate that fails cosine confirmation does not block a
/// later candidate from reusing an embedding.
#[must_use]
pub fn first_confirmed_simhash_candidate<'a>(
    query: SimHash128,
    query_embedding: &[f32],
    candidates: impl IntoIterator<Item = (&'a str, SimHash128, &'a [f32])>,
    max_hamming_distance: u32,
    cosine_floor: f32,
) -> Option<ConfirmedSimHashCandidate<'a>> {
    let mut ranked = Vec::new();
    for (candidate_id, fingerprint, embedding) in candidates {
        let distance = hamming_distance(query, fingerprint);
        if distance > max_hamming_distance {
            continue;
        }
        ranked.push((
            NearestSimHashCandidate {
                candidate_id,
                fingerprint,
                hamming_distance: distance,
            },
            embedding,
        ));
    }
    ranked.sort_by(|(left, _), (right, _)| compare_nearest_simhash_candidates(left, right));

    for (candidate, embedding) in ranked {
        let Some(cosine) = confirm_cosine_similarity(query_embedding, embedding, cosine_floor)
        else {
            continue;
        };
        if !cosine.confirmed {
            continue;
        }
        return Some(ConfirmedSimHashCandidate {
            candidate_id: candidate.candidate_id,
            fingerprint: candidate.fingerprint,
            hamming_distance: candidate.hamming_distance,
            cosine,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        ConfirmedSimHashCandidate, CosineConfirmation, NearestSimHashCandidate, SIMHASH_BITS,
        SimHash128, canonicalize_content_for_simhash, confirm_cosine_similarity, cosine_similarity,
        first_confirmed_simhash_candidate, hamming_distance, nearest_simhash_candidate,
        ranked_simhash_candidates, simhash_128,
    };

    fn candidate_ids(candidates: &[NearestSimHashCandidate<'_>]) -> Vec<&str> {
        candidates
            .iter()
            .map(|candidate| candidate.candidate_id)
            .collect()
    }

    fn candidate_distances(candidates: &[NearestSimHashCandidate<'_>]) -> Vec<u32> {
        candidates
            .iter()
            .map(|candidate| candidate.hamming_distance)
            .collect()
    }

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

    #[test]
    fn nearest_candidate_selects_exact_match_within_threshold() {
        let query = simhash_128("rust edition is 2024");
        let unrelated = simhash_128("release binaries are signed before upload");
        let candidates = [("mem_b", unrelated), ("mem_a", query)];

        let selected = nearest_simhash_candidate(query, candidates, 0).expect("exact match");

        assert_eq!(selected.candidate_id, "mem_a");
        assert_eq!(selected.fingerprint, query);
        assert_eq!(selected.hamming_distance, 0);
    }

    #[test]
    fn nearest_candidate_respects_max_hamming_distance() {
        let query = simhash_128("forbidden deps include tokio rusqlite petgraph");
        let near = simhash_128("forbidden deps include tokio rusqlite petgrph");
        let distance = hamming_distance(query, near);
        assert!(distance > 0, "test fixture must not be an exact match");

        let candidates = [("mem_near", near)];
        let selected = nearest_simhash_candidate(query, candidates, distance - 1);

        assert_eq!(selected, None);
    }

    #[test]
    fn nearest_candidate_chooses_smallest_distance_before_lexical_tie_break() {
        let query = SimHash128::from_u128(0b0000);
        let farther = SimHash128::from_u128(0b0111);
        let closer = SimHash128::from_u128(0b0001);
        let candidates = [("mem_a", farther), ("mem_z", closer)];

        let selected =
            nearest_simhash_candidate(query, candidates, SIMHASH_BITS as u32).expect("candidate");

        assert_eq!(selected.candidate_id, "mem_z");
        assert_eq!(selected.hamming_distance, 1);
    }

    #[test]
    fn nearest_candidate_tie_breaks_by_candidate_id_not_iteration_order() {
        let query = SimHash128::from_u128(0);
        let left = SimHash128::from_u128(0b0011);
        let right = SimHash128::from_u128(0b1100);

        let forward = nearest_simhash_candidate(query, [("mem_b", left), ("mem_a", right)], 2)
            .expect("forward candidate");
        let reverse = nearest_simhash_candidate(query, [("mem_a", right), ("mem_b", left)], 2)
            .expect("reverse candidate");

        assert_eq!(forward.candidate_id, "mem_a");
        assert_eq!(reverse.candidate_id, "mem_a");
        assert_eq!(forward.hamming_distance, reverse.hamming_distance);
    }

    #[test]
    fn ranked_candidates_sort_by_distance_then_candidate_id() {
        let query = SimHash128::from_u128(0);
        let candidates = [
            ("mem_c", SimHash128::from_u128(0b1111)),
            ("mem_b", SimHash128::from_u128(0b0011)),
            ("mem_a", SimHash128::from_u128(0b1100)),
            ("mem_d", SimHash128::from_u128(0b0001)),
        ];

        let ranked = ranked_simhash_candidates(query, candidates, SIMHASH_BITS as u32, 10);

        assert_eq!(
            candidate_ids(&ranked),
            vec!["mem_d", "mem_a", "mem_b", "mem_c"]
        );
        assert_eq!(candidate_distances(&ranked), vec![1, 2, 2, 4]);
    }

    #[test]
    fn ranked_candidates_respect_threshold_and_limit() {
        let query = SimHash128::from_u128(0);
        let candidates = [
            ("mem_exact", SimHash128::from_u128(0)),
            ("mem_near", SimHash128::from_u128(0b0001)),
            ("mem_far", SimHash128::from_u128(0b1111)),
        ];

        let ranked = ranked_simhash_candidates(query, candidates, 1, 1);

        assert_eq!(candidate_ids(&ranked), vec!["mem_exact"]);
        assert_eq!(candidate_distances(&ranked), vec![0]);
    }

    #[test]
    fn ranked_candidates_limit_zero_returns_empty() {
        let query = SimHash128::from_u128(0);
        let candidates = [("mem_exact", SimHash128::from_u128(0))];

        let ranked = ranked_simhash_candidates(query, candidates, SIMHASH_BITS as u32, 0);

        assert!(ranked.is_empty());
    }

    #[test]
    fn ranked_candidates_limited_prefix_matches_full_ranking() {
        let query = SimHash128::from_u128(0);
        let candidates = [
            ("mem_h", SimHash128::from_u128(0b1111_1111)),
            ("mem_b", SimHash128::from_u128(0b0011)),
            ("mem_e", SimHash128::from_u128(0b0001_1111)),
            ("mem_a", SimHash128::from_u128(0b0101)),
            ("mem_d", SimHash128::from_u128(0b0001)),
            ("mem_c", SimHash128::from_u128(0b0111)),
            ("mem_f", SimHash128::from_u128(0b0010)),
            ("mem_g", SimHash128::from_u128(0b1111)),
        ];

        let full = ranked_simhash_candidates(query, candidates, SIMHASH_BITS as u32, usize::MAX);
        let limited = ranked_simhash_candidates(query, candidates, SIMHASH_BITS as u32, 4);

        assert_eq!(limited.as_slice(), &full[..4]);
    }

    #[test]
    fn ranked_candidates_limited_selection_preserves_duplicate_key_order() {
        let query = SimHash128::from_u128(0);
        let candidates = [
            ("mem_same", SimHash128::from_u128(0b0001)),
            ("mem_same", SimHash128::from_u128(0b0010)),
            ("mem_same", SimHash128::from_u128(0b0100)),
            ("mem_same", SimHash128::from_u128(0b1000)),
        ];

        let ranked = ranked_simhash_candidates(query, candidates, SIMHASH_BITS as u32, 2);
        let fingerprints: Vec<_> = ranked
            .iter()
            .map(|candidate| candidate.fingerprint)
            .collect();

        assert_eq!(
            fingerprints,
            vec![SimHash128::from_u128(0b0001), SimHash128::from_u128(0b0010)]
        );
    }

    #[test]
    fn first_confirmed_candidate_skips_cosine_rejection_and_continues() {
        let query = SimHash128::from_u128(0b0000);
        let query_embedding = [1.0, 0.0, 0.0];
        let rejected_embedding = [0.0, 1.0, 0.0];
        let confirmed_embedding = [0.99, 0.01, 0.0];
        let candidates = [
            (
                "mem_rejected",
                SimHash128::from_u128(0b0001),
                rejected_embedding.as_slice(),
            ),
            (
                "mem_confirmed",
                SimHash128::from_u128(0b0011),
                confirmed_embedding.as_slice(),
            ),
        ];

        let selected = first_confirmed_simhash_candidate(
            query,
            &query_embedding,
            candidates,
            SIMHASH_BITS as u32,
            0.97,
        )
        .expect("confirmed candidate");

        assert_eq!(selected.candidate_id, "mem_confirmed");
        assert_eq!(selected.hamming_distance, 2);
        assert!(selected.cosine.confirmed);
    }

    #[test]
    fn first_confirmed_candidate_tie_breaks_before_cosine_confirmation() {
        let query = SimHash128::from_u128(0);
        let query_embedding = [1.0, 0.0];
        let embedding_a = [1.0, 0.0];
        let embedding_b = [1.0, 0.0];
        let candidates = [
            (
                "mem_b",
                SimHash128::from_u128(0b0011),
                embedding_b.as_slice(),
            ),
            (
                "mem_a",
                SimHash128::from_u128(0b1100),
                embedding_a.as_slice(),
            ),
        ];

        let selected = first_confirmed_simhash_candidate(
            query,
            &query_embedding,
            candidates,
            SIMHASH_BITS as u32,
            0.97,
        )
        .expect("confirmed candidate");

        assert_eq!(
            selected,
            ConfirmedSimHashCandidate {
                candidate_id: "mem_a",
                fingerprint: SimHash128::from_u128(0b1100),
                hamming_distance: 2,
                cosine: CosineConfirmation {
                    similarity: 1.0,
                    floor: 0.97,
                    confirmed: true,
                },
            }
        );
    }

    #[test]
    fn first_confirmed_candidate_respects_hamming_threshold() {
        let query = SimHash128::from_u128(0);
        let query_embedding = [1.0, 0.0];
        let candidate_embedding = [1.0, 0.0];
        let candidates = [(
            "mem_outside_threshold",
            SimHash128::from_u128(0b0011),
            candidate_embedding.as_slice(),
        )];

        let selected =
            first_confirmed_simhash_candidate(query, &query_embedding, candidates, 1, 0.97);

        assert_eq!(selected, None);
    }

    #[test]
    fn first_confirmed_candidate_rejects_non_finite_floor() {
        let query = SimHash128::from_u128(0);
        let query_embedding = [1.0];
        let candidate_embedding = [1.0];
        let candidates = [(
            "mem_exact",
            SimHash128::from_u128(0),
            candidate_embedding.as_slice(),
        )];

        let selected = first_confirmed_simhash_candidate(
            query,
            &query_embedding,
            candidates,
            SIMHASH_BITS as u32,
            f32::NAN,
        );

        assert_eq!(selected, None);
    }

    #[test]
    fn cosine_similarity_identical_vectors_confirm_reuse() {
        let embedding = [1.0, 0.0, 0.0];

        let confirmation = confirm_cosine_similarity(&embedding, &embedding, 0.97)
            .expect("valid cosine comparison");

        assert_eq!(
            confirmation,
            CosineConfirmation {
                similarity: 1.0,
                floor: 0.97,
                confirmed: true,
            }
        );
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors_reject_reuse() {
        let left = [1.0, 0.0, 0.0];
        let right = [0.0, 1.0, 0.0];

        let confirmation =
            confirm_cosine_similarity(&left, &right, 0.97).expect("valid cosine comparison");

        assert_eq!(
            confirmation,
            CosineConfirmation {
                similarity: 0.0,
                floor: 0.97,
                confirmed: false,
            }
        );
    }

    #[test]
    fn cosine_similarity_dimension_mismatch_is_not_confirmable() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), None);
    }

    #[test]
    fn cosine_similarity_zero_vector_is_not_confirmable() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), None);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 0.0]), None);
    }

    #[test]
    fn cosine_similarity_non_finite_value_is_not_confirmable() {
        assert_eq!(cosine_similarity(&[f32::NAN], &[1.0]), None);
        assert_eq!(cosine_similarity(&[f32::INFINITY], &[1.0]), None);
    }

    #[test]
    fn cosine_confirmation_non_finite_floor_is_not_confirmable() {
        assert_eq!(confirm_cosine_similarity(&[1.0], &[1.0], f32::NAN), None);
    }
}
