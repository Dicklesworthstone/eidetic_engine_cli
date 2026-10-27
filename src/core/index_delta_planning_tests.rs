//! Interruption must win before a delta, a threshold fallback, or ownership escapes.

use std::cell::Cell;
use std::time::Duration;

use super::super::document_digest;
use super::*;

const IDENTITY: &str = "cooperative-planner-identity";

fn documents(count: usize) -> Vec<IndexableDocument> {
    (0..count)
        .map(|number| {
            IndexableDocument::new(format!("doc-{number:05}"), format!("lesson {number}"))
        })
        .collect()
}

fn maps(documents: &[IndexableDocument]) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    (
        documents
            .iter()
            .map(|document| (document.id.clone(), document_digest(document)))
            .collect(),
        documents
            .iter()
            .map(|document| (document.id.clone(), embedding_input_digest(document)))
            .collect(),
    )
}

fn interrupted() -> IncrementalFallback {
    incremental_fallback(
        IncrementalFallbackReason::TierUnavailable,
        "cancelled source planning",
    )
}

#[test]
fn unchanged_and_empty_sources_observe_cancellation_before_hashing() {
    let original = documents(64);
    let (live, inputs) = maps(&original);
    for current in [&original[..], &[][..]] {
        let mut hashed = 0;
        let error = diff_with_checkpoint(
            &live,
            &inputs,
            current,
            IDENTITY.to_owned(),
            |document| {
                hashed += 1;
                document_digest(document)
            },
            || Err(interrupted()),
        )
        .expect_err("already interrupted");
        assert_eq!(hashed, 0);
        assert_eq!(error.detail, "cancelled source planning");
    }
}

#[test]
fn cancellation_after_digest_wins_over_the_inference_limit() {
    let original = documents(8);
    let (live, inputs) = maps(&original);
    let mut changed = original.clone();
    for document in &mut changed {
        document.content.push_str(" changed");
    }
    let hashed = Cell::new(0);
    let cancelled = Cell::new(false);
    let error = diff_with_checkpoint(
        &live,
        &inputs,
        &changed,
        IDENTITY.to_owned(),
        |document| {
            hashed.set(hashed.get() + 1);
            if hashed.get() == 3 {
                cancelled.set(true);
            }
            document_digest(document)
        },
        || {
            if cancelled.get() {
                Err(interrupted())
            } else {
                Ok(())
            }
        },
    )
    .expect_err("cancelled on the first over-limit input");
    assert_eq!(hashed.get(), 3);
    assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
    assert_eq!(error.detail, "cancelled source planning");
}

#[test]
fn unchanged_documents_and_survivor_membership_do_not_skip_checkpoints() {
    let original = documents(128);
    let (live, inputs) = maps(&original);
    let hashed = Cell::new(0);
    let membership_checks = Cell::new(0);
    let error = diff_with_checkpoint(
        &live,
        &inputs,
        &original,
        IDENTITY.to_owned(),
        |document| {
            hashed.set(hashed.get() + 1);
            document_digest(document)
        },
        || {
            if hashed.get() == original.len() {
                membership_checks.set(membership_checks.get() + 1);
                if membership_checks.get() == 64 {
                    return Err(interrupted());
                }
            }
            Ok(())
        },
    )
    .expect_err("cancel during unchanged-key membership scan");
    assert_eq!(hashed.get(), original.len());
    assert_eq!(membership_checks.get(), 64);
    assert_eq!(error.detail, "cancelled source planning");
}

#[test]
fn every_checkpoint_in_mixed_scan_and_ownership_can_cancel_without_mutation() {
    let original = documents(32);
    let (live, inputs) = maps(&original);
    let mut current = original[4..].to_vec();
    current[0].content.push_str(" repaired");
    current[1].title = Some("updated title".to_owned());
    current.push(IndexableDocument::new("new-document", "new input"));
    let mut count = 0;
    let delta = diff_with_checkpoint(
        &live,
        &inputs,
        &current,
        IDENTITY.to_owned(),
        document_digest,
        || {
            count += 1;
            Ok(())
        },
    )
    .expect("complete baseline");
    assert_eq!(delta.upserts.len(), 3);
    assert_eq!(delta.removals.len(), 4);
    assert_eq!(
        delta.vector_upserts(IDENTITY).expect("same producer").len(),
        2
    );
    let mut applied = live.clone();
    for id in &delta.removals {
        applied.remove(id);
    }
    applied.extend(maps(&delta.upserts).0);
    assert_eq!(applied, maps(&current).0);
    for stop_at in 1..=count {
        let mut calls = 0;
        let error = diff_with_checkpoint(
            &live,
            &inputs,
            &current,
            IDENTITY.to_owned(),
            document_digest,
            || {
                calls += 1;
                if calls == stop_at {
                    Err(interrupted())
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("no partial plan");
        assert_eq!(calls, stop_at);
        assert_eq!(error.detail, "cancelled source planning");
    }
    assert_eq!((live, inputs), maps(&original));
}

#[test]
fn installed_request_cancellation_propagates_through_the_real_dispatcher() -> Result<(), String> {
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        let original = documents(9000);
        let (live, inputs) = maps(&original);
        let mut current = original.clone();
        for document in &mut current {
            document.title = Some("updated".to_owned());
        }
        cx.set_cancel_reason(asupersync::CancelReason::user(
            "stop before maintenance fallback",
        ));
        let error = super::super::diff(&live, &inputs, &current, IDENTITY.to_owned())
            .expect_err("an expired ordinary pass must not start bulk maintenance");
        assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
        assert!(cx.checkpoint().is_err());
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn an_installed_request_cancelled_during_hashing_stops_before_next_document() -> Result<(), String>
{
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        let original = documents(64);
        let (live, inputs) = maps(&original);
        let mut hashed = 0;
        let error = diff_with_digest(&live, &inputs, &original, IDENTITY.to_owned(), |document| {
            hashed += 1;
            cx.set_cancel_reason(asupersync::CancelReason::user("stop hashing source"));
            document_digest(document)
        })
        .expect_err("ambient cancellation after a synchronous hash");
        assert_eq!(hashed, 1);
        assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
        assert!(cx.checkpoint().is_err());
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
