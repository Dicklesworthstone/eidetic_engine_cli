//! Reconcile a complete canonical source stream without retaining unchanged bodies.
//!
//! Ordinary changes, bulk growth and rolling maintenance share the full V3
//! planner's existing mutation/inference bounds. Only refused snapshots return
//! to the full collector; accepted bulk imports never retain unchanged bodies.
//! Every source document is still read and hashed; only body residency and
//! repeated whole-corpus hashing are reduced. No job list proves membership.

use std::collections::BTreeMap;
use std::path::Path;

use crate::search::IndexableDocument;

use super::{
    DOC_DIGESTS_SCHEMA_V3, Delta, DocDigests, EmbedderStack, EmbeddingReuse, IncrementalFallback,
    IncrementalFallbackReason, MAX_DELTA_PERCENT, batch, document_digest, embedder_identity,
    embedding_input_digest, incremental_fallback, metadata::document_payload_bytes, read_digests,
};

#[path = "index_delta_stream_budget.rs"]
mod budget;

const MAX_STREAM_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

pub(in super::super) struct SourceReconciler {
    request: Option<asupersync::Cx>,
    identity: String,
    live_count: usize,
    // Consume old identities as they are observed. The remaining keys are
    // removals, so unchanged rows need neither a second membership set nor
    // simultaneous old and new digest entries for the complete corpus.
    live_documents: BTreeMap<String, String>,
    live_inputs: BTreeMap<String, String>,
    current_documents: BTreeMap<String, String>,
    current_inputs: BTreeMap<String, String>,
    upserts: Vec<IndexableDocument>,
    reusable_inputs: BTreeMap<String, String>,
    inference_inputs: usize,
    new_identities: usize,
    payload_bytes: usize,
    refused: Option<IncrementalFallback>,
}

#[derive(Debug)]
pub(in super::super) struct ReconciledSource {
    pub(in super::super) delta: Delta,
    pub(in super::super) digests: DocDigests,
    pub(in super::super) documents_total: usize,
}

impl SourceReconciler {
    pub(in super::super) fn read(
        index_dir: &Path,
        stack: &EmbedderStack,
    ) -> Result<Self, IncrementalFallback> {
        let request = asupersync::Cx::current();
        checkpoint(request.as_ref())?;
        let identity = embedder_identity(stack).ok_or_else(|| {
            invalid("streaming source reconciliation requires a complete embedding identity")
        })?;
        let live = read_digests(index_dir)?;
        checkpoint(request.as_ref())?;
        if live.schema != DOC_DIGESTS_SCHEMA_V3
            || live.embedder.as_deref() != Some(identity.as_str())
        {
            return Err(invalid(
                "streaming source reconciliation requires matching v3 embedding identities",
            ));
        }
        let inputs = live
            .embedding_inputs
            .ok_or_else(|| invalid("streaming source reconciliation requires input digests"))?;
        if live.documents.len() != inputs.len() {
            return Err(invalid(
                "streaming source digest maps have different identities",
            ));
        }
        for ((id, digest), (input_id, input)) in live.documents.iter().zip(&inputs) {
            checkpoint(request.as_ref())?;
            if id.is_empty() || id != input_id || !valid_digest(digest) || !valid_digest(input) {
                return Err(invalid("streaming source digest maps are malformed"));
            }
        }
        Ok(Self {
            request,
            identity,
            live_count: live.documents.len(),
            live_documents: live.documents,
            live_inputs: inputs,
            current_documents: BTreeMap::new(),
            current_inputs: BTreeMap::new(),
            upserts: Vec::new(),
            reusable_inputs: BTreeMap::new(),
            inference_inputs: 0,
            new_identities: 0,
            payload_bytes: 0,
            refused: None,
        })
    }

    /// The caller must supply the complete admitted corpus in one snapshot.
    /// An error poisons the collector: ignoring a refused row cannot authorize
    /// publication of an incomplete prefix or manufacture a removal.
    pub(in super::super) fn observe(
        &mut self,
        document: IndexableDocument,
    ) -> Result<(), IncrementalFallback> {
        if let Some(error) = &self.refused {
            return Err(copy_refusal(error));
        }
        let result = self.observe_inner(document);
        if let Err(error) = &result {
            self.refused = Some(copy_refusal(error));
            self.upserts.clear();
            self.reusable_inputs.clear();
        }
        result
    }

    fn observe_inner(&mut self, document: IndexableDocument) -> Result<(), IncrementalFallback> {
        checkpoint(self.request.as_ref())?;
        if document.id.is_empty() || self.current_documents.contains_key(&document.id) {
            return Err(invalid(
                "source snapshot contains an empty or duplicate document identity",
            ));
        }
        let digest = document_digest(&document);
        checkpoint(self.request.as_ref())?;
        let input = embedding_input_digest(&document);
        checkpoint(self.request.as_ref())?;
        let previous = self.live_documents.get(&document.id);
        let previous_input = self.live_inputs.get(&document.id);
        let changed = previous != Some(&digest);
        let reusable = previous.is_some() && previous_input == Some(&input);
        if previous.is_some() && !changed && !reusable {
            return Err(invalid(
                "unchanged source document has an inconsistent live embedding-input digest",
            ));
        }
        if changed {
            if self.upserts.len() == budget::MAX_MUTATIONS {
                return Err(over_limit(
                    "streaming source delta exceeds the mutation budget",
                ));
            }
            let bytes = document_payload_bytes(&document);
            if bytes > MAX_STREAM_PAYLOAD_BYTES.saturating_sub(self.payload_bytes) {
                return Err(over_limit(
                    "streaming source delta exceeds the payload byte budget",
                ));
            }
            self.payload_bytes += bytes;
            self.inference_inputs += usize::from(!reusable);
            self.new_identities += usize::from(previous.is_none());
        }
        self.live_documents.remove(&document.id);
        self.live_inputs.remove(&document.id);
        self.current_documents.insert(document.id.clone(), digest);
        self.current_inputs
            .insert(document.id.clone(), input.clone());
        if changed {
            if reusable {
                self.reusable_inputs.insert(document.id.clone(), input);
            }
            self.upserts.push(document);
        }
        Ok(())
    }

    pub(in super::super) fn finish(mut self) -> Result<ReconciledSource, IncrementalFallback> {
        if let Some(error) = self.refused.take() {
            return Err(error);
        }
        checkpoint(self.request.as_ref())?;
        // Apply the full planner's ordinary/growth/maintenance policy only
        // after the complete source reveals net size and all withdrawals.
        let documents_total = self.current_documents.len();
        budget::validate(
            self.live_count,
            documents_total,
            self.upserts.len(),
            self.live_documents.len(),
            self.inference_inputs,
            self.new_identities,
        )?;
        let mut removals = Vec::with_capacity(self.live_documents.len());
        for id in self.live_documents.into_keys() {
            checkpoint(self.request.as_ref())?;
            if id.len() > MAX_STREAM_PAYLOAD_BYTES.saturating_sub(self.payload_bytes) {
                return Err(over_limit(
                    "streaming source delta exceeds the payload byte budget",
                ));
            }
            self.payload_bytes += id.len();
            removals.push(id);
        }
        checkpoint(self.request.as_ref())?;
        Ok(ReconciledSource {
            delta: Delta {
                upserts: self.upserts,
                removals,
                reuse: Some(EmbeddingReuse {
                    identity: self.identity.clone(),
                    inputs: self.reusable_inputs,
                }),
            },
            digests: DocDigests {
                schema: DOC_DIGESTS_SCHEMA_V3.to_owned(),
                embedder: Some(self.identity),
                documents: self.current_documents,
                embedding_inputs: Some(self.current_inputs),
            },
            documents_total,
        })
    }
}

fn checkpoint(request: Option<&asupersync::Cx>) -> Result<(), IncrementalFallback> {
    if let Some(cx) = request {
        batch::checkpoint(cx, "streaming source reconciliation")?;
    }
    Ok(())
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn invalid(detail: &str) -> IncrementalFallback {
    incremental_fallback(IncrementalFallbackReason::CorpusRevisionMismatch, detail)
}

fn over_limit(detail: &str) -> IncrementalFallback {
    incremental_fallback(IncrementalFallbackReason::DeltaOverThreshold, detail)
}

fn copy_refusal(error: &IncrementalFallback) -> IncrementalFallback {
    incremental_fallback(error.reason, error.detail.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::HashEmbedder;
    use std::sync::Arc;

    pub(super) type TestResult = Result<(), String>;

    pub(super) fn test_stack(seed: u64) -> EmbedderStack {
        EmbedderStack::from_parts(Arc::new(HashEmbedder::jl_384(seed)), None)
    }

    pub(super) fn documents(count: usize) -> Vec<IndexableDocument> {
        (0..count)
            .map(|number| {
                IndexableDocument::new(format!("mem_{number:05}"), format!("lesson {number}"))
            })
            .collect()
    }

    pub(super) fn generation(
        stack: &EmbedderStack,
        documents: &[IndexableDocument],
    ) -> Result<tempfile::TempDir, String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        DocDigests::new(stack, documents)
            .write(root.path())
            .map_err(|error| error.to_string())?;
        Ok(root)
    }

    pub(super) fn reconcile(
        root: &Path,
        stack: &EmbedderStack,
        documents: &[IndexableDocument],
    ) -> Result<ReconciledSource, IncrementalFallback> {
        let mut stream = SourceReconciler::read(root, stack)?;
        for document in documents {
            stream.observe(document.clone())?;
        }
        stream.finish()
    }

    pub(super) fn same_as_full_plan(
        root: &Path,
        stack: &EmbedderStack,
        current: &[IndexableDocument],
    ) -> TestResult {
        let actual = reconcile(root, stack, current).map_err(|error| error.detail)?;
        let expected = super::super::plan(root, stack, current).map_err(|error| error.detail)?;
        let identity = embedder_identity(stack).ok_or("missing producer")?;
        let ids = |documents: &[IndexableDocument]| {
            documents
                .iter()
                .map(|document| document.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&actual.delta.upserts), ids(&expected.upserts));
        assert_eq!(
            super::super::digest_map(&actual.delta.upserts),
            super::super::digest_map(&expected.upserts)
        );
        assert_eq!(actual.delta.removals, expected.removals);
        assert_eq!(
            ids(&actual
                .delta
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?),
            ids(&expected
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?)
        );
        let next = DocDigests::new(stack, current);
        assert_eq!(actual.digests.documents, next.documents);
        assert_eq!(actual.digests.embedding_inputs, next.embedding_inputs);
        assert_eq!(actual.digests.embedder, next.embedder);
        assert_eq!(actual.documents_total, current.len());
        Ok(())
    }

    #[test]
    fn streamed_mixed_changes_match_full_planning_and_complete_next_digests() -> TestResult {
        let stack = test_stack(11);
        let original = documents(32);
        let root = generation(&stack, &original)?;
        let mut current = original[..30].to_vec();
        current[0].content.push_str(" corrected");
        current[1].title = Some("updated title".to_owned());
        current[2]
            .metadata
            .insert("provenance".to_owned(), "revision-two".to_owned());
        current.push(IndexableDocument::new(
            "mem_new",
            original[3].content.clone(),
        ));
        current.rotate_left(7);
        same_as_full_plan(root.path(), &stack, &current)?;
        same_as_full_plan(root.path(), &stack, &[])?;

        let mut actual = reconcile(root.path(), &stack, &current).map_err(|error| error.detail)?;
        let identity = embedder_identity(&stack).ok_or("missing identity")?;
        let vector_ids = actual
            .delta
            .vector_upserts(&identity)
            .map_err(|error| error.detail)?
            .iter()
            .map(|document| document.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(vector_ids, ["mem_new", "mem_00000"]);
        // The existing apply-time commitments must still reject reuse after a
        // caller changes a planned input or withdraws and re-adds its identity.
        for document in &mut actual.delta.upserts {
            if document.id == "mem_00001" {
                document.content.push_str(" changed after planning");
            }
        }
        actual.delta.removals.push("mem_00002".to_owned());
        assert_eq!(
            actual
                .delta
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?
                .len(),
            actual.delta.upserts.len()
        );
        assert!(actual.delta.vector_upserts("different-producer").is_err());
        Ok(())
    }

    #[test]
    fn unchanged_stream_releases_bodies_and_consumes_old_identity_maps() -> TestResult {
        let stack = test_stack(11);
        let original = documents(512)
            .into_iter()
            .map(|mut document| {
                document
                    .content
                    .push_str(&" prior incident detail".repeat(256));
                document
            })
            .collect::<Vec<_>>();
        let root = generation(&stack, &original)?;
        let mut stream =
            SourceReconciler::read(root.path(), &stack).map_err(|error| error.detail)?;
        for (position, document) in original.iter().enumerate() {
            stream
                .observe(document.clone())
                .map_err(|error| error.detail)?;
            assert!(stream.upserts.is_empty());
            assert!(stream.reusable_inputs.is_empty());
            assert_eq!(stream.payload_bytes, 0);
            assert_eq!(stream.current_documents.len(), position + 1);
            assert_eq!(stream.live_documents.len(), original.len() - position - 1);
        }
        let result = stream.finish().map_err(|error| error.detail)?;
        assert_eq!(result.documents_total, original.len());
        assert_eq!(
            result.digests.documents,
            super::super::digest_map(&original)
        );
        assert!(result.delta.upserts.is_empty());
        assert!(result.delta.removals.is_empty());
        Ok(())
    }

    #[test]
    fn metadata_and_removals_no_longer_fall_back_at_the_old_stream_cap() -> TestResult {
        let stack = test_stack(11);
        let original = documents(1024);
        let root = generation(&stack, &original)?;
        for mutations in [256, 257, 1024] {
            let mut current = original.clone();
            for document in &mut current[..mutations] {
                document.title = Some("new title without new embedding input".to_owned());
            }
            same_as_full_plan(root.path(), &stack, &current)?;
            same_as_full_plan(root.path(), &stack, &original[..original.len() - mutations])?;
        }
        Ok(())
    }

    #[test]
    fn inference_allowance_uses_final_source_size_and_never_charges_metadata() -> TestResult {
        let stack = test_stack(11);
        let original = documents(7);
        let root = generation(&stack, &original)?;
        let mut current = original.clone();
        for document in &mut current {
            document.title = Some("metadata refresh".to_owned());
        }
        current.extend([
            IndexableDocument::new("mem_new_a", "first new input"),
            IndexableDocument::new("mem_new_b", "second new input"),
        ]);
        // The final nine-row source allows two vectors; the old seven rows
        // alone allow only one. New inputs may arrive before all survivors.
        current.rotate_right(2);
        same_as_full_plan(root.path(), &stack, &current)?;
        current[2].content.push_str(" rewritten");
        // The bulk-growth policy can admit the first old rewrite plus both
        // new IDs; a SECOND old rewrite exceeds its old-corpus allowance.
        same_as_full_plan(root.path(), &stack, &current)?;
        current[3].content.push_str(" another rewrite");
        assert!(super::super::plan(root.path(), &stack, &current).is_err());
        let error = reconcile(root.path(), &stack, &current)
            .err()
            .ok_or("old rewrites must retain their bulk-growth percentage bound")?;
        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
        Ok(())
    }

    #[test]
    fn payload_limit_counts_utf8_metadata_and_withdrawal_ids() -> TestResult {
        let stack = test_stack(11);
        let original = [
            IndexableDocument::new("a", "previous input"),
            IndexableDocument::new("b", "withdraw this document"),
        ];
        let root = generation(&stack, &original)?;
        for extra_byte in [0, 1] {
            let mut document = IndexableDocument::new("a", "");
            document.title = Some("résumé".to_owned());
            document
                .metadata
                .insert("scope".to_owned(), "世界".to_owned());
            let overhead = document_payload_bytes(&document);
            document.content =
                "x".repeat(MAX_STREAM_PAYLOAD_BYTES - overhead - "b".len() + extra_byte);
            let mut stream =
                SourceReconciler::read(root.path(), &stack).map_err(|error| error.detail)?;
            stream.observe(document).map_err(|error| error.detail)?;
            let result = stream.finish();
            if extra_byte == 0 {
                let result = result.map_err(|error| error.detail)?;
                assert_eq!(result.delta.upserts.len(), 1);
                assert_eq!(result.delta.removals, ["b"]);
                assert_eq!(result.documents_total, 1);
            } else {
                let error = result
                    .err()
                    .ok_or("withdrawal ID must spend the final byte")?;
                assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
            }
        }
        Ok(())
    }

    #[test]
    fn duplicate_and_cancelled_observations_cannot_publish_an_accepted_prefix() -> TestResult {
        let stack = test_stack(11);
        let original = documents(8);
        let root = generation(&stack, &original)?;
        let mut changed = original[0].clone();
        changed.content.push_str(" changed source input");
        let mut stream =
            SourceReconciler::read(root.path(), &stack).map_err(|error| error.detail)?;
        stream
            .observe(changed.clone())
            .map_err(|error| error.detail)?;
        assert_eq!(stream.upserts.len(), 1);
        assert!(stream.observe(original[0].clone()).is_err());
        assert!(stream.upserts.is_empty());
        assert!(stream.observe(original[1].clone()).is_err());
        assert!(stream.finish().is_err());

        let mut stream =
            SourceReconciler::read(root.path(), &stack).map_err(|error| error.detail)?;
        stream.observe(changed).map_err(|error| error.detail)?;
        assert_eq!(stream.upserts.len(), 1);
        let cx = asupersync::Cx::for_testing();
        cx.set_cancel_reason(asupersync::CancelReason::user("cancel source stream"));
        stream.request = Some(cx);
        assert!(stream.observe(original[1].clone()).is_err());
        assert!(stream.upserts.is_empty());
        assert!(stream.finish().is_err());
        Ok(())
    }

    #[test]
    fn streaming_requires_v3_complete_producer_and_consistent_digest_maps() -> TestResult {
        let stack = test_stack(11);
        let original = documents(8);
        let root = generation(&stack, &original)?;
        assert!(SourceReconciler::read(root.path(), &test_stack(29)).is_err());
        for mutation in 0..6 {
            let mut digests = DocDigests::new(&stack, &original);
            match mutation {
                0 => {
                    digests.schema = super::super::DOC_DIGESTS_SCHEMA_V2.to_owned();
                    digests.embedding_inputs = None;
                }
                1 => digests.embedder = None,
                2 => {
                    digests
                        .documents
                        .insert(original[0].id.clone(), "invalid".to_owned());
                }
                3 => {
                    digests
                        .embedding_inputs
                        .as_mut()
                        .ok_or("inputs missing")?
                        .remove(&original[0].id);
                }
                4 => {
                    digests
                        .embedding_inputs
                        .as_mut()
                        .ok_or("inputs missing")?
                        .insert(original[0].id.clone(), "A".repeat(64));
                }
                _ => {
                    digests
                        .embedding_inputs
                        .as_mut()
                        .ok_or("inputs missing")?
                        .insert(original[0].id.clone(), "0".repeat(64));
                }
            }
            digests
                .write(root.path())
                .map_err(|error| error.to_string())?;
            let result = SourceReconciler::read(root.path(), &stack);
            if mutation == 5 {
                let mut stream = result.map_err(|error| error.detail)?;
                assert!(stream.observe(original[0].clone()).is_err());
                assert!(stream.finish().is_err());
            } else {
                assert!(
                    result.is_err(),
                    "malformed baseline {mutation} was accepted"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "index_delta_stream_bulk_tests.rs"]
mod bulk_tests;
