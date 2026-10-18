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
//! V3 inference scales with the corpus, not the legacy 256-document ceiling:
//! bounded embedding/WAL batches can process a substantial import without
//! re-embedding every unchanged survivor. Mixed mutations retain independent
//! percentage, 8192-mutation and 32-MiB payload ceilings. Growing imports may
//! separate new inputs from bounded old-corpus changes, without using growth
//! to dilute the retained-input rewrite percentage or re-embedding metadata.
//! Non-growing bulk maintenance has the same stronger input-equality proof:
//! metadata refreshes and withdrawals may share the bulk mutation budget only
//! when every surviving identity and embedding input remain unchanged.

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;

use crate::search::IndexableDocument;

use super::{
    Delta, EmbeddingReuse, IncrementalFallback, IncrementalFallbackReason, MAX_DELTA_PERCENT,
    document_digest, embedding_input_digest, incremental_fallback,
};

#[path = "index_delta_append.rs"]
mod append;

#[path = "index_delta_growth.rs"]
mod growth;

#[path = "index_delta_planning.rs"]
mod planning;
use planning::diff_with_digest;

const MAX_LEXICAL_DELTA_DOCUMENTS: usize = 8192;
const MAX_LEXICAL_DELTA_BYTES: usize = 32 * 1024 * 1024;

/// Batching bounds each inference/WAL operation; it need not bound the entire
/// generation to 256 inputs. Keep the existing full-rebuild percentage policy
/// and the shared mutation ceiling, including for very large source counts.
/// An empty/tiny corpus retains its historical one-input progress allowance.
fn vector_delta_limit(live_documents: usize, source_documents: usize) -> usize {
    (live_documents
        .max(source_documents)
        .saturating_mul(MAX_DELTA_PERCENT)
        / 100)
        .clamp(1, MAX_LEXICAL_DELTA_DOCUMENTS)
}

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

/// Shared by planning and bounded lexical write batches. Counts UTF-8 payload
/// across every indexed field; it is not a provider-allocation or RSS estimate.
pub(super) fn document_payload_bytes(document: &IndexableDocument) -> usize {
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
    match diff_with_digest(live, inputs, documents, identity.clone(), document_digest) {
        Err(error) if error.reason == IncrementalFallbackReason::DeltaOverThreshold => {
            // Thresholds are rewrite-cost heuristics, not proof that existing
            // vectors need inference again. The fallback distinguishes growing
            // intake from inference-free maintenance of a non-growing corpus.
            // Both share bounded mutations and preserve use-time input checks.
            match growth::try_plan(live, inputs, documents, identity)? {
                Some(delta) => Ok(delta),
                None => Err(error),
            }
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DocDigests, MAX_DELTA_DOCUMENTS, embedder_identity, plan};
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
        // 25% of the larger (2048-row) snapshot, not the legacy 256-row cap.
        let inference_limit = 512;
        for document in &mut changed[..inference_limit] {
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
            inference_limit
        );
        changed[inference_limit].content.push_str(" one too many");
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
        assert_eq!(inspected, 513);
    }

    #[test]
    fn vector_allowance_scales_but_keeps_percentage_and_absolute_bounds() {
        for (live, source, expected) in [
            (0, 0, 1),
            (0, 1, 1),
            (8, 8, 2),
            (1024, 1024, 256),
            (2048, 1024, 512),
            (3072, 4096, 1024),
            (32_768, 32_768, 8192),
            (usize::MAX, usize::MAX, 8192),
        ] {
            assert_eq!(vector_delta_limit(live, source), expected);
            assert_eq!(vector_delta_limit(source, live), expected);
        }
    }

    #[test]
    fn large_import_keeps_exact_additions_rewrites_metadata_and_withdrawals() -> TestResult {
        let original = documents(4096);
        let mut current = original[128..].to_vec();
        for document in &mut current[..128] {
            document.content.push_str(" repaired");
        }
        retitle(&mut current[128..384]);
        current.extend((4096..5120).map(|number| {
            IndexableDocument::new(format!("mem_{number:05}"), format!("lesson {number}"))
        }));
        let delta = planned(&original, &current).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 1408);
        assert_eq!(delta.removals.len(), 128);
        let vectors = delta
            .vector_upserts(IDENTITY)
            .map_err(|error| error.detail)?;
        assert_eq!(vectors.len(), 1152);
        assert!(vectors.len() > MAX_DELTA_DOCUMENTS);
        let expected_vectors: BTreeSet<_> = current[..128]
            .iter()
            .chain(current[3968..].iter())
            .map(|document| document.id.as_str())
            .collect();
        assert_eq!(
            vectors
                .iter()
                .map(|document| document.id.as_str())
                .collect::<BTreeSet<_>>(),
            expected_vectors
        );
        // Apply to an independent document-digest map, not planner counters.
        let mut applied = super::super::digest_map(&original);
        for id in &delta.removals {
            applied.remove(id);
        }
        for document in &delta.upserts {
            applied.insert(document.id.clone(), document_digest(document));
        }
        assert_eq!(applied, super::super::digest_map(&current));
        assert!(delta.vector_upserts("different-producer").is_err());
        Ok(())
    }

    #[test]
    fn large_append_uses_the_import_lane_after_the_ordinary_percentage_limit() -> TestResult {
        let original = documents(3072);
        let mut current = documents(4096);
        let delta = planned(&original, &current).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 1024);
        assert!(delta.removals.is_empty());
        current.push(IndexableDocument::new("mem_04096", "lesson 4096"));
        let error = diff_with_digest(
            &super::super::digest_map(&original),
            &inputs(&original),
            &current,
            IDENTITY.to_owned(),
            document_digest,
        )
        .expect_err("ordinary rewrite percentage remains enforced");
        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
        assert!(error.detail.contains("inference budget"));
        let delta = planned(&original, &current).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 1025);
        assert!(delta.removals.is_empty());
        assert_eq!(
            super::super::digest_map(&delta.upserts),
            super::super::digest_map(&current[3072..])
        );
        Ok(())
    }

    #[test]
    fn large_vector_intake_requires_v3_and_a_matching_producer() -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let directory = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let stack = super::super::super::hash_fallback_embedder_stack();
        let original = documents(2048);
        let mut changed = original.clone();
        for document in &mut changed[..512] {
            document.content.push_str(" repaired");
        }
        let mut digests = DocDigests::new(&stack, &original);
        digests.schema = super::super::DOC_DIGESTS_SCHEMA_V2.to_owned();
        digests.embedding_inputs = None;
        digests
            .write(&directory)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            plan(&directory, &stack, &changed)
                .expect_err("legacy limit")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        DocDigests::new(&stack, &original)
            .write(&directory)
            .map_err(|error| error.to_string())?;
        let delta = plan(&directory, &stack, &changed).map_err(|error| error.detail)?;
        let identity = embedder_identity(&stack).ok_or("complete identity")?;
        assert_eq!(
            delta
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?
                .len(),
            512
        );
        let mut digests = DocDigests::new(&stack, &original);
        digests.embedder = Some("other-producer".to_owned());
        digests
            .write(&directory)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            plan(&directory, &stack, &changed)
                .expect_err("identity changed")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        Ok(())
    }

    #[test]
    fn expanded_intake_still_rejects_duplicate_ids_and_excess_payload() {
        let original = documents(2048);
        let mut changed = original.clone();
        for document in &mut changed[..300] {
            document.content.push_str(" repaired");
        }
        changed.push(changed[0].clone());
        assert_eq!(
            planned(&original, &changed)
                .expect_err("duplicate after legacy bound")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        changed.pop();
        changed[299].content = "x".repeat(MAX_LEXICAL_DELTA_BYTES + 1);
        let error = planned(&original, &changed).expect_err("payload is independently bounded");
        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
        assert!(error.detail.contains("byte budget"));
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
        // The ordinary mixed-mutation ceiling is unchanged. Only the stronger
        // inference-free fallback can accept the complete metadata refresh.
        assert_eq!(
            diff_with_digest(
                &super::super::digest_map(&original),
                &inputs(&original),
                &changed,
                IDENTITY.to_owned(),
                document_digest,
            )
            .expect_err("ordinary lexical budget")
            .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        let maintenance = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(maintenance.upserts.len(), original.len());
        assert!(
            maintenance
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .is_empty()
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
        // Growth gets its own intake allowance; equal text still requires
        // every new document identity to receive a vector.
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(
            delta
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .len(),
            3
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
            let delta = planned(&original, &original[..retained]).map_err(|error| error.detail)?;
            assert!(delta.upserts.is_empty());
            assert_eq!(delta.removals.len(), original.len() - retained);
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
        assert!(
            diff_with_digest(
                &super::super::digest_map(&original),
                &inputs(&original),
                &original[..7],
                IDENTITY.to_owned(),
                document_digest,
            )
            .is_err()
        );
        let maintenance = planned(&original, &original[..7]).map_err(|error| error.detail)?;
        assert_eq!(maintenance.removals.len(), MAX_LEXICAL_DELTA_DOCUMENTS + 1);
        assert!(maintenance.upserts.is_empty());

        let mut changed = original[..9].to_vec();
        retitle(&mut changed[..1]);
        let delta = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(delta.removals.len(), MAX_LEXICAL_DELTA_DOCUMENTS - 1);
        assert_eq!(delta.upserts.len(), 1);
        retitle(&mut changed[1..2]);
        assert_eq!(
            diff_with_digest(
                &super::super::digest_map(&original),
                &inputs(&original),
                &changed,
                IDENTITY.to_owned(),
                document_digest,
            )
            .expect_err("removals and upserts share the ordinary mutation budget")
            .reason,
            IncrementalFallbackReason::DeltaOverThreshold
        );
        let maintenance = planned(&original, &changed).map_err(|error| error.detail)?;
        assert_eq!(maintenance.removals.len(), MAX_LEXICAL_DELTA_DOCUMENTS - 1);
        assert_eq!(maintenance.upserts.len(), 2);
        assert!(
            maintenance
                .vector_upserts(IDENTITY)
                .map_err(|error| error.detail)?
                .is_empty()
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
        legacy
            .write(&directory)
            .map_err(|error| error.to_string())?;
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
        finalized_vector_rows(&index)
    }

    fn finalized_quality_vectors(directory: &Path) -> Result<BTreeMap<String, Vec<f32>>, String> {
        let index = crate::core::index::open_quality_vector_index_read_only(directory)
            .map_err(|error| error.detail)?
            .ok_or("expected a real quality tier")?;
        finalized_vector_rows(&index)
    }

    fn finalized_vector_rows(
        index: &frankensearch::VectorIndex,
    ) -> Result<BTreeMap<String, Vec<f32>>, String> {
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
            lexical.doc_count().map_err(|error| error.to_string())?,
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
            let before =
                std::fs::read(live.join("vector.fast.idx")).map_err(|error| error.to_string())?;
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
                current
                    .iter()
                    .map(|document| &document.id)
                    .collect::<Vec<_>>()
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
                std::fs::read(live.join("vector.fast.idx")).map_err(|error| error.to_string())?,
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

    fn generation_files(root: &Path) -> Result<BTreeMap<std::path::PathBuf, Vec<u8>>, String> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).map_err(|error| error.to_string())? {
                let path = entry.map_err(|error| error.to_string())?.path();
                let kind = std::fs::symlink_metadata(&path)
                    .map_err(|error| error.to_string())?
                    .file_type();
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() {
                    files.insert(
                        path.strip_prefix(root)
                            .map_err(|error| error.to_string())?
                            .to_path_buf(),
                        std::fs::read(&path).map_err(|error| error.to_string())?,
                    );
                } else {
                    return Err("unexpected special entry in native generation".to_owned());
                }
            }
        }
        Ok(files)
    }

    #[cfg(feature = "lexical-bm25")]
    async fn assert_large_lexical_corpus(
        cx: &asupersync::Cx,
        directory: &Path,
        expected: &[IndexableDocument],
    ) -> TestResult {
        use crate::search::LexicalRead;

        let lexical = crate::search::TantivyIndex::open_read_only(&directory.join("lexical"))
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lexical.doc_count().map_err(|error| error.to_string())?,
            expected.len()
        );
        let by_id: BTreeMap<_, _> = expected
            .iter()
            .map(|document| (document.id.as_str(), document))
            .collect();
        // Every planted term has a literal, independently known membership.
        // Check changed bodies, changed titles, new rows and a true negative,
        // not just the total number of documents returned by the writer.
        for query in [
            "lesson",
            "repaired",
            "updated",
            "largeimportmarker",
            "absentneedle",
        ] {
            let wanted: BTreeSet<_> = expected
                .iter()
                .filter(|document| {
                    document.content.contains(query)
                        || document
                            .title
                            .as_ref()
                            .is_some_and(|title| title.contains(query))
                })
                .map(|document| document.id.as_str())
                .collect();
            let hits = lexical
                .search(cx, query, expected.len() + 1)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                hits.len(),
                wanted.len(),
                "{query}: duplicates or missing rows"
            );
            assert_eq!(
                hits.iter()
                    .map(|hit| hit.doc_id.as_str())
                    .collect::<BTreeSet<_>>(),
                wanted,
                "{query}"
            );
            for hit in &hits {
                let document = by_id
                    .get(hit.doc_id.as_str())
                    .ok_or("unexpected lexical ID")?;
                for (key, value) in &document.metadata {
                    assert_eq!(
                        hit.metadata
                            .as_ref()
                            .and_then(|metadata| metadata.get(key.as_str()))
                            .and_then(serde_json::Value::as_str),
                        Some(value.as_str()),
                        "{}: {key}",
                        document.id
                    );
                }
            }
        }
        Ok(())
    }

    /// Exercise the production publisher, its real lease/rollback, and both
    /// native tiers. The DB supplies coordination, while the caller supplies a
    /// typed source snapshot just as the real collector does. No fake vectors
    /// or index writer can satisfy the comparisons with a fresh native build.
    fn native_large_publication_case(two_tiers: bool, fail_commit_once: bool) -> TestResult {
        use crate::core::index;
        use crate::db::{CreateWorkspaceInput, DbConnection};

        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(180), |cx| async move {
            let _root = root;
            let live = parent.join("index");
            let rebuilt = parent.join("reference");
            let db = DbConnection::open_memory().map_err(|error| error.to_string())?;
            db.migrate().map_err(|error| error.to_string())?;
            let workspace = "wsp_01234567890123456789012345";
            db.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: parent.to_string_lossy().into_owned(),
                    name: Some("large delta publication".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
            let lock = index::IndexPublishLockOwner::acquire(&cx, &db, workspace)
                .map_err(|error| error.to_string())?;
            let fast = Arc::new(CountingHash {
                inner: HashEmbedder::default_256(),
                inputs: AtomicUsize::new(0),
            });
            let quality = two_tiers.then(|| {
                Arc::new(CountingHash {
                    inner: HashEmbedder::jl_384(17),
                    inputs: AtomicUsize::new(0),
                })
            });
            let stack = crate::search::EmbedderStack::from_parts(
                fast.clone(),
                quality
                    .as_ref()
                    .map(|quality| quality.clone() as Arc<dyn Embedder>),
            );
            let mut original = documents(2048);
            for (position, document) in original.iter_mut().enumerate() {
                document
                    .metadata
                    .insert("origin".to_owned(), format!("source-{position}"));
            }
            index::publish_full_index_generation_with_stack(
                &lock,
                &live,
                stack.clone(),
                original.clone(),
                7,
                index::IndexDocumentCounts::memory_only(2048),
                || Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            assert_eq!(index::validated_index_generation(&live)?, 7);
            let original_files = generation_files(&live)?;
            let original_vectors = finalized_vectors(&live)?;
            let original_quality = two_tiers
                .then(|| finalized_quality_vectors(&live))
                .transpose()?;

            let mut current = original[32..].to_vec();
            for document in &mut current[..128] {
                document.content.push_str(" repaired");
            }
            retitle(&mut current[128..256]);
            current.extend((2048..2432).map(|number| {
                let mut document = IndexableDocument::new(
                    format!("mem_{number:05}"),
                    format!("lesson {number} largeimportmarker"),
                );
                document
                    .metadata
                    .insert("origin".to_owned(), format!("new-{number}"));
                document
            }));
            assert_eq!(current.len(), 2400);
            let count = index::IndexDocumentCounts::memory_only(2400);
            let expected_inputs = 512;
            let identity = embedder_identity(&stack).ok_or("complete stack identity")?;
            let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
            assert_eq!(delta.upserts.len(), 640);
            assert_eq!(delta.removals.len(), 32);
            assert_eq!(
                delta
                    .vector_upserts(&identity)
                    .map_err(|error| error.detail)?
                    .len(),
                expected_inputs
            );

            let reset_counts = || {
                fast.inputs.store(0, Ordering::SeqCst);
                if let Some(quality) = &quality {
                    quality.inputs.store(0, Ordering::SeqCst);
                }
            };
            let assert_counts = |expected| {
                assert_eq!(fast.inputs.load(Ordering::SeqCst), expected);
                if let Some(quality) = &quality {
                    assert_eq!(quality.inputs.load(Ordering::SeqCst), expected);
                }
            };
            let mut commit_calls = 0;
            if fail_commit_once {
                reset_counts();
                let failed = index::publish_index_generation_preferring_delta(
                    &lock,
                    &live,
                    stack.clone(),
                    current.clone(),
                    8,
                    count,
                    || {
                        commit_calls += 1;
                        Err(index::IndexRebuildError::Index(
                            "injected commit refusal".to_owned(),
                        ))
                    },
                )
                .await;
                assert!(failed.is_err());
                assert_eq!(
                    commit_calls, 1,
                    "the complete staged delta must reach the commit tail"
                );
                assert_counts(expected_inputs);
                assert_eq!(index::validated_index_generation(&live)?, 7);
                assert_eq!(generation_files(&live)?, original_files);
                assert_eq!(finalized_vectors(&live)?, original_vectors);
                if let Some(expected) = &original_quality {
                    assert_eq!(&finalized_quality_vectors(&live)?, expected);
                }
                #[cfg(feature = "lexical-bm25")]
                assert_large_lexical_corpus(&cx, &live, &original).await?;
            }

            reset_counts();
            let (_, kind) = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                stack.clone(),
                current.clone(),
                8,
                count,
                || {
                    commit_calls += 1;
                    Ok(())
                },
            )
            .await
            .map_err(|error| error.to_string())?;
            assert!(matches!(kind, index::PublishedGenerationKind::Delta));
            assert_eq!(commit_calls, 1 + usize::from(fail_commit_once));
            assert_counts(expected_inputs);
            assert_eq!(index::validated_index_generation(&live)?, 8);
            index::verify_published_tier_counts(&live, 2400, two_tiers)?;
            let actual = finalized_vectors(&live)?;
            let actual_quality = two_tiers
                .then(|| finalized_quality_vectors(&live))
                .transpose()?;
            #[cfg(feature = "lexical-bm25")]
            assert_large_lexical_corpus(&cx, &live, &current).await?;

            // Reload the PUBLISHED digest map. A retry must not infer these
            // inputs again or resurrect any withdrawn document identity.
            let replay = plan(&live, &stack, &current).map_err(|error| error.detail)?;
            assert!(replay.upserts.is_empty() && replay.removals.is_empty());
            reset_counts();
            let (_, kind) = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                stack.clone(),
                current.clone(),
                9,
                count,
                || Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            assert!(matches!(kind, index::PublishedGenerationKind::Delta));
            assert_counts(0);
            assert_eq!(index::validated_index_generation(&live)?, 9);
            assert_eq!(finalized_vectors(&live)?, actual);
            if let Some(expected) = &actual_quality {
                assert_eq!(&finalized_quality_vectors(&live)?, expected);
            }

            let builder = index::IndexBuilder::new(&rebuilt).with_embedder_stack(stack.clone());
            #[cfg(feature = "lexical-bm25")]
            let builder = builder.with_tantivy_lexical();
            builder
                .add_documents(current.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(actual, finalized_vectors(&rebuilt)?);
            if let Some(expected) = actual_quality {
                assert_eq!(expected, finalized_quality_vectors(&rebuilt)?);
            }
            #[cfg(feature = "lexical-bm25")]
            assert_large_lexical_corpus(&cx, &rebuilt, &current).await?;
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn native_large_delta_publishes_without_reembedding_unchanged_documents() -> TestResult {
        native_large_publication_case(false, false)
    }

    #[test]
    fn native_large_delta_preserves_both_vector_tiers_and_lexical_membership() -> TestResult {
        native_large_publication_case(true, false)
    }

    #[test]
    fn native_large_delta_commit_failure_restores_the_live_generation_then_retries() -> TestResult {
        native_large_publication_case(true, true)
    }
}
