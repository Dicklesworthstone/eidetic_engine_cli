//! Bulk growth can also refresh retained session documents and withdraw rows.
//!
//! New inputs have the bounded bulk-intake allowance. Mutations of the old
//! corpus keep a separate 8192-operation bound; old input rewrites additionally
//! keep the 25% bound measured against the OLD corpus, not its inflated size.
//! Every mutation shares the existing bulk row and 32-MiB payload budgets.
//! Metadata-only changes reuse vectors only with the same ID, input digest and
//! complete producer identity. This is planning, not a new publication protocol.

use std::collections::{BTreeMap, BTreeSet};

use crate::search::IndexableDocument;

use super::{
    Delta, EmbeddingReuse, IncrementalFallback, IncrementalFallbackReason,
    MAX_LEXICAL_DELTA_DOCUMENTS, append, document_digest, document_payload_bytes,
    embedding_input_digest, incremental_fallback, over_limit, vector_delta_limit,
};

#[path = "index_delta_maintenance.rs"]
mod maintenance;

struct GrowthBudget {
    total: append::AppendBudget,
    existing: usize,
    rewrites: usize,
    rewrite_limit: usize,
}

impl GrowthBudget {
    fn new(live_documents: usize) -> Self {
        Self {
            total: append::AppendBudget::default(),
            existing: 0,
            rewrites: 0,
            rewrite_limit: vector_delta_limit(live_documents, live_documents),
        }
    }

    fn existing(&mut self, bytes: usize, reembed: bool) -> Result<(), IncrementalFallback> {
        if self.existing >= MAX_LEXICAL_DELTA_DOCUMENTS {
            return Err(over_limit("bulk intake exceeds the existing-document mutation budget"));
        }
        if reembed && self.rewrites >= self.rewrite_limit {
            return Err(over_limit("bulk intake exceeds the retained-input rewrite budget"));
        }
        // Do not partially spend a budget on a refused operation. Additions,
        // metadata refreshes, body rewrites and removal IDs all charge here.
        self.total.admit(bytes)?;
        self.existing += 1;
        self.rewrites += usize::from(reembed);
        Ok(())
    }
}

pub(super) fn try_plan(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
) -> Result<Option<Delta>, IncrementalFallback> {
    // The dispatcher reaches this fallback only after ordinary V3 limits.
    // A non-growing archive has a separate, inference-free maintenance lane;
    // any actual input rewrite still keeps the ordinary refusal unchanged.
    if documents.len() <= live.len() {
        return maintenance::try_plan(live, inputs, documents, identity);
    }
    // Preserve the already-tested pure-append path. A changed old document
    // makes it stop early; it never produces a partial delta to combine here.
    if let Some(delta) = append::try_plan(live, documents, identity.clone())? {
        return Ok(Some(delta));
    }
    let request = asupersync::Cx::current();
    plan_with_checkpoint(live, inputs, documents, identity, || {
        if let Some(cx) = &request {
            super::super::batch::checkpoint(cx, "bulk growth planning")?;
        }
        Ok(())
    })
}

fn invalid_identity_map() -> IncrementalFallback {
    incremental_fallback(
        IncrementalFallbackReason::CorpusRevisionMismatch,
        "bulk intake requires complete matching document and embedding-input identity maps",
    )
}

fn plan_with_checkpoint(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
    mut checkpoint: impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<Option<Delta>, IncrementalFallback> {
    checkpoint()?;
    let Some(growth) = documents.len().checked_sub(live.len()).filter(|count| *count > 0) else {
        // No growing import: retain the ordinary planner's refusal and its
        // established metadata/removal behavior, including empty final stores.
        return Ok(None);
    };
    if growth > append::MAX_APPEND_DOCUMENTS {
        return Err(over_limit("bulk intake exceeds the document budget"));
    }
    if live.len() != inputs.len() {
        return Err(invalid_identity_map());
    }
    for (document_id, input_id) in live.keys().zip(inputs.keys()) {
        checkpoint()?;
        if document_id != input_id {
            return Err(invalid_identity_map());
        }
    }

    let mut budget = GrowthBudget::new(live.len());
    let mut current = BTreeSet::new();
    let mut changes = Vec::new();
    for document in documents {
        checkpoint()?;
        if !current.insert(document.id.as_str()) {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "source snapshot contains duplicate document identities",
            ));
        }
        let reusable = if let Some(expected) = live.get(&document.id) {
            let digest = document_digest(document);
            checkpoint()?;
            if *expected == digest {
                continue;
            }
            let expected_input = inputs.get(&document.id).ok_or_else(invalid_identity_map)?;
            let actual_input = embedding_input_digest(document);
            checkpoint()?;
            let reuse = *expected_input == actual_input;
            let bytes = document_payload_bytes(document);
            checkpoint()?;
            budget.existing(bytes, !reuse)?;
            reuse.then_some(expected_input)
        } else {
            let bytes = document_payload_bytes(document);
            checkpoint()?;
            budget.total.admit(bytes)?;
            None
        };
        changes.push((document, reusable));
    }
    let mut removed = Vec::new();
    for id in live.keys() {
        checkpoint()?;
        if !current.contains(id.as_str()) {
            budget.existing(id.len(), false)?;
            removed.push(id);
        }
    }
    checkpoint()?;

    // Only a completely checked snapshot may transfer owned payloads to the
    // publisher. Removal order is canonical; upserts retain source order.
    let mut upserts = Vec::with_capacity(changes.len());
    let mut reusable = BTreeMap::new();
    for (document, input) in changes {
        checkpoint()?;
        if let Some(input) = input {
            reusable.insert(document.id.clone(), input.clone());
        }
        upserts.push(document.clone());
    }
    let mut removals = Vec::with_capacity(removed.len());
    for id in removed {
        checkpoint()?;
        removals.push(id.clone());
    }
    checkpoint()?;
    Ok(Some(Delta {
        upserts,
        removals,
        reuse: Some(EmbeddingReuse {
            identity,
            inputs: reusable,
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: &str = "bulk-growth-identity";

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

    fn planned(original: &[IndexableDocument], current: &[IndexableDocument]) -> Delta {
        // Exercise the production fallback entry, not just the new collector.
        super::super::diff(&digests(original), &inputs(original), current, IDENTITY.to_owned())
            .expect("admitted growth")
    }

    fn assert_reconstructs(original: &[IndexableDocument], current: &[IndexableDocument], delta: &Delta) {
        let mut applied = digests(original);
        for id in &delta.removals {
            assert!(applied.remove(id).is_some());
        }
        applied.extend(digests(&delta.upserts));
        assert_eq!(applied, digests(current));
    }

    #[test]
    fn growing_session_metadata_does_not_reembed_twenty_thousand_unchanged_inputs() {
        let original = documents(20_000);
        let mut current = documents(30_000);
        for document in &mut current[..32] {
            document.title = Some("updated session title".to_owned());
            document.metadata.insert("message_count".to_owned(), "2000".to_owned());
        }
        let delta = planned(&original, &current);
        assert_eq!(delta.upserts.len(), 10_032);
        assert!(delta.removals.is_empty());
        assert_eq!(delta.vector_upserts(IDENTITY).expect("same identity").len(), 10_000);
        assert_reconstructs(&original, &current, &delta);
        assert!(delta.vector_upserts("other producer").is_err());
    }

    #[test]
    fn large_mixed_growth_preserves_rewrites_metadata_withdrawals_and_every_new_id() {
        let original = documents(1024);
        let mut current = original[16..].to_vec();
        for document in &mut current[..16] {
            document.content.push_str(" repaired");
        }
        for document in &mut current[16..48] {
            document.metadata.insert("revision".to_owned(), "2".to_owned());
        }
        current.extend(documents(1524).into_iter().skip(1024));
        current.reverse();
        let delta = planned(&original, &current);
        assert_eq!(delta.upserts.len(), 548);
        assert_eq!(delta.removals.len(), 16);
        assert_eq!(delta.vector_upserts(IDENTITY).expect("vectors").len(), 516);
        assert_reconstructs(&original, &current, &delta);
    }

    #[test]
    fn new_growth_cannot_dilute_the_retained_corpus_rewrite_limit() {
        let original = documents(1024);
        let mut current = documents(21_024);
        for document in &mut current[..256] {
            document.content.push_str(" changed");
        }
        let delta = planned(&original, &current);
        assert_eq!(delta.vector_upserts(IDENTITY).expect("exact limit").len(), 20_256);
        current[256].content.push_str(" too much old-corpus churn");
        current.rotate_left(1024);
        let error = try_plan(&digests(&original), &inputs(&original), &current, IDENTITY.to_owned())
            .expect_err("large growth does not enlarge the old rewrite budget");
        assert!(error.detail.contains("retained-input rewrite budget"));
    }

    #[test]
    fn withdrawals_and_metadata_updates_share_one_existing_mutation_budget() {
        let mut budget = GrowthBudget::new(100_000);
        for number in 0..MAX_LEXICAL_DELTA_DOCUMENTS {
            budget.existing(number % 2, false).expect("exact existing bound");
        }
        assert!(budget.existing(0, false).is_err());
        assert!(budget.existing(0, true).is_err());
        assert_eq!(budget.existing, MAX_LEXICAL_DELTA_DOCUMENTS);
        assert_eq!(budget.rewrites, 0);
    }

    #[test]
    fn all_mutations_share_the_bulk_document_and_utf8_payload_bounds() {
        let mut budget = GrowthBudget::new(4);
        for _ in 0..append::MAX_APPEND_DOCUMENTS - 1 {
            budget.total.admit(0).expect("bulk row");
        }
        budget.existing(0, false).expect("last shared row");
        assert!(budget.total.admit(0).is_err());
        assert!(budget.existing(0, false).is_err());
        assert_eq!(budget.existing, 1);

        let mut budget = GrowthBudget::new(4);
        let removed_id = "doc-資料é";
        budget.total.admit(super::super::MAX_LEXICAL_DELTA_BYTES - removed_id.len())
            .expect("new payload");
        budget.existing(removed_id.len(), false).expect("removal spends UTF-8 bytes");
        assert!(budget.total.admit(1).is_err());
        assert!(budget.existing(usize::MAX, false).is_err());
        assert_eq!(budget.existing, 1);
    }

    #[test]
    fn late_duplicate_ids_and_incomplete_input_maps_cannot_produce_growth() {
        let original = documents(8);
        let live = digests(&original);
        let input = inputs(&original);
        for duplicate in [0, 127] {
            let mut current = documents(128);
            current[0].metadata.insert("revision".to_owned(), "2".to_owned());
            current.push(current[duplicate].clone());
            assert_eq!(try_plan(&live, &input, &current, IDENTITY.to_owned())
                .expect_err("duplicate").reason, IncrementalFallbackReason::CorpusRevisionMismatch);
        }
        let current = documents(128);
        for replace_key in [false, true] {
            let mut damaged = input.clone();
            let value = damaged.remove(&original[0].id).expect("known input");
            if replace_key {
                damaged.insert("foreign-identity".to_owned(), value);
            }
            assert_eq!(plan_with_checkpoint(&live, &damaged, &current, IDENTITY.to_owned(), || Ok(()))
                .expect_err("complete inputs required").reason,
                IncrementalFallbackReason::CorpusRevisionMismatch);
        }
    }

    #[test]
    fn equal_text_never_reuses_a_vector_under_a_new_or_withdrawn_identity() {
        let original = documents(8);
        let mut current = original[1..].to_vec();
        current[0].title = Some("updated".to_owned());
        for number in 0..16 {
            current.push(IndexableDocument::new(format!("new-{number}"), &original[0].content));
        }
        let mut delta = planned(&original, &current);
        assert_eq!(delta.vector_upserts(IDENTITY).expect("new IDs").len(), 16);
        assert_eq!(delta.removals, [original[0].id.clone()]);
        delta.upserts[0].content.push_str(" changed after planning");
        assert_eq!(delta.vector_upserts(IDENTITY).expect("use-time digest").len(), 17);
        assert_reconstructs(&original, &current, &planned(&original, &current));
    }

    #[test]
    fn non_growing_sources_retain_the_ordinary_planner() {
        let original = documents(8);
        for current in [&original[..], &original[..1], &[][..]] {
            assert!(plan_with_checkpoint(&digests(&original), &inputs(&original), current,
                IDENTITY.to_owned(), || Ok(())).expect("no growth").is_none());
        }
    }

    #[test]
    fn every_scan_and_ownership_checkpoint_can_refuse_without_a_partial_delta() {
        let original = documents(8);
        let mut current = documents(128);
        current[0].title = Some("updated".to_owned());
        let live = digests(&original);
        let input = inputs(&original);
        let mut total = 0;
        assert!(plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
            total += 1;
            Ok(())
        }).expect("count checkpoints").is_some());
        for stop_at in 1..=total {
            let mut calls = 0;
            let error = plan_with_checkpoint(&live, &input, &current, IDENTITY.to_owned(), || {
                calls += 1;
                if calls == stop_at {
                    Err(incremental_fallback(IncrementalFallbackReason::TierUnavailable,
                        "cancelled growth planning"))
                } else {
                    Ok(())
                }
            }).expect_err("cancelled before ownership escapes");
            assert_eq!(calls, stop_at);
            assert_eq!(error.detail, "cancelled growth planning");
        }
    }
}

#[cfg(test)]
#[path = "index_delta_growth_native_tests.rs"]
mod native_tests;
