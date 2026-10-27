//! Pure append and mixed growth share one retained-document comparison pass.

use super::*;

const IDENTITY: &str = "single-pass-growth";

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
fn bulk_growth_hashes_each_survivor_once_even_with_a_late_change_or_withdrawal() {
    let original = documents(1024);
    let live = digests(&original);
    let input = inputs(&original);
    for change in 0..4 {
        let mut current = documents(4096);
        match change {
            1 => current[1023].title = Some("late metadata refresh".to_owned()),
            2 => current[1023].content.push_str(" late body repair"),
            3 => {
                current.remove(1023);
            }
            _ => {}
        }
        // New rows first, the changing old row last: the old append probe
        // would scan nearly the entire input before starting over.
        current.rotate_left(1024 - usize::from(change == 3));
        let mut hashed = BTreeSet::new();
        let delta = try_plan_with_digest(&live, &input, &current, IDENTITY.to_owned(), |d| {
            assert!(
                live.contains_key(&d.id),
                "do not hash new payload for equality"
            );
            assert!(hashed.insert(d.id.clone()), "one bulk pass per retained ID");
            document_digest(d)
        })
        .expect("bounded bulk growth")
        .expect("growing source");
        assert_eq!(hashed.len(), 1024 - usize::from(change == 3));
        assert_eq!(
            delta.upserts.len(),
            3072 + usize::from(change == 1 || change == 2)
        );
        assert_eq!(delta.removals.len(), usize::from(change == 3));
        assert_eq!(
            delta.vector_upserts(IDENTITY).expect("same producer").len(),
            3072 + usize::from(change == 2)
        );
        let mut reconstructed = live.clone();
        for id in &delta.removals {
            assert!(reconstructed.remove(id).is_some());
        }
        reconstructed.extend(digests(&delta.upserts));
        assert_eq!(reconstructed, digests(&current));
        assert!(delta.vector_upserts("wrong-producer").is_err());
    }
}

#[test]
fn direct_growth_matches_the_existing_pure_append_oracle_in_any_source_order() {
    let original = documents(32);
    let live = digests(&original);
    let input = inputs(&original);
    for rotation in [0, 1, 31, 32, 63, 127] {
        let mut current = documents(128);
        current.rotate_left(rotation);
        let reference = append::try_plan(&live, &current, IDENTITY.to_owned())
            .expect("pure append oracle")
            .expect("append-only");
        let actual = try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect("direct growth")
            .expect("growing source");
        assert_eq!(digests(&actual.upserts), digests(&reference.upserts));
        assert_eq!(
            actual.upserts.iter().map(|d| &d.id).collect::<Vec<_>>(),
            reference.upserts.iter().map(|d| &d.id).collect::<Vec<_>>()
        );
        assert_eq!(actual.removals, reference.removals);
        assert_eq!(actual.vector_upserts(IDENTITY).expect("vectors").len(), 96);
    }
    let current = documents(128);
    let bootstrap = try_plan(
        &BTreeMap::new(),
        &BTreeMap::new(),
        &current,
        IDENTITY.to_owned(),
    )
    .expect("empty baseline")
    .expect("bootstrap append");
    assert_eq!(digests(&bootstrap.upserts), digests(&current));
    assert_eq!(
        bootstrap
            .vector_upserts(IDENTITY)
            .expect("all new vectors")
            .len(),
        128
    );
}

#[test]
fn pure_growth_cannot_skip_complete_input_maps_or_late_duplicate_identity_checks() {
    let original = documents(32);
    let live = digests(&original);
    let input = inputs(&original);
    let current = documents(128);
    for replaced in [false, true] {
        let mut damaged = input.clone();
        let value = damaged.remove(&original[31].id).expect("known input");
        if replaced {
            damaged.insert("unrelated-id".to_owned(), value);
        }
        let mut hashes = 0;
        let error = try_plan_with_digest(&live, &damaged, &current, IDENTITY.to_owned(), |d| {
            hashes += 1;
            document_digest(d)
        })
        .expect_err("even pure growth requires a complete V3 input map");
        assert_eq!(
            error.reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        assert_eq!(hashes, 0);
    }
    for duplicate in [0, 127] {
        let mut current = current.clone();
        current.push(current[duplicate].clone());
        let error = try_plan(&live, &input, &current, IDENTITY.to_owned())
            .expect_err("duplicate identities cannot be hidden by growth");
        assert_eq!(
            error.reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
    }
}

#[test]
fn actual_growth_dispatch_observes_cancellation_after_its_first_retained_hash() -> Result<(), String>
{
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(10), |cx| async move {
        let original = documents(32);
        let live = digests(&original);
        let input = inputs(&original);
        let current = documents(128);
        let mut hashed = 0;
        let error = try_plan_with_digest(&live, &input, &current, IDENTITY.to_owned(), |d| {
            hashed += 1;
            cx.set_cancel_reason(asupersync::CancelReason::user("stop direct bulk planning"));
            document_digest(d)
        })
        .expect_err("cancelled equality check cannot launch another planner");
        assert_eq!(hashed, 1);
        assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
        assert!(cx.checkpoint().is_err());
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
