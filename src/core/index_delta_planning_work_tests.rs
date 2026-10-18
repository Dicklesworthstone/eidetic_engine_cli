//! Source identity decides newness without hashing a new document's payload.

use super::super::document_digest;
use super::*;

const IDENTITY: &str = "identity-first-ordinary";

fn documents(count: usize) -> Vec<IndexableDocument> {
    (0..count)
        .map(|n| IndexableDocument::new(format!("doc-{n:05}"), format!("lesson {n}")))
        .collect()
}

fn digests(documents: &[IndexableDocument]) -> BTreeMap<String, String> {
    documents
        .iter()
        .map(|d| (d.id.clone(), document_digest(d)))
        .collect()
}

fn inputs(documents: &[IndexableDocument]) -> BTreeMap<String, String> {
    documents
        .iter()
        .map(|d| (d.id.clone(), embedding_input_digest(d)))
        .collect()
}

#[test]
fn new_identity_never_hashes_for_equality_or_reuses_an_unrelated_input_entry() {
    let current = vec![
        IndexableDocument::new("new-id", "資料é repeated content")
            .with_title("new searchable title"),
    ];
    // Even a stray matching input entry cannot prove a vector exists for a
    // document absent from the live membership map.
    let delta = diff_with_digest(
        &BTreeMap::new(),
        &inputs(&current),
        &current,
        IDENTITY.to_owned(),
        |_| panic!("new membership is known without hashing its body or metadata"),
    )
    .expect("one new document retains the ordinary progress allowance");
    assert_eq!(digests(&delta.upserts), digests(&current));
    assert!(delta.removals.is_empty());
    assert_eq!(delta.vector_upserts(IDENTITY).expect("new vector").len(), 1);
    assert!(delta.vector_upserts("another-producer").is_err());
}

#[test]
fn mixed_ordinary_plan_hashes_only_retained_ids_and_reconstructs_the_complete_source() {
    let original = documents(1024);
    let live = digests(&original);
    let input = inputs(&original);
    let mut current = original[16..].to_vec();
    for d in &mut current[..16] {
        d.content.push_str(" repaired");
    }
    for d in &mut current[16..32] {
        d.title = Some("updated".to_owned());
    }
    current.extend(documents(1280).into_iter().skip(1024));
    current.reverse();
    let mut hashed = BTreeSet::new();
    let delta = diff_with_digest(&live, &input, &current, IDENTITY.to_owned(), |d| {
        assert!(
            live.contains_key(&d.id),
            "new bodies do not need an equality hash"
        );
        assert!(
            hashed.insert(d.id.clone()),
            "no retained body is hashed twice"
        );
        document_digest(d)
    })
    .expect("bounded mixed source");
    assert_eq!(
        hashed,
        original[16..].iter().map(|d| d.id.clone()).collect()
    );
    assert_eq!(delta.upserts.len(), 288);
    assert_eq!(
        delta.removals,
        original[..16]
            .iter()
            .map(|d| d.id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        delta
            .vector_upserts(IDENTITY)
            .expect("input equality")
            .len(),
        272
    );
    let expected_order: Vec<_> = current
        .iter()
        .filter(|d| live.get(&d.id) != Some(&document_digest(d)))
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(
        delta
            .upserts
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
        expected_order
    );
    let mut applied = live.clone();
    for id in &delta.removals {
        assert!(applied.remove(id).is_some());
    }
    applied.extend(digests(&delta.upserts));
    assert_eq!(applied, digests(&current));
    assert_eq!((live, input), (digests(&original), inputs(&original)));
}

#[test]
fn hash_free_additions_still_enforce_inference_payload_and_duplicate_bounds() {
    let mut current = documents(2);
    let error = diff_with_digest(
        &BTreeMap::new(),
        &BTreeMap::new(),
        &current,
        IDENTITY.to_owned(),
        |_| panic!("new rows need no equality hash"),
    )
    .expect_err("two new rows exceed the empty-store ordinary inference allowance");
    assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
    assert!(error.detail.contains("inference budget"));
    current[1] = current[0].clone();
    assert_eq!(
        diff_with_digest(
            &BTreeMap::new(),
            &BTreeMap::new(),
            &current,
            IDENTITY.to_owned(),
            |_| panic!("new rows need no equality hash")
        )
        .expect_err("duplicate new identity")
        .reason,
        IncrementalFallbackReason::CorpusRevisionMismatch
    );
    current.truncate(1);
    current[0].title = Some("x".repeat(super::super::MAX_LEXICAL_DELTA_BYTES));
    let error = diff_with_digest(
        &BTreeMap::new(),
        &BTreeMap::new(),
        &current,
        IDENTITY.to_owned(),
        |_| panic!("oversize new rows need no equality hash"),
    )
    .expect_err("the ID and body put this payload above the inclusive byte ceiling");
    assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
    assert!(error.detail.contains("byte budget"));
}

#[test]
fn every_hash_free_addition_checkpoint_can_stop_before_a_delta_escapes() {
    let current = documents(1);
    let mut total = 0;
    diff_with_checkpoint(
        &BTreeMap::new(),
        &BTreeMap::new(),
        &current,
        IDENTITY.to_owned(),
        |_| panic!("new row"),
        || {
            total += 1;
            Ok(())
        },
    )
    .expect("baseline single addition");
    assert!(
        total >= 5,
        "membership, payload and ownership remain cooperative"
    );
    for stop_at in 1..=total {
        let mut calls = 0;
        let error = diff_with_checkpoint(
            &BTreeMap::new(),
            &BTreeMap::new(),
            &current,
            IDENTITY.to_owned(),
            |_| panic!("new row"),
            || {
                calls += 1;
                if calls == stop_at {
                    Err(incremental_fallback(
                        IncrementalFallbackReason::TierUnavailable,
                        "cancelled hash-free intake",
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("no partial delta");
        assert_eq!(calls, stop_at);
        assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
        assert_eq!(error.detail, "cancelled hash-free intake");
    }
}
