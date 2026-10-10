//! Metadata-only changes consume lexical capacity, not the inference budget.
//!
//! Only a validated v3 input map and matching complete producer identity may
//! enter this planner. Source bodies are borrowed until every budget passes.
//! The byte bound measures cloned document payload, not total process memory.

use std::collections::{BTreeMap, BTreeSet};

use crate::search::IndexableDocument;

use super::{
    Delta, EmbeddingReuse, IncrementalFallback, IncrementalFallbackReason, MAX_DELTA_DOCUMENTS,
    MAX_DELTA_PERCENT, document_digest, embedding_input_digest, incremental_fallback,
};

const MAX_LEXICAL_DELTA_DOCUMENTS: usize = 8192;
const MAX_LEXICAL_DELTA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
struct LexicalBudget {
    documents: usize,
    bytes: usize,
}

impl LexicalBudget {
    fn admit(&mut self, bytes: usize) -> Result<(), IncrementalFallback> {
        if self.documents == MAX_LEXICAL_DELTA_DOCUMENTS {
            return Err(over_limit("lexical delta exceeds the document budget"));
        }
        if bytes > MAX_LEXICAL_DELTA_BYTES.saturating_sub(self.bytes) {
            return Err(over_limit("lexical delta exceeds the document-payload byte budget"));
        }
        self.documents += 1;
        self.bytes += bytes;
        Ok(())
    }
}

fn over_limit(detail: &str) -> IncrementalFallback {
    incremental_fallback(IncrementalFallbackReason::DeltaOverThreshold, detail)
}

fn document_payload_bytes(document: &IndexableDocument) -> usize {
    document.metadata.iter().fold(
        document
            .id
            .len()
            .saturating_add(document.content.len())
            .saturating_add(document.title.as_ref().map_or(0, String::len)),
        |bytes, (key, value)| bytes.saturating_add(key.len()).saturating_add(value.len()),
    )
}

pub(super) fn diff(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
) -> Result<Delta, IncrementalFallback> {
    diff_with_digest(live, inputs, documents, identity, document_digest)
}

fn diff_with_digest(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
    mut digest: impl FnMut(&IndexableDocument) -> String,
) -> Result<Delta, IncrementalFallback> {
    let vector_limit = MAX_DELTA_DOCUMENTS
        .min(
            documents
                .len()
                .max(live.len())
                .saturating_mul(MAX_DELTA_PERCENT)
                / 100,
        )
        .max(1);
    let mut vector_changes = 0;
    let mut lexical = LexicalBudget::default();
    let mut current = BTreeSet::new();
    let mut upserts = Vec::new();
    let mut reusable = BTreeMap::new();
    for document in documents {
        if !current.insert(document.id.as_str()) {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "source snapshot contains duplicate document identities",
            ));
        }
        if live.get(&document.id) == Some(&digest(document)) {
            continue;
        }
        lexical.admit(document_payload_bytes(document))?;
        let input = embedding_input_digest(document);
        if live.contains_key(&document.id) && inputs.get(&document.id) == Some(&input) {
            reusable.insert(document.id.clone(), input);
        } else {
            if vector_changes == vector_limit {
                return Err(over_limit("vector delta exceeds the inference/removal budget"));
            }
            vector_changes += 1;
        }
        upserts.push(document);
    }
    let mut removals = Vec::new();
    for id in live.keys().filter(|id| !current.contains(id.as_str())) {
        if vector_changes == vector_limit {
            return Err(over_limit("vector delta exceeds the inference/removal budget"));
        }
        lexical.admit(id.len())?;
        vector_changes += 1;
        removals.push(id.clone());
    }
    Ok(Delta {
        upserts: upserts.into_iter().cloned().collect(),
        removals,
        reuse: Some(EmbeddingReuse {
            identity,
            inputs: reusable,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{DocDigests, embedder_identity, plan};

    type TestResult = Result<(), String>;
    const IDENTITY: &str = "planner-fixture-identity";

    fn documents(count: usize) -> Vec<IndexableDocument> {
        (0..count)
            .map(|number| {
                IndexableDocument::new(format!("mem_{number:05}"), format!("lesson {number}"))
            })
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
            document.title = Some("updated title".to_owned());
            document
                .metadata
                .insert("revision".to_owned(), "2".to_owned());
        }
    }

    fn planned(
        original: &[IndexableDocument],
        changed: &[IndexableDocument],
    ) -> Result<Delta, IncrementalFallback> {
        diff(
            &super::super::digest_map(original),
            &inputs(original),
            changed,
            IDENTITY.to_owned(),
        )
    }

    #[test]
    fn whole_corpus_metadata_refresh_keeps_every_lexical_update_and_reuses_vectors() -> TestResult {
        let original = documents(1024);
        let mut changed = original.clone();
        retitle(&mut changed);
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), changed.len());
        assert!(delta.removals.is_empty());
        assert!(
            delta
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .is_empty()
        );
        for (actual, expected) in delta.upserts.iter().zip(&changed) {
            assert_eq!(document_digest(actual), document_digest(expected));
        }
        Ok(())
    }

    #[test]
    fn metadata_updates_do_not_spend_the_shared_vector_update_and_removal_budget() -> TestResult {
        let original = documents(2048);
        let mut changed = original[..1920].to_vec();
        retitle(&mut changed);
        for document in &mut changed[..128] {
            document.content.push_str(" repaired");
        }
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 1920);
        assert_eq!(delta.removals.len(), 128);
        assert_eq!(
            delta
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .len(),
            128
        );
        changed[128].content.push_str(" one too many");
        assert_eq!(
            planned(&original, &changed)
                .expect_err("shared vector budget")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        Ok(())
    }

    #[test]
    fn body_rewrites_still_stop_at_the_first_over_budget_document() {
        let original = documents(2048);
        let mut changed = original.clone();
        for document in &mut changed {
            document.content.push_str(" rewritten");
        }
        let mut inspected = 0;
        let error = diff_with_digest(
            &super::super::digest_map(&original),
            &inputs(&original),
            &changed,
            IDENTITY.to_owned(),
            |document| {
                inspected += 1;
                document_digest(document)
            },
        )
        .expect_err("full body rewrite");
        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
        assert_eq!(inspected, MAX_DELTA_DOCUMENTS + 1);
    }

    #[test]
    fn lexical_document_bound_is_inclusive_and_independent_of_corpus_percentage() -> TestResult {
        let original = documents(MAX_LEXICAL_DELTA_DOCUMENTS + 1);
        let mut changed = original.clone();
        retitle(&mut changed[..MAX_LEXICAL_DELTA_DOCUMENTS]);
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), MAX_LEXICAL_DELTA_DOCUMENTS);
        assert!(
            delta
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .is_empty()
        );
        retitle(&mut changed[MAX_LEXICAL_DELTA_DOCUMENTS..]);
        assert_eq!(
            planned(&original, &changed)
                .expect_err("lexical budget")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        Ok(())
    }

    #[test]
    fn byte_budget_counts_utf8_payload_and_rejects_overflow_without_allocating() {
        let mut document = IndexableDocument::new("é", "資料").with_title("🏴");
        document.metadata.insert("clé".to_owned(), "値".to_owned());
        let expected = "é".len() + "資料".len() + "🏴".len() + "clé".len() + "値".len();
        assert_eq!(document_payload_bytes(&document), expected);
        let mut budget = LexicalBudget::default();
        assert!(budget.admit(MAX_LEXICAL_DELTA_BYTES - expected).is_ok());
        assert!(budget.admit(document_payload_bytes(&document)).is_ok());
        assert_eq!(budget.bytes, MAX_LEXICAL_DELTA_BYTES);
        assert!(budget.admit(1).is_err());
        assert!(budget.admit(usize::MAX).is_err());
        assert_eq!(budget.documents, 2);
        assert_eq!(budget.bytes, MAX_LEXICAL_DELTA_BYTES);
    }

    #[test]
    fn v2_cannot_gain_the_metadata_allowance_without_a_v3_input_commitment() -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let directory = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let stack = super::super::super::hash_fallback_embedder_stack();
        let original = documents(1024);
        let mut changed = original.clone();
        retitle(&mut changed);
        let mut digests = DocDigests::new(&stack, &original);
        digests.schema = super::super::DOC_DIGESTS_SCHEMA_V2.to_owned();
        digests.embedding_inputs = None;
        digests
            .write(&directory)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            plan(&directory, &stack, &changed)
                .expect_err("v2 cannot prove input equality")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        DocDigests::new(&stack, &original)
            .write(&directory)
            .map_err(|error| error.to_string())?;
        let delta = plan(&directory, &stack, &changed).map_err(|error| error.detail)?;
        let identity = embedder_identity(&stack).ok_or("complete identity")?;
        assert!(
            delta
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?
                .is_empty()
        );
        assert_eq!(delta.upserts.len(), original.len());
        Ok(())
    }

    #[test]
    fn duplicate_source_ids_and_new_ids_with_identical_text_cannot_bypass_admission() -> TestResult {
        let original = documents(8);
        let mut changed = original.clone();
        retitle(&mut changed);
        changed.push(changed[0].clone());
        assert_eq!(
            planned(&original, &changed)
                .expect_err("duplicate identity")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        changed.pop();
        for number in 0..2 {
            changed.push(IndexableDocument::new(
                format!("new_{number}"),
                &original[0].content,
            ));
        }
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(
            delta
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .len(),
            2
        );
        changed.push(IndexableDocument::new("new_2", &original[0].content));
        assert_eq!(
            planned(&original, &changed)
                .expect_err("new IDs spend vector budget")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        Ok(())
    }

    #[test]
    fn expanded_metadata_plan_retains_use_time_identity_and_input_checks() -> TestResult {
        let original = documents(1024);
        let mut changed = original.clone();
        retitle(&mut changed);
        let mut delta = planned(&original, &changed).map_err(|error| error.detail)?;
        delta.upserts[0].content.push_str(" changed after planning");
        delta.removals.push(delta.upserts[1].id.clone());
        let vectors = delta
            .vector_upserts(IDENTITY)
            .map_err(|error| error.detail)?;
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0].id, original[0].id);
        assert_eq!(vectors[1].id, original[1].id);
        assert_eq!(
            delta
                .vector_upserts("different-producer")
                .expect_err("producer drift")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        Ok(())
    }
}
