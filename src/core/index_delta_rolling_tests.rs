//! Rolling archives must not rebuild unchanged history just because net growth is zero.

use super::*;

const IDENTITY: &str = "rolling-intake-identity";

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
        .map(|d| (d.id.clone(), document_digest(d)))
        .collect()
}

fn inputs(documents: &[IndexableDocument]) -> BTreeMap<String, String> {
    documents
        .iter()
        .map(|d| (d.id.clone(), embedding_input_digest(d)))
        .collect()
}

fn additions(count: usize) -> Vec<IndexableDocument> {
    (0..count)
        .map(|number| {
            IndexableDocument::new(
                format!("new-{number:05}"),
                format!("fresh evidence {number}"),
            )
        })
        .collect()
}

fn retitle(documents: &mut [IndexableDocument]) {
    for document in documents {
        document.title = Some("refreshed history".to_owned());
        document
            .metadata
            .insert("revision".to_owned(), "2".to_owned());
    }
}

fn planned(original: &[IndexableDocument], current: &[IndexableDocument]) -> Delta {
    // Exercise the actual ordinary-planner-to-bulk dispatcher, not a new API
    // that publication never calls. The source snapshot remains caller-owned.
    super::super::super::diff(
        &digests(original),
        &inputs(original),
        current,
        IDENTITY.to_owned(),
    )
    .expect("admitted rolling intake")
}

fn assert_exact(original: &[IndexableDocument], current: &[IndexableDocument], delta: &Delta) {
    let live = digests(original);
    let mut reconstructed = live.clone();
    for id in &delta.removals {
        assert!(reconstructed.remove(id).is_some());
    }
    reconstructed.extend(digests(&delta.upserts));
    assert_eq!(reconstructed, digests(current));
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
    let expected_vectors: BTreeSet<_> = current
        .iter()
        .filter(|d| !live.contains_key(&d.id))
        .map(|d| d.id.as_str())
        .collect();
    let vectors = delta
        .vector_upserts(IDENTITY)
        .expect("same complete producer");
    assert_eq!(
        vectors
            .iter()
            .map(|d| d.id.as_str())
            .collect::<BTreeSet<_>>(),
        expected_vectors
    );
    assert_eq!(vectors.len(), expected_vectors.len());
    assert!(delta.removals.windows(2).all(|ids| ids[0] < ids[1]));
    assert!(delta.vector_upserts("different-producer").is_err());
}

#[test]
fn constant_size_archive_rolls_eight_thousand_new_inputs_without_rebuilding_survivors() {
    let original = documents(32_768);
    let mut current = original[8192..].to_vec();
    retitle(&mut current[..4096]);
    current.extend(additions(8192));
    current.rotate_left(12_000);
    assert_eq!(current.len(), original.len());
    let delta = planned(&original, &current);
    assert_eq!(delta.upserts.len(), 12_288);
    assert_eq!(delta.removals.len(), 8192);
    assert_exact(&original, &current, &delta);
}

#[test]
fn shrinking_archive_can_refresh_metadata_withdraw_evidence_and_ingest_in_one_delta() {
    let original = documents(20_000);
    let mut current = original[10_000..].to_vec();
    retitle(&mut current[..5000]);
    current.extend(additions(2048));
    current.reverse();
    let delta = planned(&original, &current);
    assert_eq!(delta.upserts.len(), 7048);
    assert_eq!(delta.removals.len(), 10_000);
    assert_exact(&original, &current, &delta);
}

#[test]
fn new_inputs_keep_both_the_percentage_and_absolute_inference_limits() {
    for (total, retained, limit) in [(1024, 512, 256), (40_000, 30_000, 8192)] {
        let original = documents(total);
        let live = digests(&original);
        let input = inputs(&original);
        let mut current = original[..retained].to_vec();
        current.extend(additions(limit));
        let delta = try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect("inclusive ordinary inference limit")
            .expect("non-growing intake");
        assert_exact(&original, &current, &delta);
        current.push(IndexableDocument::new("one-too-many", "one more input"));
        let error = try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect_err("withdrawals never purchase additional inference capacity");
        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
        assert!(error.detail.contains("new-input inference budget"));
    }
}

#[test]
fn new_upserts_metadata_updates_and_withdrawals_share_one_bulk_operation_budget() {
    let original = documents(append::MAX_APPEND_DOCUMENTS - 1);
    let mut current = original[..1].to_vec();
    retitle(&mut current);
    current.extend(additions(1));
    let live = digests(&original);
    let input = inputs(&original);
    let delta = try_plan(&live, &input, &current, IDENTITY.to_owned())
        .expect("exact shared mutation bound")
        .expect("rolling source");
    assert_eq!(delta.upserts.len(), 2);
    assert_eq!(delta.removals.len(), append::MAX_APPEND_DOCUMENTS - 2);
    assert_exact(&original, &current, &delta);
    current.push(IndexableDocument::new("extra-new", "extra mutation"));
    let error = try_plan(&live, &input, &current, IDENTITY.to_owned())
        .expect_err("new inputs also consume the shared bulk operation budget");
    assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
    assert!(error.detail.contains("document budget"));
}

#[test]
fn rolling_intake_charges_complete_utf8_payloads_and_withdrawn_ids_before_ownership() {
    let original = documents(4);
    let mut current = original[..1].to_vec();
    retitle(&mut current);
    current.push(IndexableDocument::new("new-資料é", "new evidence").with_title(""));
    let removed_bytes: usize = original[1..].iter().map(|d| d.id.len()).sum();
    let used = current.iter().map(document_payload_bytes).sum::<usize>() + removed_bytes;
    let remaining = super::super::super::MAX_LEXICAL_DELTA_BYTES - used;
    current[1].title = Some(format!(
        "{}{}",
        "é".repeat(remaining / 2),
        "x".repeat(remaining % 2)
    ));
    let live = digests(&original);
    let input = inputs(&original);
    let delta = try_plan(&live, &input, &current, IDENTITY.to_owned())
        .expect("exact byte boundary")
        .expect("rolling source");
    assert_exact(&original, &current, &delta);
    drop(delta);
    current[1].metadata.insert("é".to_owned(), String::new());
    let error = try_plan(&live, &input, &current, IDENTITY.to_owned())
        .expect_err("metadata cannot hide outside the payload allowance");
    assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
    assert!(error.detail.contains("byte budget"));
}

#[test]
fn late_rewritten_survivor_duplicates_and_inconsistent_input_maps_never_return_partial_intake() {
    let original = documents(32);
    let live = digests(&original);
    let input = inputs(&original);
    let mut current = additions(8);
    current.extend(original[16..].iter().cloned());
    assert!(
        try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect("baseline")
            .is_some()
    );
    let mut rewritten = current.clone();
    rewritten
        .last_mut()
        .expect("old survivor")
        .content
        .push(' ');
    assert!(
        try_plan(&live, &input, &rewritten, IDENTITY.to_owned())
            .expect("old input rewrite retains the ordinary fallback")
            .is_none()
    );
    let mut inconsistent = input.clone();
    inconsistent.insert(original[31].id.clone(), "0".repeat(64));
    assert!(
        try_plan(&live, &inconsistent, &current, IDENTITY.to_owned())
            .expect("unchanged full digest cannot hide inconsistent input")
            .is_none()
    );
    let mut incomplete = input;
    incomplete.remove(&original[0].id);
    assert_eq!(
        try_plan(&live, &incomplete, &current, IDENTITY.to_owned())
            .expect_err("complete maps before reading source")
            .reason,
        IncrementalFallbackReason::CorpusRevisionMismatch
    );
    for duplicate in [0, 23] {
        let mut duplicated = current.clone();
        duplicated.push(current[duplicate].clone());
        assert_eq!(
            try_plan(&live, &inputs(&original), &duplicated, IDENTITY.to_owned())
                .expect_err("late duplicate")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
    }
}

#[test]
fn identical_text_still_creates_every_new_vector_identity_and_rechecks_use_time_changes() {
    let original = documents(32);
    let mut current = original[16..].to_vec();
    retitle(&mut current[..2]);
    current
        .extend((0..4).map(|n| IndexableDocument::new(format!("new-{n}"), &original[0].content)));
    let mut delta = try_plan(
        &digests(&original),
        &inputs(&original),
        &current,
        IDENTITY.to_owned(),
    )
    .expect("equal text under distinct new IDs")
    .expect("rolling source");
    assert_exact(&original, &current, &delta);
    assert_eq!(delta.vector_upserts(IDENTITY).expect("fresh IDs").len(), 4);
    delta.upserts[0].content.push_str(" changed after planning");
    delta.removals.push(delta.upserts[1].id.clone());
    assert_eq!(
        delta
            .vector_upserts(IDENTITY)
            .expect("use-time recheck")
            .len(),
        6
    );
    assert!(delta.vector_upserts("changed producer").is_err());
}

#[test]
fn every_rolling_scan_and_ownership_checkpoint_can_cancel_before_a_plan_escapes() {
    let original = documents(32);
    let mut current = original[16..].to_vec();
    retitle(&mut current[..8]);
    current.extend(additions(8));
    let live = digests(&original);
    let input = inputs(&original);
    let mut total = 0;
    let delta = plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
        total += 1;
        Ok(())
    })
    .expect("count checks")
    .expect("rolling plan");
    assert_exact(&original, &current, &delta);
    for stop_at in 1..=total {
        let mut calls = 0;
        let error = plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
            calls += 1;
            if calls == stop_at {
                Err(incremental_fallback(
                    IncrementalFallbackReason::TierUnavailable,
                    "cancelled rolling intake",
                ))
            } else {
                Ok(())
            }
        })
        .expect_err("no partial delta");
        assert_eq!(calls, stop_at);
        assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
        assert_eq!(error.detail, "cancelled rolling intake");
    }
    assert_eq!((live, input), (digests(&original), inputs(&original)));
}

#[test]
fn installed_request_cancellation_prevents_rolling_fallback_and_ownership() -> Result<(), String> {
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(10), |cx| async move {
        let original = documents(10_000);
        let mut current = original[9000..].to_vec();
        current.extend(additions(1000));
        let live = digests(&original);
        let input = inputs(&original);
        cx.set_cancel_reason(asupersync::CancelReason::user(
            "cancel before rolling intake",
        ));
        let error = super::super::super::diff(&live, &input, &current, IDENTITY.to_owned())
            .expect_err("the ordinary planner cannot start a fallback after cancellation");
        assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
        assert!(cx.checkpoint().is_err());
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
