//! Bounded append-only intake for an already identity-validated V3 generation.
//!
//! A large import should not infer unchanged history again merely because new
//! documents exceed the rewrite planner's percentage. This lane requires a
//! stronger source proof: every old ID is present exactly once and its complete
//! document digest is unchanged. No metadata rewrite or withdrawal qualifies.
//! The existing staged publisher still owns tier validation, leases, rollback
//! and cancellation. This planner never opens an index or writes source state.

use std::collections::{BTreeMap, BTreeSet};

use crate::search::IndexableDocument;

use super::{
    Delta, EmbeddingReuse, IncrementalFallback, IncrementalFallbackReason, MAX_LEXICAL_DELTA_BYTES,
    document_digest, document_payload_bytes, incremental_fallback,
};

// Inference and WAL writes remain bounded to the existing small batches.
// The retained delta is bounded separately: at most 64K new document objects
// and the SAME 32-MiB payload ceiling as ordinary V3 intake. This is not an RSS
// bound, nor permission to bypass the publisher's storage/request budgets.
pub(super) const MAX_APPEND_DOCUMENTS: usize = 65_536;

#[derive(Default)]
pub(super) struct AppendBudget {
    documents: usize,
    bytes: usize,
}

impl AppendBudget {
    pub(super) fn admit(&mut self, bytes: usize) -> Result<(), IncrementalFallback> {
        if self.documents >= MAX_APPEND_DOCUMENTS {
            return Err(over_limit("bulk intake exceeds the document budget"));
        }
        if bytes > MAX_LEXICAL_DELTA_BYTES.saturating_sub(self.bytes) {
            return Err(over_limit(
                "bulk intake exceeds the document-payload byte budget",
            ));
        }
        self.documents += 1;
        self.bytes += bytes;
        Ok(())
    }
}

fn over_limit(detail: &str) -> IncrementalFallback {
    incremental_fallback(IncrementalFallbackReason::DeltaOverThreshold, detail)
}

/// None means this source is not append-only; preserve the ordinary planner's
/// refusal. An error is a violated bound, duplicate identity or cancellation,
/// never a partial delta. The parent has already checked V3 map completeness
/// and the complete fast/quality producer identity before calling this lane.
pub(super) fn try_plan(
    live: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
) -> Result<Option<Delta>, IncrementalFallback> {
    let request = asupersync::Cx::current();
    let mut checkpoint = || {
        if let Some(cx) = &request {
            super::super::batch::checkpoint(cx, "append planning")?;
        }
        Ok(())
    };
    let Some(additions) = collect_appends(live, documents, &mut checkpoint)? else {
        return Ok(None);
    };
    // Do not clone a new body until the entire old source has been proven
    // unchanged and every new payload has passed both bounds.
    let mut upserts = Vec::with_capacity(additions.len());
    for document in additions {
        checkpoint()?;
        upserts.push(document.clone());
    }
    checkpoint()?;
    tracing::debug!(
        target: "ee::index",
        retained_documents = live.len(),
        appended_documents = upserts.len(),
        "planned bounded append-only index intake"
    );
    Ok(Some(Delta {
        upserts,
        removals: Vec::new(),
        // Empty reuse means EVERY new ID gets a vector, including new IDs
        // whose text equals an old document. Identity is rechecked at apply.
        reuse: Some(EmbeddingReuse {
            identity,
            inputs: BTreeMap::new(),
        }),
    }))
}

fn collect_appends<'a>(
    live: &BTreeMap<String, String>,
    documents: &'a [IndexableDocument],
    checkpoint: &mut impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<Option<Vec<&'a IndexableDocument>>, IncrementalFallback> {
    checkpoint()?;
    // A pure append must grow the source. Refuse obvious non-appends and
    // oversized growth before allocating the source-identity membership set.
    let Some(added) = documents
        .len()
        .checked_sub(live.len())
        .filter(|count| *count > 0)
    else {
        return Ok(None);
    };
    if added > MAX_APPEND_DOCUMENTS {
        return Err(over_limit("bulk intake exceeds the document budget"));
    }
    let mut seen = BTreeSet::new();
    let mut retained = 0_usize;
    let mut budget = AppendBudget::default();
    let mut additions = Vec::new();
    for document in documents {
        checkpoint()?;
        if !seen.insert(document.id.as_str()) {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "source snapshot contains duplicate document identities",
            ));
        }
        if let Some(expected) = live.get(&document.id) {
            let actual = document_digest(document);
            checkpoint()?;
            if *expected != actual {
                return Ok(None);
            }
            retained += 1;
        } else {
            let bytes = document_payload_bytes(document);
            checkpoint()?;
            budget.admit(bytes)?;
            additions.push(document);
        }
    }
    checkpoint()?;
    // Equal old-row count plus unique IDs proves full membership, even when
    // source order differs. A missing late old row must not hide behind growth.
    if retained != live.len() {
        return Ok(None);
    }
    Ok(Some(additions))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: &str = "append-planner-identity";

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

    #[test]
    fn bulk_append_retains_every_old_digest_and_only_owns_new_documents() {
        let original = documents(1024);
        let current = documents(21_024);
        let delta = try_plan(&digests(&original), &current, IDENTITY.to_owned())
            .expect("bounded import")
            .expect("pure append");
        assert_eq!(delta.upserts.len(), 20_000);
        assert!(delta.removals.is_empty());
        assert_eq!(digests(&delta.upserts), digests(&current[1024..]));
        let mut reconstructed = digests(&original);
        reconstructed.extend(digests(&delta.upserts));
        assert_eq!(reconstructed, digests(&current));
        assert_eq!(
            delta.vector_upserts(IDENTITY).expect("same producer").len(),
            20_000
        );
        assert!(delta.vector_upserts("changed-producer").is_err());
    }

    #[test]
    fn late_body_title_or_metadata_rewrites_cannot_use_the_append_allowance() {
        let original = documents(8);
        for change in 0..3 {
            let mut current = documents(1024);
            match change {
                0 => current[7].content.push_str(" changed"),
                1 => current[7].title = Some("changed title".to_owned()),
                _ => {
                    current[7]
                        .metadata
                        .insert("scope".to_owned(), "changed".to_owned());
                }
            }
            current.rotate_left(8);
            assert!(
                try_plan(&digests(&original), &current, IDENTITY.to_owned())
                    .expect("composition check")
                    .is_none()
            );
        }
    }

    #[test]
    fn a_missing_old_document_cannot_be_hidden_by_many_additions() {
        let original = documents(8);
        let current = documents(1024);
        assert!(
            try_plan(&digests(&original), &current[1..], IDENTITY.to_owned())
                .expect("membership check")
                .is_none()
        );
    }

    #[test]
    fn duplicate_old_or_new_ids_never_produce_an_append_plan() {
        let original = documents(8);
        for duplicate in [0, 100] {
            let mut current = documents(1024);
            current.push(current[duplicate].clone());
            let error = try_plan(&digests(&original), &current, IDENTITY.to_owned())
                .expect_err("duplicate source identity");
            assert_eq!(
                error.reason,
                IncrementalFallbackReason::CorpusRevisionMismatch
            );
        }
    }

    #[test]
    fn source_order_and_identical_text_do_not_alias_document_identities() {
        let original = documents(8);
        let mut current = original.clone();
        for number in 0..16 {
            current.push(IndexableDocument::new(
                format!("new-{number}"),
                &original[0].content,
            ));
        }
        current.reverse();
        let expected: Vec<_> = current
            .iter()
            .take(16)
            .map(|document| document.id.as_str())
            .collect();
        let delta = try_plan(&digests(&original), &current, IDENTITY.to_owned())
            .expect("source order")
            .expect("append");
        assert_eq!(
            delta
                .upserts
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            delta
                .vector_upserts(IDENTITY)
                .expect("new vector IDs")
                .len(),
            16
        );
    }

    #[test]
    fn zero_growth_and_withdrawals_keep_the_ordinary_planner() {
        let original = documents(8);
        for current in [&original[..], &original[..7], &original[..0]] {
            assert!(
                try_plan(&digests(&original), current, IDENTITY.to_owned())
                    .expect("no growth")
                    .is_none()
            );
        }
        assert!(
            try_plan(&BTreeMap::new(), &[], IDENTITY.to_owned())
                .expect("empty")
                .is_none()
        );
        let current = documents(128);
        let delta = try_plan(&BTreeMap::new(), &current, IDENTITY.to_owned())
            .expect("existing empty generation")
            .expect("append");
        assert_eq!(delta.upserts.len(), current.len());
    }

    #[test]
    fn append_document_limit_is_inclusive_without_lifting_the_payload_limit() {
        let original = documents(1);
        let mut current = documents(MAX_APPEND_DOCUMENTS + 1);
        let delta = try_plan(&digests(&original), &current, IDENTITY.to_owned())
            .expect("exact row bound")
            .expect("append");
        assert_eq!(delta.upserts.len(), MAX_APPEND_DOCUMENTS);
        current.push(IndexableDocument::new("one-too-many", "lesson"));
        assert_eq!(
            try_plan(&digests(&original), &current, IDENTITY.to_owned())
                .expect_err("row bound")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        let mut budget = AppendBudget::default();
        assert!(budget.admit(MAX_LEXICAL_DELTA_BYTES).is_ok());
        assert!(budget.admit(0).is_ok());
        assert!(budget.admit(1).is_err());
        assert!(budget.admit(usize::MAX).is_err());
        assert_eq!(budget.documents, 2);
        assert_eq!(budget.bytes, MAX_LEXICAL_DELTA_BYTES);
    }

    #[test]
    fn full_utf8_payload_is_charged_before_any_new_body_is_cloned() {
        let original = documents(1);
        let mut current = original.clone();
        let mut added = IndexableDocument::new("new-é", "資料").with_title("title");
        added.metadata.insert("clé".to_owned(), "値".to_owned());
        let overhead = document_payload_bytes(&added);
        added
            .content
            .push_str(&"x".repeat(MAX_LEXICAL_DELTA_BYTES - overhead));
        current.push(added);
        let live = digests(&original);
        assert_eq!(
            collect_appends(&live, &current, &mut || Ok(()))
                .expect("exact payload")
                .expect("append")
                .len(),
            1
        );
        current[1]
            .metadata
            .insert("extra".to_owned(), "value".to_owned());
        let error =
            collect_appends(&live, &current, &mut || Ok(())).expect_err("metadata counts too");
        assert!(error.detail.contains("byte budget"));
    }

    #[test]
    fn cancellation_stops_before_scanning_and_at_the_final_membership_check() {
        let original = documents(8);
        let current = documents(128);
        let live = digests(&original);
        let mut total = 0;
        assert!(
            collect_appends(&live, &current, &mut || {
                total += 1;
                Ok(())
            })
            .expect("reference scan")
            .is_some()
        );
        for stop_at in [1, 7, total] {
            let mut calls = 0;
            let error = collect_appends(&live, &current, &mut || {
                calls += 1;
                if calls == stop_at {
                    Err(incremental_fallback(
                        IncrementalFallbackReason::TierUnavailable,
                        "cancelled append scan",
                    ))
                } else {
                    Ok(())
                }
            })
            .expect_err("checkpoint refusal");
            assert_eq!(calls, stop_at);
            assert_eq!(error.detail, "cancelled append scan");
        }
    }
}
