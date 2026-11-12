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

#[test]
fn ordinary_fallback_rejects_inconsistent_unchanged_input_commitments() {
    let original = documents(16);
    let live = digests(&original);
    let valid_inputs = inputs(&original);
    let mut current = original.clone();
    current[0].title = Some("metadata-only update before the corrupt row".to_owned());
    current[1].content.push_str(" changed input before the corrupt row");
    let unchanged = &original[15];

    for missing in [false, true] {
        let mut input = valid_inputs.clone();
        if missing {
            input.remove(&unchanged.id);
        } else {
            let mut conflicting = unchanged.clone();
            conflicting.content.push_str(" a different indexed input");
            input.insert(
                unchanged.id.clone(),
                embedding_input_digest(&conflicting),
            );
        }
        let before = input.clone();
        // Exercise the dispatcher as well: a proof refusal must not fall
        // through to growth/maintenance or return the already planned prefix.
        let error = super::super::diff(&live, &input, &current, IDENTITY.to_owned())
            .expect_err("inconsistent unchanged input must require a full rebuild");
        assert_eq!(
            error.reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        assert!(
            error
                .detail
                .contains("inconsistent live embedding-input digest")
        );
        assert_eq!(input, before, "planning never repairs its own proof");
        assert_eq!(live, digests(&original));
    }
}

#[test]
fn verified_unchanged_inputs_preserve_metadata_reuse_and_reembed_changed_bodies() {
    let original = documents(16);
    let live = digests(&original);
    let input = inputs(&original);
    let unchanged = super::super::diff(&live, &input, &original, IDENTITY.to_owned())
        .expect("matching document and input commitments");
    assert!(unchanged.upserts.is_empty());
    assert!(unchanged.removals.is_empty());

    let mut current = original.clone();
    current[0].title = Some("new metadata, identical embedding input".to_owned());
    let metadata_only = super::super::diff(&live, &input, &current, IDENTITY.to_owned())
        .expect("metadata reuse remains available");
    assert_eq!(metadata_only.upserts.len(), 1);
    assert!(
        metadata_only
            .vector_upserts(IDENTITY)
            .expect("matching producer")
            .is_empty()
    );

    current[1].content.push_str(" a genuinely changed body");
    let changed = super::super::diff(&live, &input, &current, IDENTITY.to_owned())
        .expect("changed bodies are not mistaken for inconsistent unchanged proofs");
    assert_eq!(changed.upserts.len(), 2);
    assert!(changed.removals.is_empty());
    let vectors = changed.vector_upserts(IDENTITY).expect("matching producer");
    assert_eq!(vectors.len(), 1);
    assert_eq!(vectors[0].id, current[1].id);
    assert_eq!(vectors[0].content, current[1].content);
    assert_eq!(input, inputs(&original));
    assert_eq!(live, digests(&original));
}
