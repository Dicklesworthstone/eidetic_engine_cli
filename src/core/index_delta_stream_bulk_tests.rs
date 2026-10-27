//! Differential bulk streaming tests against the actual complete V3 planner.

use super::tests::{TestResult, documents, generation, reconcile, same_as_full_plan, test_stack};
use super::*;

#[test]
fn ten_thousand_document_mixed_maintenance_retains_only_changed_bodies() -> TestResult {
    let stack = test_stack(11);
    let original = documents(10_000);
    let root = generation(&stack, &original)?;
    let mut current = original[1000..].to_vec();
    for document in &mut current[..8500] {
        document.title = Some("updated metadata".to_owned());
    }
    for document in &mut current[..16] {
        document.content.push_str(" actual body correction");
    }
    current.extend((0..10).map(|n| IndexableDocument::new(format!("new-{n}"), "new evidence")));
    current.rotate_left(4000);
    same_as_full_plan(root.path(), &stack, &current)?;
    let mut stream = SourceReconciler::read(root.path(), &stack).map_err(|e| e.detail)?;
    for document in &current {
        stream.observe(document.clone()).map_err(|e| e.detail)?;
    }
    assert_eq!(stream.upserts.len(), 8510);
    assert_eq!(stream.inference_inputs, 26);
    assert_eq!(stream.new_identities, 10);
    assert_eq!(stream.reusable_inputs.len(), 8484);
    assert_eq!(stream.live_documents.len(), 1000);
    let result = stream.finish().map_err(|e| e.detail)?;
    assert_eq!(result.documents_total, current.len());
    Ok(())
}

#[test]
fn bulk_appends_are_streamed_without_reinferring_old_history() -> TestResult {
    let stack = test_stack(11);
    let original = documents(1024);
    let root = generation(&stack, &original)?;
    let mut current = original[16..].to_vec();
    for document in &mut current[..16] {
        document.content.push_str(" corrected");
    }
    for document in &mut current[16..48] {
        document
            .metadata
            .insert("revision".to_owned(), "2".to_owned());
    }
    current.extend((0..4096).map(|n| IndexableDocument::new(format!("new-{n}"), "new evidence")));
    // New documents appear before the survivor rows: intermediate size must
    // not decide whether this is growth, or prematurely consume a small cap.
    current.rotate_right(4096);
    same_as_full_plan(root.path(), &stack, &current)?;
    let actual = reconcile(root.path(), &stack, &current).map_err(|e| e.detail)?;
    assert_eq!(actual.delta.upserts.len(), 4144);
    let identity = embedder_identity(&stack).ok_or("producer")?;
    assert_eq!(
        actual
            .delta
            .vector_upserts(&identity)
            .map_err(|e| e.detail)?
            .len(),
        4112
    );
    Ok(())
}

#[test]
fn bulk_operation_and_inference_boundaries_agree_with_the_full_planner() -> TestResult {
    let stack = test_stack(11);
    let original = documents(budget::MAX_MUTATIONS + 1);
    let root = generation(&stack, &original)?;
    // Upserts plus removals share the ceiling, including complete withdrawal.
    same_as_full_plan(root.path(), &stack, &original[..1])?;
    assert!(reconcile(root.path(), &stack, &[]).is_err());
    assert!(super::super::plan(root.path(), &stack, &[]).is_err());
    let mut current = original.clone();
    for document in &mut current[..budget::MAX_MUTATIONS] {
        document.title = Some("metadata".to_owned());
    }
    same_as_full_plan(root.path(), &stack, &current)?;
    current.last_mut().ok_or("last")?.title = Some("one too many".to_owned());
    assert!(reconcile(root.path(), &stack, &current).is_err());
    assert!(super::super::plan(root.path(), &stack, &current).is_err());
    let original = documents(1024);
    let root = generation(&stack, &original)?;
    let mut current = original[512..].to_vec();
    for document in &mut current[..128] {
        document.content.push_str(" edited");
    }
    current.extend((0..128).map(|n| IndexableDocument::new(format!("new-{n}"), "new")));
    same_as_full_plan(root.path(), &stack, &current)?;
    current[128].content.push_str(" extra inference");
    assert!(reconcile(root.path(), &stack, &current).is_err());
    assert!(super::super::plan(root.path(), &stack, &current).is_err());
    Ok(())
}

#[test]
fn refused_bulk_prefix_cannot_be_salvaged_after_duplicate_or_cancellation() -> TestResult {
    let stack = test_stack(11);
    let original = documents(1024);
    let root = generation(&stack, &original)?;
    for cancel in [false, true] {
        let mut stream = SourceReconciler::read(root.path(), &stack).map_err(|e| e.detail)?;
        for original in &original[..512] {
            let mut document = original.clone();
            document.title = Some("metadata update".to_owned());
            stream.observe(document).map_err(|e| e.detail)?;
        }
        assert_eq!(stream.upserts.len(), 512);
        if cancel {
            let cx = asupersync::Cx::for_testing();
            cx.set_cancel_reason(asupersync::CancelReason::user("cancel bulk source"));
            stream.request = Some(cx);
        }
        // Exercise cancellation on a fresh row so duplicate detection cannot
        // accidentally satisfy the cancellation assertion.
        let next = if cancel { 512 } else { 0 };
        let error = stream
            .observe(original[next].clone())
            .expect_err("refused source");
        assert_eq!(
            error.reason,
            if cancel {
                IncrementalFallbackReason::TierUnavailable
            } else {
                IncrementalFallbackReason::CorpusRevisionMismatch
            },
        );
        assert!(stream.upserts.is_empty());
        assert!(stream.reusable_inputs.is_empty());
        assert!(stream.observe(original[512].clone()).is_err());
        assert!(stream.finish().is_err());
    }
    Ok(())
}

#[test]
fn deterministic_mixed_corpora_match_eager_acceptance_and_every_output_digest() -> TestResult {
    let stack = test_stack(11);
    let original = documents(64);
    let root = generation(&stack, &original)?;
    let mut random = 1729_u64;
    let mut next = || {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        random >> 32
    };
    let mut accepted = 0;
    let mut refused = 0;
    for case in 0..128 {
        let mut current = Vec::new();
        for document in &original {
            let choice = next() % 8;
            if choice == 0 {
                continue;
            }
            let mut document = document.clone();
            if choice <= 2 {
                document.content.push_str(" body update");
            }
            if choice <= 5 {
                document.title = Some("metadata update".to_owned());
            }
            current.push(document);
        }
        let additions = next() % 24;
        current.extend(
            (0..additions)
                .map(|n| IndexableDocument::new(format!("new-{case}-{n}"), "new evidence")),
        );
        current.reverse();
        let actual = reconcile(root.path(), &stack, &current);
        let expected = super::super::plan(root.path(), &stack, &current);
        assert_eq!(actual.is_ok(), expected.is_ok(), "case {case}");
        if actual.is_ok() {
            same_as_full_plan(root.path(), &stack, &current)?;
            accepted += 1;
        } else {
            refused += 1;
        }
    }
    assert!(accepted > 0 && refused > 0, "nonvacuous differential cases");
    Ok(())
}
