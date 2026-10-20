//! Non-growing mixed updates keep inference proportional to changed inputs.

use super::*;

const IDENTITY: &str = "mixed-maintenance-producer";

fn documents(count: usize) -> Vec<IndexableDocument> {
    (0..count)
        .map(|n| IndexableDocument::new(format!("doc-{n:05}"), format!("original lesson {n}")))
        .collect()
}

fn maps(documents: &[IndexableDocument]) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    (
        documents
            .iter()
            .map(|d| (d.id.clone(), document_digest(d)))
            .collect(),
        documents
            .iter()
            .map(|d| (d.id.clone(), embedding_input_digest(d)))
            .collect(),
    )
}

fn assert_exact(original: &[IndexableDocument], current: &[IndexableDocument], delta: &Delta) {
    let (live, input) = maps(original);
    let mut rebuilt = live.clone();
    for id in &delta.removals {
        assert!(rebuilt.remove(id).is_some());
    }
    rebuilt.extend(maps(&delta.upserts).0);
    assert_eq!(rebuilt, maps(current).0);
    let expected_upserts: Vec<_> = current
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
        expected_upserts
    );
    let expected_vectors: Vec<_> = current
        .iter()
        .filter(|d| input.get(&d.id) != Some(&embedding_input_digest(d)))
        .map(|d| (d.id.as_str(), d.content.as_str()))
        .collect();
    let vectors = delta.vector_upserts(IDENTITY).expect("same producer");
    assert_eq!(
        vectors
            .iter()
            .map(|d| (d.id.as_str(), d.content.as_str()))
            .collect::<Vec<_>>(),
        expected_vectors
    );
    assert!(delta.removals.windows(2).all(|ids| ids[0] < ids[1]));
    assert!(delta.vector_upserts("another producer").is_err());
}

#[test]
fn large_metadata_refresh_with_edits_and_withdrawals_uses_the_live_dispatcher() {
    let original = documents(10_000);
    let mut current = original[1000..].to_vec();
    for d in &mut current {
        d.title = Some("new title".to_owned());
        d.metadata.insert("revision".to_owned(), "2".to_owned());
    }
    for d in &mut current[..16] {
        d.content.push_str(" corrected body");
    }
    for n in 0..10 {
        current.push(IndexableDocument::new(format!("new-{n}"), "new session"));
    }
    current.rotate_left(4000);
    let (live, input) = maps(&original);
    let ordinary = super::super::super::planning::diff_with_digest(
        &live,
        &input,
        &current,
        IDENTITY.to_owned(),
        document_digest,
    )
    .expect_err("more than the unchanged ordinary mutation limit");
    assert_eq!(
        ordinary.reason,
        IncrementalFallbackReason::DeltaOverThreshold
    );
    let delta = super::super::super::diff(&live, &input, &current, IDENTITY.to_owned())
        .expect("mixed maintenance must not rebuild unchanged history");
    assert_eq!(delta.upserts.len(), 9010);
    assert_eq!(delta.removals.len(), 1000);
    assert_eq!(delta.vector_upserts(IDENTITY).expect("vectors").len(), 26);
    assert_exact(&original, &current, &delta);
}

#[test]
fn edits_and_new_identities_share_percentage_and_absolute_inference_bounds() {
    for (size, kept, limit) in [(1024, 512, 256), (40_000, 30_000, 8192)] {
        let original = documents(size);
        let (live, input) = maps(&original);
        let rewrites = limit / 2;
        let mut current = original[..kept].to_vec();
        for d in &mut current[..rewrites] {
            d.content.push_str(" corrected");
        }
        current.extend(
            (0..limit - rewrites).map(|n| IndexableDocument::new(format!("new-{n}"), "new input")),
        );
        let delta = try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect("inclusive combined inference budget")
            .expect("non-growing source");
        assert_eq!(
            delta.vector_upserts(IDENTITY).expect("vectors").len(),
            limit
        );
        assert_exact(&original, &current, &delta);
        current[rewrites]
            .content
            .push_str(" one additional rewrite");
        let error = try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect_err("withdrawals cannot buy extra inference for old IDs");
        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
        assert!(error.detail.contains("inference budget"));
        current.reverse();
        assert!(try_plan(&live, &input, &current, IDENTITY.to_owned()).is_err());
    }
}

#[test]
fn same_text_with_a_new_id_is_not_reused_and_identical_maps_cannot_hide_a_rewrite() {
    let original = documents(32);
    let (live, input) = maps(&original);
    let mut current = original[8..].to_vec();
    current[0].content = original[0].content.clone();
    current[1].title = Some("metadata only".to_owned());
    current.push(IndexableDocument::new("new-copy", &original[0].content));
    let mut delta = try_plan(&live, &input, &current, IDENTITY.to_owned())
        .expect("two fresh inputs")
        .expect("maintenance");
    assert_eq!(delta.vector_upserts(IDENTITY).expect("vectors").len(), 2);
    assert_exact(&original, &current, &delta);
    let reused = delta
        .upserts
        .iter_mut()
        .find(|d| d.id == original[9].id)
        .expect("reused");
    reused.content.push_str(" changed after planning");
    assert_eq!(
        delta
            .vector_upserts(IDENTITY)
            .expect("live input recheck")
            .len(),
        3
    );
    let mut inconsistent = input.clone();
    inconsistent.insert(original[31].id.clone(), "0".repeat(64));
    assert!(
        try_plan(&live, &inconsistent, &current, IDENTITY.to_owned())
            .expect("a supposedly unchanged full digest disagrees with its input")
            .is_none()
    );
    let mut duplicate = current.clone();
    duplicate.push(current[0].clone());
    assert_eq!(
        try_plan(&live, &input, &duplicate, IDENTITY.to_owned())
            .expect_err("duplicate late in mixed source")
            .reason,
        IncrementalFallbackReason::CorpusRevisionMismatch
    );
}

#[test]
fn every_mixed_planning_checkpoint_can_cancel_without_publishing_a_partial_plan() {
    let original = documents(64);
    let (live, input) = maps(&original);
    let mut current = original[16..].to_vec();
    for d in &mut current[..8] {
        d.content.push_str(" corrected");
    }
    for d in &mut current[8..16] {
        d.title = Some("metadata only".to_owned());
    }
    current.extend((0..8).map(|n| IndexableDocument::new(format!("new-{n}"), "new input")));
    let mut total = 0;
    let delta = plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
        total += 1;
        Ok(())
    })
    .expect("count checks")
    .expect("mixed plan");
    assert_exact(&original, &current, &delta);
    for stop in 1..=total {
        let mut calls = 0;
        let error = plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
            calls += 1;
            if calls == stop {
                Err(incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    "cancelled mixed maintenance",
                ))
            } else {
                Ok(())
            }
        })
        .expect_err("no partially planned delta escapes cancellation");
        assert_eq!(calls, stop);
        assert_eq!(error.detail, "cancelled mixed maintenance");
    }
    assert_eq!((live, input), maps(&original));
}

#[test]
fn mixed_updates_charge_rewritten_utf8_payloads_and_removal_ids_before_cloning() {
    let original = documents(4);
    let (live, input) = maps(&original);
    let mut current = original[..1].to_vec();
    current[0].content = "é".to_owned();
    current[0].title = Some(String::new());
    let used = document_payload_bytes(&current[0])
        + original[1..].iter().map(|d| d.id.len()).sum::<usize>();
    let remaining = super::super::super::MAX_LEXICAL_DELTA_BYTES - used;
    current[0].title = Some(format!(
        "{}{}",
        "é".repeat(remaining / 2),
        "x".repeat(remaining % 2)
    ));
    let delta = try_plan(&live, &input, &current, IDENTITY.to_owned())
        .expect("inclusive complete payload limit")
        .expect("mixed plan");
    assert_exact(&original, &current, &delta);
    drop(delta);
    current[0].content.push('x');
    assert_eq!(
        try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect_err("changed content bytes count too")
            .reason,
        IncrementalFallbackReason::DeltaOverThreshold
    );
}
