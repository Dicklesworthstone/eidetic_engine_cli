//! Bounded embedding and WAL batches for a private delta generation.
//!
//! Coalescing source writes must also coalesce inference and durable vector
//! appends. One inference call and one WAL batch cover each bounded group;
//! every output's identity and the complete response cardinality are checked
//! before any vector in that group is passed to the writer.

use frankensearch::VectorIndex;
use frankensearch::core::traits::IdentityBoundEmbedding;

use crate::search::{Embedder, IndexableDocument};

use super::{IncrementalFallback, IncrementalFallbackReason, incremental_fallback};

const MAX_BATCH_DOCUMENTS: usize = 32;
const MAX_BATCH_INPUT_BYTES: usize = 256 * 1024;
const MAX_BATCH_VECTOR_BYTES: usize = 1024 * 1024;

pub(super) fn checkpoint(cx: &asupersync::Cx, tier: &str) -> Result<(), IncrementalFallback> {
    cx.checkpoint()
        .map_err(|error| tier_error(format!("{tier}-tier delta interrupted: {error}")))
}

fn tier_error(detail: String) -> IncrementalFallback {
    incremental_fallback(IncrementalFallbackReason::TierUnavailable, detail)
}

/// Bound documents, input bytes, and expected f32 values independently. A
/// single over-budget document is sent alone, unchanged, so batching never
/// truncates evidence or prevents progress. This is not a quota on a provider's
/// internal allocations, its identity metadata, or its wire encoding.
fn batch_len(documents: &[IndexableDocument], dimension: usize) -> usize {
    let vector_bytes = dimension.saturating_mul(std::mem::size_of::<f32>());
    let mut input_bytes = 0_usize;
    let mut count = 0_usize;
    for document in documents.iter().take(MAX_BATCH_DOCUMENTS) {
        let next_input = input_bytes.saturating_add(document.content.len());
        let next_vectors = vector_bytes.saturating_mul(count + 1);
        if count > 0
            && (next_input > MAX_BATCH_INPUT_BYTES || next_vectors > MAX_BATCH_VECTOR_BYTES)
        {
            break;
        }
        input_bytes = next_input;
        count += 1;
    }
    count
}

fn validate_batch(
    vectors: &[IdentityBoundEmbedding],
    expected_count: usize,
    expected_identity: &str,
    tier: &str,
) -> Result<(), IncrementalFallback> {
    if vectors.len() != expected_count {
        return Err(tier_error(format!(
            "{tier}-tier embedding batch returned {} vectors for {expected_count} documents",
            vectors.len(),
        )));
    }
    for vector in vectors {
        super::validate_bound_embedding(vector, expected_identity, tier)?;
    }
    Ok(())
}

pub(super) async fn upsert(
    cx: &asupersync::Cx,
    index: &mut VectorIndex,
    embedder: &dyn Embedder,
    documents: &[IndexableDocument],
    expected_identity: &str,
    tier: &str,
) -> Result<(), IncrementalFallback> {
    embed_batches_with(
        cx,
        embedder,
        documents,
        expected_identity,
        tier,
        |entries| {
            // Frankensearch validates all values before writing this one atomic
            // WAL batch. Do not replace this with append() inside a document loop.
            index.append_batch(&entries).map_err(|error| {
                tier_error(format!("{tier}-tier vector batch upsert failed: {error}"))
            })
        },
    )
    .await
}

async fn embed_batches_with(
    cx: &asupersync::Cx,
    embedder: &dyn Embedder,
    mut documents: &[IndexableDocument],
    expected_identity: &str,
    tier: &str,
    mut write: impl FnMut(Vec<(String, Vec<f32>)>) -> Result<(), IncrementalFallback>,
) -> Result<(), IncrementalFallback> {
    while !documents.is_empty() {
        checkpoint(cx, tier)?;
        let count = batch_len(documents, embedder.dimension());
        let (batch, remainder) = documents.split_at(count);
        let texts: Vec<_> = batch
            .iter()
            .map(|document| document.content.as_str())
            .collect();
        let vectors = embedder
            .embed_batch_bound(cx, &texts)
            .await
            .map_err(|error| tier_error(format!("{tier}-tier batch embedding failed: {error}")))?;
        // Cancellation during inference must be seen before a WAL write even
        // when a custom embedder returns Ok without checking its caller Cx.
        checkpoint(cx, tier)?;
        validate_batch(&vectors, batch.len(), expected_identity, tier)?;
        let entries = batch
            .iter()
            .zip(vectors)
            .map(|(document, vector)| (document.id.clone(), vector.values))
            .collect();
        write(entries)?;
        documents = remainder;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::HashEmbedder;
    use frankensearch::core::generation::EmbeddingIdentityBundleV1;
    use frankensearch::core::traits::{ModelCategory, SearchFuture};
    use std::sync::Mutex;
    use std::time::Duration;

    type TestResult = Result<(), String>;

    // Real hash inference and identities, with invocation sizes observed at
    // the trait boundary. No fake vectors or mock index implementation.
    struct ObservedHash {
        inner: HashEmbedder,
        sizes: Mutex<Vec<usize>>,
    }

    impl ObservedHash {
        fn new() -> Self {
            Self {
                inner: HashEmbedder::default_256(),
                sizes: Mutex::new(Vec::new()),
            }
        }
    }

    impl Embedder for ObservedHash {
        fn embed<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            text: &'a str,
        ) -> SearchFuture<'a, Vec<f32>> {
            self.inner.embed(cx, text)
        }

        fn embed_batch<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            texts: &'a [&'a str],
        ) -> SearchFuture<'a, Vec<Vec<f32>>> {
            self.sizes
                .lock()
                .expect("batch observations")
                .push(texts.len());
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

    fn documents(count: usize) -> Vec<IndexableDocument> {
        (0..count)
            .map(|index| {
                IndexableDocument::new(
                    format!("mem_batch_{index:03}"),
                    format!("lesson number {index}"),
                )
            })
            .collect()
    }

    #[test]
    fn batch_bounds_count_bytes_and_dimensions_without_dropping_oversized_documents() {
        assert_eq!(batch_len(&[], 256), 0);
        let docs = documents(65);
        assert_eq!(batch_len(&docs, 256), 32);
        assert_eq!(batch_len(&docs, MAX_BATCH_VECTOR_BYTES / 4), 1);
        assert_eq!(batch_len(&docs, usize::MAX), 1);
        let mut large = documents(3);
        large[0].content = "é".repeat(MAX_BATCH_INPUT_BYTES / 4);
        large[1].content = large[0].content.clone();
        assert_eq!(
            batch_len(&large, 256),
            2,
            "count UTF-8 bytes, not characters"
        );
        large[0].content.push('x');
        assert_eq!(batch_len(&large, 256), 1);
        large[0].content = "x".repeat(MAX_BATCH_INPUT_BYTES + 1);
        assert_eq!(batch_len(&large, 256), 1);
        assert_eq!(
            batch_len(&large[1..], 256),
            2,
            "the next batch still advances"
        );
    }

    #[test]
    fn batching_matches_real_per_document_inference_and_preserves_every_id() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let expected_identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let docs = documents(65);
            let expected: Vec<_> = docs
                .iter()
                .map(|document| {
                    (
                        document.id.clone(),
                        embedder.inner.embed_sync(&document.content),
                    )
                })
                .collect();
            let mut written = Vec::new();
            let mut writes = 0;
            embed_batches_with(
                &cx,
                &embedder,
                &docs,
                &expected_identity,
                "fast",
                |entries| {
                    writes += 1;
                    written.extend(entries);
                    Ok(())
                },
            )
            .await
            .map_err(|error| error.detail)?;
            assert_eq!(written, expected);
            assert_eq!(writes, 3);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![32, 32, 1]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn empty_delta_never_calls_inference_or_the_writer() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            embed_batches_with(&cx, &embedder, &[], "unused", "fast", |_| {
                panic!("an empty delta must not write a WAL batch")
            })
            .await
            .map_err(|error| error.detail)?;
            assert!(embedder.sizes.lock().expect("sizes").is_empty());
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cardinality_and_every_identity_are_checked_before_a_batch_is_admitted() -> TestResult {
        let hash = HashEmbedder::default_256();
        let identity = hash.identity().map_err(|error| error.to_string())?.clone();
        let fingerprint = identity.fingerprint();
        let good = IdentityBoundEmbedding {
            values: hash.embed_sync("actual input"),
            identity,
        };
        for count in [0, 1, 3] {
            assert!(validate_batch(&vec![good.clone(); count], 2, &fingerprint, "fast").is_err());
        }
        assert!(validate_batch(&[good.clone(), good.clone()], 2, &fingerprint, "fast").is_ok());
        let other = HashEmbedder::jl_384(11);
        let foreign = IdentityBoundEmbedding {
            values: other.embed_sync("actual input"),
            identity: other.identity().map_err(|error| error.to_string())?.clone(),
        };
        assert!(validate_batch(&[good.clone(), foreign], 2, &fingerprint, "quality").is_err());
        let mut short = good.clone();
        let _ = short.values.pop();
        assert!(validate_batch(&[good, short], 2, &fingerprint, "fast").is_err());
        Ok(())
    }

    #[test]
    fn a_failed_wal_batch_stops_before_the_next_inference_request() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let result =
                embed_batches_with(&cx, &embedder, &documents(65), &identity, "fast", |_| {
                    Err(tier_error("writer refused".to_owned()))
                })
                .await;
            assert_eq!(result.expect_err("writer failure").detail, "writer refused");
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![32]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn mismatched_producer_never_reaches_the_batch_writer() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let foreign = HashEmbedder::jl_384(11);
            let identity = foreign
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let result =
                embed_batches_with(&cx, &embedder, &documents(3), &identity, "quality", |_| {
                    panic!("an incompatible producer must be refused before writing")
                })
                .await;
            assert_eq!(
                result.expect_err("producer drift").reason,
                IncrementalFallbackReason::CorpusRevisionMismatch
            );
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![3]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn staged_batch_updates_match_a_full_build_and_leave_the_live_index_unchanged() -> TestResult {
        use super::super::super::{
            IndexBuilder, compact_incremental_vector_index, hash_fallback_embedder_stack,
            open_fast_vector_index,
        };
        use std::collections::BTreeMap;

        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            let live = parent.join("live");
            let staging = parent.join("staging");
            let rebuilt = parent.join("rebuilt");
            let original = documents(70);
            let stack = hash_fallback_embedder_stack();
            IndexBuilder::new(&live)
                .with_embedder_stack(stack.clone())
                .add_documents(original.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            std::fs::create_dir(&staging).map_err(|error| error.to_string())?;
            super::super::copy_generation(&live, &staging).map_err(|error| error.to_string())?;
            let before = std::fs::read_dir(&live)
                .map_err(|error| error.to_string())?
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_file())
                .map(|entry| {
                    Ok((
                        entry.file_name(),
                        std::fs::read(entry.path()).map_err(|error| error.to_string())?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            let mut updated = original.clone();
            for document in &mut updated[..65] {
                document.content.push_str(" revised");
            }
            updated.truncate(68);
            let mut index = open_fast_vector_index(&staging).map_err(|error| error.detail)?;
            let removed = [&original[68].id[..], &original[69].id[..]];
            assert_eq!(
                index
                    .soft_delete_batch(&removed)
                    .map_err(|error| error.to_string())?,
                2
            );
            super::super::super::vacuum_incremental_vector_index(&mut index, "fast")
                .map_err(|error| error.detail)?;
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            upsert(
                &cx,
                &mut index,
                &embedder,
                &updated[..65],
                &identity,
                "fast",
            )
            .await
            .map_err(|error| error.detail)?;
            compact_incremental_vector_index(&mut index, "fast").map_err(|error| error.detail)?;
            drop(index);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![32, 32, 1]);
            IndexBuilder::new(&rebuilt)
                .with_embedder_stack(stack)
                .add_documents(updated)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            let read_vectors =
                |path: &std::path::Path| -> Result<BTreeMap<String, Vec<f32>>, String> {
                    let index = super::super::super::open_fast_vector_index_read_only(path)
                        .map_err(|error| error.detail)?;
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
                };
            assert_eq!(read_vectors(&staging)?, read_vectors(&rebuilt)?);
            assert_eq!(read_vectors(&live)?.len(), 70);
            for (name, bytes) in before {
                assert_eq!(
                    std::fs::read(live.join(name)).map_err(|error| error.to_string())?,
                    bytes
                );
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }
}
