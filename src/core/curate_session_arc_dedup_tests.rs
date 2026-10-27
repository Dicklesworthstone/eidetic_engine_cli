#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::super::tests::{endpoints, mine, span};
use super::*;

const FAILURE: &str = "The build failed in src/cache.rs because the workspace cache retained a stale entry while the retry reused the previous identity from the original request";
const REPAIR: &str =
    "Fixed src/cache.rs by invalidating the stale entry before retrying the request.";

fn near_failure() -> String {
    FAILURE.replacen("failed in", "failed unexpectedly in", 1)
}

fn signature(failure: &str) -> Signature {
    Signature::new(failure, REPAIR).expect("bounded source fixture")
}

fn episode(id: &str, line: u32, failure: &str, repair: &str) -> Vec<ReviewSessionCandidate> {
    let rows = mine(&[
        span(&format!("{id}-failure"), line, failure),
        span(&format!("{id}-repair"), line + 1, repair),
    ]);
    assert_eq!(rows.len(), 2, "fixture must produce a complete lesson");
    rows
}

#[test]
fn repeated_failures_with_the_exact_same_repair_share_one_review_pair() {
    let near = near_failure();
    assert!(signature(&near).duplicates(&signature(FAILURE)));
    let sources = [
        span("first-failure", 1, FAILURE),
        span("first-repair", 2, REPAIR),
        span("repeat-failure", 3, &near),
        span("repeat-repair", 4, REPAIR),
    ];
    let original = sources.clone();
    let expected = mine(&sources[..2]);
    assert_eq!(expected.len(), 2);
    let rows = mine(&sources);
    assert_eq!(
        rows, expected,
        "do not merge sources or increase confidence"
    );
    assert_eq!(endpoints(&rows), [("first-failure", "first-repair")]);
    assert_eq!(sources, original, "durable source bytes remain unchanged");
    let mut reversed = sources.to_vec();
    reversed.reverse();
    assert_eq!(
        mine(&reversed),
        rows,
        "canonical order chooses the representative"
    );
}

#[test]
fn repairs_remain_distinct_even_when_symptoms_are_identical() {
    let earlier = signature(FAILURE);
    for repair in [
        "Fixed src/cache.rs by using a fresh identity before retrying the request.",
        "Fixed src/cache.rs by invalidating the stale entry after retrying the request.",
        "Fixed src/cache.rs by setting CACHE=1 before retrying the request.",
    ] {
        assert!(
            !Signature::new(FAILURE, repair)
                .unwrap()
                .duplicates(&earlier)
        );
        // Check the pair filter directly as well: the proposal builder's
        // exact-ID collision policy is independent of similarity filtering.
        let mut pairs = episode("first", 1, FAILURE, REPAIR);
        pairs.extend(episode("other", 3, FAILURE, repair));
        let expected = pairs.clone();
        assert_eq!(retain_distinct_pairs(pairs), expected);
    }
}

#[test]
fn technical_resources_literals_and_polarity_never_disappear_in_similarity() {
    let earlier = signature(FAILURE);
    assert!(!signature(&FAILURE.replace("src/cache.rs", "src/Cache.rs")).duplicates(&earlier));
    assert!(!signature(&FAILURE.replace("src/cache.rs", "src/other.rs")).duplicates(&earlier));
    for (left, right) in [
        ("code E0507", "code E0509"),
        ("count 0", "count 1"),
        ("using CacheKey", "using cacheKey"),
        ("using `CACHE`", "using `cache`"),
        ("not available", "available"),
        ("before retry", "after retry"),
        ("--strict", "--lenient"),
    ] {
        let left = signature(&format!("{FAILURE} {left}"));
        let right = signature(&format!("{FAILURE} {right}"));
        assert!(!right.duplicates(&left));
    }
    let upper = Signature::new(FAILURE, "Fixed src/cache.rs by setting CACHE=1.").unwrap();
    let lower = Signature::new(FAILURE, "Fixed src/cache.rs by setting cache=1.").unwrap();
    assert!(
        !upper.duplicates(&lower),
        "repair identity is case-sensitive"
    );
}

#[test]
fn short_unanchored_and_distant_failures_require_more_than_word_overlap() {
    assert!(
        !signature("The build failed in src/cache.rs.")
            .duplicates(&signature("The build failed unexpectedly in src/cache.rs."))
    );
    let anonymous = FAILURE.replace("src/cache.rs", "the workspace");
    let near = anonymous.replace("failed in", "failed unexpectedly in");
    assert!(!signature(&anonymous).duplicates(&signature(&near)));
    assert!(signature(&anonymous).duplicates(&signature(&anonymous)));
    assert!(!signature("The build failed in src/cache.rs when the compiler rejected an incompatible exported interface and the linker could not find the required symbol")
        .duplicates(&signature(FAILURE)));
}

fn synthetic_shingles(label: &str, start: usize, count: usize) -> Signature {
    let mut row = signature(FAILURE);
    row.failure = label.to_owned();
    row.shingles = (start..start + count)
        .map(|i| format!("word-{i} next"))
        .collect();
    row
}

#[test]
fn exact_jaccard_threshold_and_nontransitive_representatives_are_pinned() {
    let original = synthetic_shingles("original", 0, 100);
    assert!(synthetic_shingles("at-threshold", 0, 85).duplicates(&original));
    assert!(!synthetic_shingles("below-threshold", 0, 84).duplicates(&original));
    let bridge = synthetic_shingles("bridge", 5, 100);
    let distant = synthetic_shingles("distant", 10, 100);
    assert!(bridge.duplicates(&original));
    assert!(distant.duplicates(&bridge));
    assert!(!distant.duplicates(&original));
    let mut index = PairIndex::default();
    assert!(!index.is_duplicate(original));
    assert!(index.is_duplicate(bridge));
    assert!(!index.is_duplicate(distant));
    assert_eq!(index.representatives, 2);
}

#[test]
fn budget_exhaustion_retains_new_lessons_and_never_truncates_source_text() {
    assert!(Signature::new(&"x".repeat(MAX_FACET_BYTES + 1), REPAIR).is_none());
    assert!(Signature::new(FAILURE, &"x".repeat(MAX_FACET_BYTES + 1)).is_none());
    assert!(Signature::new(&"x ".repeat(MAX_SHINGLES + 2), REPAIR).is_none());
    let mut index = PairIndex::default();
    for i in 0..MAX_PER_REPAIR {
        assert!(!index.is_duplicate(signature(
            &FAILURE.replace("src/cache.rs", &format!("src/part-{i}.rs"))
        )));
    }
    assert_eq!(index.representatives, MAX_PER_REPAIR);
    assert!(!index.is_duplicate(signature(FAILURE)));
    assert!(
        !index.is_duplicate(signature(FAILURE)),
        "unindexed rows must not suppress peers"
    );
    assert_eq!(index.representatives, MAX_PER_REPAIR);
    assert!(index.is_duplicate(signature(&FAILURE.replace("src/cache.rs", "src/part-0.rs"))));
    for mut full in [
        PairIndex {
            representatives: MAX_REPRESENTATIVES,
            ..PairIndex::default()
        },
        PairIndex {
            retained_bytes: MAX_RETAINED_BYTES,
            ..PairIndex::default()
        },
    ] {
        assert!(!full.is_duplicate(signature(FAILURE)));
        assert!(full.repairs.is_empty());
    }
}

#[test]
fn filtering_moves_the_original_pair_and_preserves_incomplete_inputs() {
    let first = episode("first", 1, FAILURE, REPAIR);
    let pointers: Vec<_> = first
        .iter()
        .map(|row| (row.proposed_content.as_ptr(), row.source_ids.as_ptr()))
        .collect();
    let expected = first.clone();
    let mut input = first;
    input.extend(episode("repeat", 3, &near_failure(), REPAIR));
    let kept = retain_distinct_pairs(input);
    assert_eq!(kept, expected);
    for (row, pointers) in kept.iter().zip(pointers) {
        assert_eq!(row.proposed_content.as_ptr(), pointers.0);
        assert_eq!(row.source_ids.as_ptr(), pointers.1);
    }
    let orphan = vec![kept[0].clone()];
    assert_eq!(retain_distinct_pairs(orphan.clone()), orphan);
    let mut malformed = kept.clone();
    malformed[0].session_arc = None;
    let mut input = kept;
    input.extend(malformed.clone());
    let result = retain_distinct_pairs(input);
    assert_eq!(&result[2..], &malformed);
}
