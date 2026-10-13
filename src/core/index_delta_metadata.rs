//! Metadata-only changes and removals consume mutation capacity, not inference.
//!
//! Only a validated v3 input map and matching complete producer identity may
//! enter this planner. Source bodies are borrowed until every budget passes.
//! The byte bound measures cloned document payload, not total process memory.
//! Bulk evidence withdrawal must not re-embed the surviving corpus: removals
//! spend the same bounded lexical-operation allowance as upserts, while only
//! new or changed embedding inputs spend the inference allowance. V2 keeps its
//! conservative shared vector/removal limit in the parent planner. Both paths
//! still use the same private staging, tier finalization and publication fence.

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
            return Err(over_limit(
                "lexical delta exceeds the document-payload byte budget",
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
    let inference_limit = MAX_DELTA_DOCUMENTS
        .min(
            documents
                .len()
                .max(live.len())
                .saturating_mul(MAX_DELTA_PERCENT)
                / 100,
        )
        .max(1);
    let mut inference_inputs = 0;
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
            if inference_inputs == inference_limit {
                return Err(over_limit("vector delta exceeds the inference budget"));
            }
            inference_inputs += 1;
        }
        upserts.push(document);
    }
    let mut removals = Vec::new();
    for id in live.keys().filter(|id| !current.contains(id.as_str())) {
        // No embedding is produced for a removal. Charging it as inference
        // forced large session withdrawals to re-embed every surviving row.
        // Still charge each operation and its complete ID before cloning it;
        // deletions and upserts share one finite mutation allowance. The
        // existing apply path finalizes each vector tier at most once, using
        // vacuum for deletion-only work and compaction when upserts add WAL.
        lexical.admit(id.len())?;
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
    use super::super::{DocDigests, embedder_identity, plan};
    use super::*;
    use crate::search::{Embedder, HashEmbedder};
    use frankensearch::core::generation::EmbeddingIdentityBundleV1;
    use frankensearch::core::traits::{ModelCategory, SearchFuture};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    fn metadata_updates_and_bulk_removals_do_not_spend_the_inference_budget() -> TestResult {
        let original = documents(2048);
        let mut changed = original[..1024].to_vec();
        retitle(&mut changed);
        for document in &mut changed[..MAX_DELTA_DOCUMENTS] {
            document.content.push_str(" repaired");
        }
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 1024);
        assert_eq!(delta.removals.len(), 1024);
        assert_eq!(
            delta
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .len(),
            MAX_DELTA_DOCUMENTS
        );
        changed[MAX_DELTA_DOCUMENTS].content.push_str(" one too many");
        assert_eq!(
            planned(&original, &changed)
                .expect_err("inference budget")
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
            &super::super::digest_map(original),
            &inputs(original),
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
    fn duplicate_source_ids_and_new_ids_with_identical_text_cannot_bypass_admission() -> TestResult
    {
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

    #[test]
    fn bulk_removals_preserve_exact_survivors_including_an_empty_corpus() -> TestResult {
        let original = documents(1024);
        for retained in [0, 1, 128, 768, 1024] {
            let delta =
                planned(&original, &original[..retained]).map_err(|error| error.detail)?;
            assert!(delta.upserts.is_empty());
            assert_eq!(
                delta.removals,
                original[retained..]
                    .iter()
                    .map(|document| document.id.clone())
                    .collect::<Vec<_>>()
            );
            assert!(
                delta
                    .vector_upserts(IDENTITY)
                    .map_err(|error| error.detail)?
                    .is_empty()
            );
        }
        Ok(())
    }

    #[test]
    fn removals_and_upserts_share_the_exact_mutation_limit() -> TestResult {
        let original = documents(MAX_LEXICAL_DELTA_DOCUMENTS + 8);
        let delta = planned(&original, &original[..8]).map_err(|error| error.detail)?;
        assert_eq!(delta.removals.len(), MAX_LEXICAL_DELTA_DOCUMENTS);
        assert!(planned(&original, &original[..7]).is_err());

        let mut changed = original[..9].to_vec();
        retitle(&mut changed[..1]);
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(delta.removals.len(), MAX_LEXICAL_DELTA_DOCUMENTS - 1);
        assert_eq!(delta.upserts.len(), 1);
        retitle(&mut changed[1..2]);
        assert_eq!(
            planned(&original, &changed)
                .expect_err("removals and upserts cannot each claim the whole budget")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        Ok(())
    }

    #[test]
    fn removal_ids_spend_the_same_utf8_byte_allowance_as_document_updates() {
        let id = "evi_資料é";
        let mut budget = LexicalBudget {
            documents: 1,
            bytes: MAX_LEXICAL_DELTA_BYTES - id.len(),
        };
        assert!(budget.admit(id.len()).is_ok());
        assert_eq!(budget.bytes, MAX_LEXICAL_DELTA_BYTES);
        assert!(budget.admit(1).is_err());
        assert_eq!(budget.documents, 2);
        assert_eq!(budget.bytes, MAX_LEXICAL_DELTA_BYTES);
    }

    #[test]
    fn v2_bulk_removal_stays_conservative_until_a_v3_generation_is_available() -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let directory = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let stack = super::super::super::hash_fallback_embedder_stack();
        let original = documents(1024);
        let survivors = &original[..128];
        let mut legacy = DocDigests::new(&stack, &original);
        legacy.schema = super::super::DOC_DIGESTS_SCHEMA_V2.to_owned();
        legacy.embedding_inputs = None;
        legacy.write(&directory).map_err(|error| error.to_string())?;
        assert_eq!(
            plan(&directory, &stack, survivors)
                .expect_err("legacy removal budget remains conservative")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        DocDigests::new(&stack, &original)
            .write(&directory)
            .map_err(|error| error.to_string())?;
        let delta = plan(&directory, &stack, survivors).map_err(|error| error.detail)?;
        assert_eq!(delta.removals.len(), 896);
        assert!(delta.upserts.is_empty());
        Ok(())
    }

    // Observe actual hash inference without replacing the vector or lexical
    // backend. A removal must not infer the surviving documents again.
    struct CountingHash {
        inner: HashEmbedder,
        inputs: AtomicUsize,
    }

    impl Embedder for CountingHash {
        fn embed<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            text: &'a str,
        ) -> SearchFuture<'a, Vec<f32>> {
            self.inputs.fetch_add(1, Ordering::SeqCst);
            self.inner.embed(cx, text)
        }

        fn embed_batch<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            texts: &'a [&'a str],
        ) -> SearchFuture<'a, Vec<Vec<f32>>> {
            self.inputs.fetch_add(texts.len(), Ordering::SeqCst);
            self.inner.embed_batch(cx, texts)
        }

        fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
            self.inner.identity()
        }

        fn dimension(&self) -> usize {
            self.inner.dimension()
        }

        fn id(&self) -> &str {
            self.inner.id()
        }

        fn model_name(&self) -> &str {
            self.inner.model_name()
        }

        fn is_semantic(&self) -> bool {
            self.inner.is_semantic()
        }

        fn category(&self) -> ModelCategory {
            self.inner.category()
        }
    }

    fn finalized_vectors(directory: &Path) -> Result<BTreeMap<String, Vec<f32>>, String> {
        let index = crate::core::index::open_fast_vector_index_read_only(directory)
            .map_err(|error| error.detail)?;
        assert_eq!(index.wal_record_count(), 0);
        assert_eq!(index.tombstone_count(), 0);
        (0..index.record_count())
            .map(|position| {
                Ok((
                    index
                        .doc_id_at(position)
                        .map_err(|error| error.to_string())?
                        .to_owned(),
                    index
                        .vector_at_f32(position)
                        .map_err(|error| error.to_string())?,
                ))
            })
            .collect()
    }

    #[cfg(feature = "lexical-bm25")]
    async fn assert_lexical_survivors(
        cx: &asupersync::Cx,
        directory: &Path,
        expected: &[IndexableDocument],
        updated: bool,
    ) -> TestResult {
        use crate::search::LexicalRead;

        let lexical = crate::search::TantivyIndex::open_read_only(&directory.join("lexical"))
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lexical.doc_count().map_err(|error| error.to_string())? as usize,
            expected.len()
        );
        let hits = lexical
            .search(cx, "lesson", 1024)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            hits.iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<BTreeSet<_>>(),
            expected
                .iter()
                .map(|document| document.id.as_str())
                .collect::<BTreeSet<_>>()
        );
        for hit in hits {
            assert_eq!(
                hit.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("revision"))
                    .and_then(serde_json::Value::as_str),
                updated.then_some("2")
            );
        }
        Ok(())
    }

    fn native_bulk_removal_case(retained: usize, changed_inputs: usize) -> TestResult {
        use crate::core::index;

        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
            let _root = root;
            let live = parent.join("live");
            let staged = parent.join("staged");
            let rebuilt = parent.join("rebuilt");
            let observed = Arc::new(CountingHash {
                inner: HashEmbedder::default_256(),
                inputs: AtomicUsize::new(0),
            });
            let stack = crate::search::EmbedderStack::from_parts(observed.clone(), None);
            let original = documents(512);
            let builder = index::IndexBuilder::new(&live).with_embedder_stack(stack.clone());
            #[cfg(feature = "lexical-bm25")]
            let builder = builder.with_tantivy_lexical();
            builder
                .add_documents(original.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            DocDigests::new(&stack, &original)
                .write(&live)
                .map_err(|error| error.to_string())?;
            assert!(observed.inputs.load(Ordering::SeqCst) >= original.len());
            let before = std::fs::read(live.join("vector.fast.idx"))
                .map_err(|error| error.to_string())?;
            let original_vectors = finalized_vectors(&live)?;

            let mut current = original[..retained].to_vec();
            retitle(&mut current);
            for document in &mut current[..changed_inputs] {
                document.content.push_str(" repaired");
            }
            let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
            assert_eq!(delta.removals.len(), original.len() - retained);
            assert!(delta.removals.len() > MAX_DELTA_DOCUMENTS);
            assert_eq!(delta.upserts.len(), retained);
            std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
            super::super::copy_generation(&live, &staged).map_err(|error| error.to_string())?;
            observed.inputs.store(0, Ordering::SeqCst);
            super::super::apply(&cx, &staged, &stack, &delta)
                .await
                .map_err(|error| error.detail)?;
            assert_eq!(observed.inputs.load(Ordering::SeqCst), changed_inputs);
            let actual = finalized_vectors(&staged)?;
            assert_eq!(actual.len(), retained);
            assert_eq!(
                actual.keys().collect::<Vec<_>>(),
                current.iter().map(|document| &document.id).collect::<Vec<_>>()
            );
            #[cfg(feature = "lexical-bm25")]
            assert_lexical_survivors(&cx, &staged, &current, true).await?;
            if !current.is_empty() {
                let builder = index::IndexBuilder::new(&rebuilt).with_embedder_stack(stack);
                #[cfg(feature = "lexical-bm25")]
                let builder = builder.with_tantivy_lexical();
                builder
                    .add_documents(current.clone())
                    .build(&cx)
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(actual, finalized_vectors(&rebuilt)?);
                #[cfg(feature = "lexical-bm25")]
                assert_lexical_survivors(&cx, &rebuilt, &current, true).await?;
            }
            assert_eq!(finalized_vectors(&live)?, original_vectors);
            assert_eq!(
                std::fs::read(live.join("vector.fast.idx"))
                    .map_err(|error| error.to_string())?,
                before
            );
            #[cfg(feature = "lexical-bm25")]
            assert_lexical_survivors(&cx, &live, &original, false).await?;
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn native_bulk_removal_reuses_surviving_vectors_and_updates_lexical_metadata() -> TestResult {
        native_bulk_removal_case(64, 0)
    }

    #[test]
    fn native_bulk_removal_with_body_changes_matches_a_fresh_rebuild() -> TestResult {
        native_bulk_removal_case(64, 5)
    }

    #[test]
    fn native_bulk_removal_can_finalize_an_empty_index_without_inference() -> TestResult {
        native_bulk_removal_case(0, 0)
    }
}
