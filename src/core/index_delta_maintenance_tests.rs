//! Exact input reuse with bounded mixed maintenance inference.

use super::*;

const IDENTITY: &str = "maintenance-fixture-identity";

fn documents(count: usize) -> Vec<IndexableDocument> {
    (0..count)
        .map(|number| {
            IndexableDocument::new(format!("doc-{number:05}"), format!("lesson {number}"))
        })
        .collect()
}

fn digests(documents: &[IndexableDocument]) -> BTreeMap<String, String> {
    documents
        .iter()
        .map(|document| (document.id.clone(), document_digest(document)))
        .collect()
}

fn inputs(documents: &[IndexableDocument]) -> BTreeMap<String, String> {
    documents
        .iter()
        .map(|document| (document.id.clone(), embedding_input_digest(document)))
        .collect()
}

fn retitle(documents: &mut [IndexableDocument]) {
    for document in documents {
        document.title = Some("refreshed title".to_owned());
        document
            .metadata
            .insert("revision".to_owned(), "2".to_owned());
    }
}

fn planned(original: &[IndexableDocument], current: &[IndexableDocument]) -> Delta {
    // Use the actual V3 dispatcher, including the ordinary threshold refusal.
    super::super::super::diff(
        &digests(original),
        &inputs(original),
        current,
        IDENTITY.to_owned(),
    )
    .expect("bounded maintenance")
}

fn assert_reconstructs(
    original: &[IndexableDocument],
    current: &[IndexableDocument],
    delta: &Delta,
) {
    let mut applied = digests(original);
    for id in &delta.removals {
        assert!(applied.remove(id).is_some());
    }
    applied.extend(digests(&delta.upserts));
    assert_eq!(applied, digests(current));
    assert!(
        delta
            .vector_upserts(IDENTITY)
            .expect("same producer")
            .is_empty()
    );
}

#[test]
fn fifty_thousand_metadata_updates_do_not_reinfer_the_corpus() {
    let original = documents(50_000);
    let mut current = original.clone();
    retitle(&mut current);
    let delta = planned(&original, &current);
    assert_eq!(delta.upserts.len(), 50_000);
    assert!(delta.removals.is_empty());
    assert_reconstructs(&original, &current, &delta);
}

#[test]
fn shrinking_archive_keeps_all_metadata_changes_and_withdrawals() {
    let original = documents(30_000);
    let mut current = original[10_000..].to_vec();
    retitle(&mut current[..10_000]);
    current.reverse();
    let delta = planned(&original, &current);
    assert_eq!(delta.upserts.len(), 10_000);
    assert_eq!(delta.removals.len(), 10_000);
    assert_eq!(
        delta
            .upserts
            .iter()
            .map(|document| &document.id)
            .collect::<Vec<_>>(),
        current[10_000..]
            .iter()
            .map(|document| &document.id)
            .collect::<Vec<_>>()
    );
    assert_reconstructs(&original, &current, &delta);
}

#[test]
fn large_deletion_only_and_empty_final_stores_need_no_inference() {
    let original = documents(10_000);
    for current in [&original[..1], &[][..]] {
        let delta = planned(&original, current);
        assert!(delta.upserts.is_empty());
        assert_eq!(delta.removals.len(), original.len() - current.len());
        assert_reconstructs(&original, current, &delta);
    }
}

#[test]
fn survivor_rewrites_and_new_identities_receive_fresh_vectors_without_reinferring_metadata() {
    let original = documents(10_000);
    for new_identity in [false, true] {
        let mut current = original[..9000].to_vec();
        retitle(&mut current);
        if new_identity {
            current[8999].id = "new-document".to_owned();
        } else {
            current[8999].content.push(' ');
        }
        let ordinary = super::super::super::planning::diff_with_digest(
            &digests(&original),
            &inputs(&original),
            &current,
            IDENTITY.to_owned(),
            document_digest,
        )
        .expect_err("ordinary mixed allowance is unchanged");
        assert_eq!(
            ordinary.reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        let delta = planned(&original, &current);
        assert_eq!(delta.upserts.len(), 9000);
        assert_eq!(delta.removals.len(), 1000 + usize::from(new_identity));
        let vectors = delta.vector_upserts(IDENTITY).expect("same producer");
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].id, current[8999].id);
        assert_eq!(vectors[0].content, current[8999].content);
        let mut applied = digests(&original);
        for id in &delta.removals {
            assert!(applied.remove(id).is_some());
        }
        applied.extend(digests(&delta.upserts));
        assert_eq!(applied, digests(&current));
    }
}

#[test]
fn duplicate_and_incomplete_or_inconsistent_identity_maps_cannot_authorize_reuse() {
    let original = documents(32);
    let live = digests(&original);
    let input = inputs(&original);
    let mut current = original[..16].to_vec();
    current.push(current[15].clone());
    assert_eq!(
        try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect_err("late duplicate")
            .reason,
        IncrementalFallbackReason::CorpusRevisionMismatch
    );
    for foreign_key in [false, true] {
        let mut incomplete = input.clone();
        let value = incomplete.remove(&original[0].id).expect("input");
        if foreign_key {
            incomplete.insert("foreign-id".to_owned(), value);
        }
        assert_eq!(
            try_plan(&live, &incomplete, &original, IDENTITY.to_owned())
                .expect_err("complete map")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
    }
    let mut wrong = input;
    wrong.insert(original[0].id.clone(), "0".repeat(64));
    // Even an unchanged full-document digest must not hide a wrong input map.
    assert!(
        try_plan(&live, &wrong, &original, IDENTITY.to_owned())
            .expect("input mismatch")
            .is_none()
    );
}

#[test]
fn one_shared_operation_bound_counts_both_upserts_and_removals() {
    let original = documents(append::MAX_APPEND_DOCUMENTS + 1);
    let mut current = original[..1].to_vec();
    retitle(&mut current);
    let accepted = try_plan(
        &digests(&original[..append::MAX_APPEND_DOCUMENTS]),
        &inputs(&original[..append::MAX_APPEND_DOCUMENTS]),
        &current,
        IDENTITY.to_owned(),
    )
    .expect("exact bulk mutation bound")
    .expect("maintenance");
    assert_eq!(accepted.upserts.len(), 1);
    assert_eq!(accepted.removals.len(), append::MAX_APPEND_DOCUMENTS - 1);
    assert!(
        try_plan(
            &digests(&original),
            &inputs(&original),
            &current,
            IDENTITY.to_owned()
        )
        .is_err()
    );
    assert!(
        try_plan(
            &digests(&original),
            &inputs(&original),
            &[],
            IDENTITY.to_owned()
        )
        .is_err()
    );
}

#[test]
fn byte_budget_counts_unicode_metadata_titles_and_withdrawn_ids() {
    let original = documents(2);
    let mut current = original[..1].to_vec();
    current[0]
        .metadata
        .insert("clé".to_owned(), "資料".to_owned());
    current[0].title = Some(String::new());
    let bytes = super::super::super::MAX_LEXICAL_DELTA_BYTES;
    let available = bytes - document_payload_bytes(&current[0]) - original[1].id.len();
    current[0].title = Some(format!(
        "{}{}",
        "é".repeat(available / 2),
        "x".repeat(available % 2)
    ));
    let live = digests(&original);
    let input = inputs(&original);
    let delta = try_plan(&live, &input, &current, IDENTITY.to_owned())
        .expect("exact byte bound")
        .expect("maintenance");
    assert_eq!(delta.upserts.len(), 1);
    assert_eq!(delta.removals.len(), 1);
    assert_reconstructs(&original, &current, &delta);
    drop(delta);
    current[0].metadata.insert("x".to_owned(), String::new());
    assert!(try_plan(&live, &input, &current, IDENTITY.to_owned()).is_err());
}

#[test]
fn unchanged_payload_is_not_charged_as_a_mutation_and_source_order_is_irrelevant() {
    let mut original = documents(32);
    original[0].title = Some("x".repeat(super::super::super::MAX_LEXICAL_DELTA_BYTES));
    let mut current = original[..16].to_vec();
    retitle(&mut current[1..]);
    current.reverse();
    let delta = try_plan(
        &digests(&original),
        &inputs(&original),
        &current,
        IDENTITY.to_owned(),
    )
    .expect("unchanged large payload is borrowed")
    .expect("maintenance");
    assert_eq!(delta.upserts.len(), 15);
    assert_eq!(delta.removals.len(), 16);
    assert_reconstructs(&original, &current, &delta);
}

#[test]
fn use_time_checks_still_require_the_original_producer_and_each_exact_input() {
    let original = documents(32);
    let mut current = original[..16].to_vec();
    retitle(&mut current);
    let mut delta = try_plan(
        &digests(&original),
        &inputs(&original),
        &current,
        IDENTITY.to_owned(),
    )
    .expect("admitted")
    .expect("maintenance");
    assert!(delta.vector_upserts("changed producer").is_err());
    assert!(
        delta
            .vector_upserts(IDENTITY)
            .expect("exact inputs")
            .is_empty()
    );
    delta.upserts[0].content.push_str(" changed after planning");
    delta.removals.push(delta.upserts[1].id.clone());
    assert_eq!(
        delta
            .vector_upserts(IDENTITY)
            .expect("use-time changes")
            .len(),
        2
    );
}

#[test]
fn every_scan_measurement_and_ownership_checkpoint_can_cancel_without_a_partial_delta() {
    let original = documents(32);
    let mut current = original[..16].to_vec();
    retitle(&mut current[..8]);
    let live = digests(&original);
    let input = inputs(&original);
    let mut total = 0;
    assert!(
        plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
            total += 1;
            Ok(())
        })
        .expect("count checkpoints")
        .is_some()
    );
    for stop_at in 1..=total {
        let mut calls = 0;
        let error = plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
            calls += 1;
            if calls == stop_at {
                Err(incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    "cancelled maintenance planning",
                ))
            } else {
                Ok(())
            }
        })
        .expect_err("cancelled before a delta escapes");
        assert_eq!(calls, stop_at);
        assert_eq!(error.detail, "cancelled maintenance planning");
    }
    assert_eq!(live, digests(&original));
    assert_eq!(input, inputs(&original));
}
