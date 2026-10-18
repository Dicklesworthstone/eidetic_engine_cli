//! Bounded embedding and WAL batches for a private delta generation.
//!
//! Coalescing source writes must also coalesce inference and durable vector
//! appends. Each bounded group writes every document, but only byte-distinct
//! uncached inputs need inference. Every fresh output's identity and the whole
//! response cardinality are checked before any vector reaches the writer.
//! Batch equivalence is a provider capability, not a property of Vec outputs:
//! match IndexBuilder's bound single/batch dispatch on every cache miss.

use std::collections::{BTreeMap, btree_map::Entry};

use frankensearch::VectorIndex;
use frankensearch::core::traits::IdentityBoundEmbedding;

use crate::search::{Embedder, IndexableDocument};

use super::{IncrementalFallback, IncrementalFallbackReason, incremental_fallback};

const MAX_BATCH_DOCUMENTS: usize = 32;
const MAX_BATCH_INPUT_BYTES: usize = 256 * 1024;
const MAX_BATCH_VECTOR_BYTES: usize = 1024 * 1024;
const MAX_CACHED_INPUTS: usize = 128;
const MAX_CACHED_VECTOR_BYTES: usize = 1024 * 1024;

/// Reuse is confined to one tier, expected identity and upsert invocation.
/// Keys borrow the caller's existing inputs; neither source text nor identity
/// metadata is copied into the cache. Only validated vectors may enter it;
/// boxed slices discard a provider's excess Vec capacity before retention.
/// A full cache evicts its least recently used inputs, so repeated evidence
/// arriving late in a large delta can still reuse inference. Oversized vectors
/// bypass retention without evicting useful inputs. Misses always use normal
/// bounded inference; eviction never changes a document or its WAL ordering.
/// This is not a persistent cache or a way around the planner's delta budget.
#[derive(Default)]
struct ValidatedInputCache<'a> {
    vectors: BTreeMap<&'a str, CachedInput>,
    vector_bytes: usize,
    clock: u64,
}

struct CachedInput {
    values: Box<[f32]>,
    last_used: u64,
}

impl<'a> ValidatedInputCache<'a> {
    fn get(&mut self, text: &str) -> Option<&[f32]> {
        let now = self.next_tick();
        let input = self.vectors.get_mut(text)?;
        input.last_used = now;
        Some(input.values.as_ref())
    }

    fn remember(&mut self, text: &'a str, values: Vec<f32>) {
        // Touch an existing entry without replacing its validated values or
        // disturbing byte accounting, even if a later supplied Vec differs.
        if self.get(text).is_some() {
            return;
        }
        let bytes = values.len().saturating_mul(std::mem::size_of::<f32>());
        if bytes > MAX_CACHED_VECTOR_BYTES {
            return;
        }
        while self.vectors.len() >= MAX_CACHED_INPUTS
            || bytes > MAX_CACHED_VECTOR_BYTES.saturating_sub(self.vector_bytes)
        {
            let Some(oldest) = self
                .vectors
                .iter()
                .min_by(|left, right| {
                    left.1
                        .last_used
                        .cmp(&right.1.last_used)
                        .then_with(|| left.0.cmp(right.0))
                })
                .map(|(text, _)| *text)
            else {
                return;
            };
            if let Some(evicted) = self.vectors.remove(oldest) {
                self.vector_bytes -= evicted.values.len() * std::mem::size_of::<f32>();
            }
        }
        let last_used = self.next_tick();
        self.vectors.insert(
            text,
            CachedInput {
                values: values.into_boxed_slice(),
                last_used,
            },
        );
        self.vector_bytes += bytes;
    }

    fn next_tick(&mut self) -> u64 {
        if self.clock == u64::MAX {
            // Rank at most 128 borrowed keys instead of wrapping old entries
            // into apparent recency. This rare path copies no source text or
            // vectors and preserves the same deterministic eviction order.
            let mut ages = self
                .vectors
                .iter()
                .map(|(text, input)| (*text, input.last_used))
                .collect::<Vec<_>>();
            ages.sort_unstable_by(|left, right| {
                left.1.cmp(&right.1).then_with(|| left.0.cmp(right.0))
            });
            for (rank, (text, _)) in ages.into_iter().enumerate() {
                if let Some(input) = self.vectors.get_mut(text) {
                    input.last_used = rank as u64;
                }
            }
            self.clock = self.vectors.len() as u64;
        }
        self.clock += 1;
        self.clock
    }
}

pub(super) fn checkpoint(cx: &asupersync::Cx, tier: &str) -> Result<(), IncrementalFallback> {
    cx.checkpoint()
        .map_err(|error| tier_error(format!("{tier}-tier delta interrupted: {error}")))
}

fn tier_error(detail: String) -> IncrementalFallback {
    incremental_fallback(IncrementalFallbackReason::TierUnavailable, detail)
}

/// Bound documents, input bytes, and expected f32 values independently. A
/// single over-budget document is sent alone, unchanged, so batching never
/// truncates evidence or prevents progress. These bounds apply BEFORE input
/// deduplication, including all expanded WAL entries. They are not quotas on
/// a provider's internal allocations, identity metadata, or wire encoding.
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
            "{tier}-tier embedding batch returned {} vectors for {expected_count} distinct inputs",
            vectors.len(),
        )));
    }
    for vector in vectors {
        super::validate_bound_embedding(vector, expected_identity, tier)?;
    }
    Ok(())
}

/// A cache hit proves which producer made the retained vector, not which
/// producer this invocation is still using. Check the live contract even for
/// cache-only batches and at completion. Descriptor names are not identities.
fn validate_producer(
    cx: &asupersync::Cx,
    embedder: &dyn Embedder,
    expected_identity: &str,
    tier: &str,
) -> Result<(), IncrementalFallback> {
    checkpoint(cx, tier)?;
    let identity = super::verified_embedder_identity(embedder);
    checkpoint(cx, tier)?;
    if identity.as_deref() != Some(expected_identity) {
        return Err(incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            format!("{tier}-tier embedding producer no longer matches the planned identity"),
        ));
    }
    Ok(())
}

/// The full builder uses bound batches only with an explicit equivalence
/// guarantee. Otherwise a provider's overridden embed_bound can supply checks
/// or transformations absent from its raw batch method. Preserve that boundary
/// here too; native batching, deduplication and one WAL write per group remain.
async fn infer_inputs(
    cx: &asupersync::Cx,
    embedder: &dyn Embedder,
    texts: &[&str],
    expected_identity: &str,
    tier: &str,
) -> Result<Vec<IdentityBoundEmbedding>, IncrementalFallback> {
    validate_producer(cx, embedder, expected_identity, tier)?;
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    // Provider errors can include private source text or endpoint details.
    // Identity/cardinality validation below retains useful, bounded diagnostics.
    let failed = |_| tier_error(format!("{tier}-tier bound embedding inference failed"));
    if embedder.bound_batch_is_native() {
        validate_producer(cx, embedder, expected_identity, tier)?;
        let outcome = embedder.embed_batch_bound(cx, texts).await;
        // Inspect the caller before propagating even an ordinary provider error.
        validate_producer(cx, embedder, expected_identity, tier)?;
        let vectors = outcome.map_err(failed)?;
        validate_batch(&vectors, texts.len(), expected_identity, tier)?;
        return Ok(vectors);
    }

    let mut vectors = Vec::with_capacity(texts.len());
    for text in texts {
        validate_producer(cx, embedder, expected_identity, tier)?;
        let outcome = embedder.embed_bound(cx, text).await;
        validate_producer(cx, embedder, expected_identity, tier)?;
        let vector = outcome.map_err(failed)?;
        // Custom bound implementations are not assumed to perform validation.
        // Refuse a bad prefix now rather than inferring the rest of the group.
        super::validate_bound_embedding(&vector, expected_identity, tier)?;
        vectors.push(vector);
    }
    Ok(vectors)
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
    if documents.is_empty() {
        // A no-op has no producer or cache to validate, but cannot reset Cx.
        return checkpoint(cx, tier);
    }
    let mut cache = ValidatedInputCache::default();
    while !documents.is_empty() {
        validate_producer(cx, embedder, expected_identity, tier)?;
        let count = batch_len(documents, embedder.dimension());
        let (batch, remainder) = documents.split_at(count);
        let mut missing = BTreeMap::new();
        let mut texts = Vec::new();
        for document in batch {
            let text = document.content.as_str();
            if cache.get(text).is_none()
                && let Entry::Vacant(entry) = missing.entry(text)
            {
                entry.insert(texts.len());
                // Keep first-occurrence order: the provider's output order
                // binds these inputs, not the map's lexical key order.
                texts.push(text);
            }
        }
        let vectors = infer_inputs(cx, embedder, &texts, expected_identity, tier).await?;
        // Cancellation during inference must be seen before a WAL write even
        // when a custom embedder returns Ok without checking its caller Cx.
        // Cache-only batches take this checkpoint too; they cannot renew Cx.
        validate_producer(cx, embedder, expected_identity, tier)?;
        validate_batch(&vectors, texts.len(), expected_identity, tier)?;
        let entries = batch
            .iter()
            .map(|document| {
                let text = document.content.as_str();
                let values = cache.get(text).or_else(|| {
                    missing
                        .get(text)
                        .and_then(|position| vectors.get(*position))
                        .map(|vector| vector.values.as_slice())
                });
                let values = values.ok_or_else(|| {
                    tier_error(format!(
                        "{tier}-tier embedding batch has an unresolved input"
                    ))
                })?;
                // Reuse inference, never a document identity or a mutable WAL
                // buffer. Equal text under different IDs still writes every ID.
                Ok((document.id.clone(), values.to_vec()))
            })
            .collect::<Result<Vec<_>, IncrementalFallback>>()?;
        validate_producer(cx, embedder, expected_identity, tier)?;
        let written = write(entries);
        checkpoint(cx, tier)?;
        written?;
        for (text, vector) in texts.into_iter().zip(vectors) {
            // All fresh vectors were validated against this invocation's one
            // expected identity before the write. A failure above caches none.
            cache.remember(text, vector.values);
        }
        documents = remainder;
    }
    // A final cache-only write can also race cancellation or provider drift.
    // Earlier writes are private staging state; errors still abandon the stage.
    validate_producer(cx, embedder, expected_identity, tier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::HashEmbedder;
    use frankensearch::core::generation::EmbeddingIdentityBundleV1;
    use frankensearch::core::traits::{ModelCategory, SearchFuture};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    type TestResult = Result<(), String>;

    // Real hash inference and identities, with invocation sizes observed at
    // the trait boundary. One opt-in fault drops an actual response vector;
    // no fake vectors or mock index implementation supply successful results.
    struct ObservedHash {
        inner: HashEmbedder,
        sizes: Mutex<Vec<usize>>,
        truncate_response: bool,
        native_batch: bool,
    }

    impl ObservedHash {
        fn new() -> Self {
            Self {
                inner: HashEmbedder::default_256(),
                sizes: Mutex::new(Vec::new()),
                truncate_response: false,
                native_batch: true,
            }
        }
    }

    impl Embedder for ObservedHash {
        fn bound_batch_is_native(&self) -> bool {
            // Both bound methods use the core defaults over this real provider.
            self.native_batch
        }

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
            if self.truncate_response {
                Box::pin(async move {
                    let mut vectors = self.inner.embed_batch(cx, texts).await?;
                    let _ = vectors.pop();
                    Ok(vectors)
                })
            } else {
                self.inner.embed_batch(cx, texts)
            }
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
            assert!(embedder.sizes.lock().expect("sizes").is_empty());
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn identical_text_reuses_inference_across_batches_without_merging_documents() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut docs = documents(65);
            for (position, document) in docs.iter_mut().enumerate() {
                document.content = if position % 2 == 0 {
                    "shared repair"
                } else {
                    "shared failure"
                }
                .to_owned();
                document.title = Some(format!("distinct title {position}"));
                document
                    .metadata
                    .insert("source".to_owned(), position.to_string());
            }
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
            let mut sizes = Vec::new();
            embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |entries| {
                sizes.push(entries.len());
                written.extend(entries);
                Ok(())
            })
            .await
            .map_err(|error| error.detail)?;
            assert_eq!(written, expected);
            assert_eq!(sizes, vec![32, 32, 1]);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![2]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn mixed_hits_misses_and_unicode_preserve_exact_input_and_provider_order() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut docs = documents(65);
            for document in &mut docs[..32] {
                document.content = "already cached".to_owned();
            }
            let bodies = [
                "z-new",
                "already cached",
                "café",
                "cafe\u{301}",
                "Café",
                "",
                "a-new",
            ];
            for (position, document) in docs[32..].iter_mut().enumerate() {
                document.content = bodies[position % bodies.len()].to_owned();
            }
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
            embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |entries| {
                written.extend(entries);
                Ok(())
            })
            .await
            .map_err(|error| error.detail)?;
            assert_eq!(written, expected);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![1, 6]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cache_capacity_bounds_entries_and_vector_bytes_without_replacing_admitted_values() {
        let hash = HashEmbedder::default_256();
        let docs = documents(MAX_CACHED_INPUTS + 1);
        let mut cache = ValidatedInputCache::default();
        for document in &docs {
            cache.remember(&document.content, hash.embed_sync(&document.content));
        }
        assert_eq!(cache.vectors.len(), MAX_CACHED_INPUTS);
        assert_eq!(
            cache.vector_bytes,
            MAX_CACHED_INPUTS * 256 * std::mem::size_of::<f32>()
        );
        assert!(cache.get(&docs[0].content).is_none());
        assert!(cache.get(&docs[MAX_CACHED_INPUTS].content).is_some());

        // Allocation accounting is independent of inference. Exercise the
        // inclusive byte bound without constructing a pathological provider.
        let mut full = ValidatedInputCache::default();
        full.remember(
            "full",
            vec![0.0; MAX_CACHED_VECTOR_BYTES / std::mem::size_of::<f32>()],
        );
        assert_eq!(full.vector_bytes, MAX_CACHED_VECTOR_BYTES);
        full.remember("full", vec![1.0]);
        assert_eq!(full.vectors.len(), 1);
        assert_eq!(full.get("full").expect("original value")[0], 0.0);
        assert_eq!(full.vector_bytes, MAX_CACHED_VECTOR_BYTES);
        full.remember("overflow", vec![1.0]);
        assert_eq!(full.vectors.len(), 1);
        assert!(full.get("full").is_none());
        assert_eq!(full.get("overflow"), Some([1.0].as_slice()));
        assert_eq!(full.vector_bytes, std::mem::size_of::<f32>());
        let mut empty = ValidatedInputCache::default();
        empty.remember(
            "oversized",
            vec![0.0; MAX_CACHED_VECTOR_BYTES / std::mem::size_of::<f32>() + 1],
        );
        assert_eq!(empty.vector_bytes, 0);
        assert!(empty.vectors.is_empty());
    }

    #[test]
    fn late_inputs_are_reused_and_evicted_inputs_recomputed_without_losing_wal_entries()
    -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let original = documents(MAX_CACHED_INPUTS + MAX_BATCH_DOCUMENTS);
            let mut docs = original.clone();
            for (position, document) in original[MAX_CACHED_INPUTS..].iter().enumerate() {
                docs.push(IndexableDocument::new(
                    format!("overflow_replay_{position}"),
                    document.content.clone(),
                ));
            }
            docs.push(IndexableDocument::new(
                "evicted_replay",
                original[0].content.clone(),
            ));
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
            embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |entries| {
                assert!(entries.len() <= MAX_BATCH_DOCUMENTS);
                written.extend(entries);
                Ok(())
            })
            .await
            .map_err(|error| error.detail)?;
            assert_eq!(written, expected);
            let mut expected_batches =
                vec![MAX_BATCH_DOCUMENTS; MAX_CACHED_INPUTS / MAX_BATCH_DOCUMENTS + 1];
            expected_batches.push(1);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), expected_batches);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cached_only_batches_still_stop_on_writer_failure() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut docs = documents(65);
            for document in &mut docs {
                document.content = "shared repair".to_owned();
            }
            let mut writes = 0;
            let result = embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |_| {
                writes += 1;
                if writes == 2 {
                    Err(tier_error("cached batch refused".to_owned()))
                } else {
                    Ok(())
                }
            })
            .await;
            assert_eq!(
                result.expect_err("writer failure").detail,
                "cached batch refused"
            );
            assert_eq!(writes, 2);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![1]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn malformed_response_for_a_deduplicated_input_never_reaches_the_writer() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let mut embedder = ObservedHash::new();
            embedder.truncate_response = true;
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut docs = documents(65);
            for document in &mut docs {
                document.content = "shared repair".to_owned();
            }
            let result = embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |_| {
                panic!("a missing distinct-input vector cannot be expanded or written")
            })
            .await;
            assert_eq!(
                result.expect_err("missing vector").reason,
                IncrementalFallbackReason::TierUnavailable
            );
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![1]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn reuse_never_crosses_invocations_or_shares_mutable_writer_buffers() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut docs = documents(65);
            for document in &mut docs {
                document.content = "shared repair".to_owned();
            }
            let expected = embedder.inner.embed_sync("shared repair");
            for _ in 0..2 {
                let mut count = 0;
                embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |mut entries| {
                    for (_, values) in &mut entries {
                        assert_eq!(values.as_slice(), expected.as_slice());
                        values.fill(42.0);
                    }
                    count += entries.len();
                    Ok(())
                })
                .await
                .map_err(|error| error.detail)?;
                assert_eq!(count, docs.len());
            }
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![1, 1]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn staged_batch_updates_match_a_full_build_and_leave_the_live_index_unchanged() -> TestResult {
        assert_staged_updates(false, true)
    }

    #[test]
    fn staged_duplicate_inputs_match_a_full_build_without_merging_index_identities() -> TestResult {
        assert_staged_updates(true, true)
    }

    #[test]
    fn bound_single_staging_matches_a_full_build_with_duplicate_input_and_withdrawals() -> TestResult
    {
        assert_staged_updates(true, false)
    }

    #[test]
    fn cache_hits_protect_recent_inputs_when_a_later_input_is_admitted() {
        let hash = HashEmbedder::default_256();
        let docs = documents(MAX_CACHED_INPUTS + 1);
        let mut cache = ValidatedInputCache::default();
        for document in &docs[..MAX_CACHED_INPUTS] {
            cache.remember(&document.content, hash.embed_sync(&document.content));
        }
        assert!(cache.get(&docs[0].content).is_some());
        let newest = &docs[MAX_CACHED_INPUTS].content;
        cache.remember(newest, hash.embed_sync(newest));
        assert!(cache.get(&docs[0].content).is_some());
        assert!(cache.get(&docs[1].content).is_none());
        assert_eq!(cache.get(newest), Some(hash.embed_sync(newest).as_slice()));
        assert_eq!(cache.vectors.len(), MAX_CACHED_INPUTS);
        assert_eq!(cache.vector_bytes, MAX_CACHED_INPUTS * 256 * 4);
    }

    #[test]
    fn byte_pressure_evicts_enough_cold_inputs_but_an_oversized_miss_evicts_none() {
        let quarter = MAX_CACHED_VECTOR_BYTES / std::mem::size_of::<f32>() / 4;
        let mut cache = ValidatedInputCache::default();
        for key in ["a", "b", "c", "d"] {
            cache.remember(key, vec![1.0; quarter]);
        }
        assert_eq!(cache.vector_bytes, MAX_CACHED_VECTOR_BYTES);
        assert!(cache.get("a").is_some());
        cache.remember("e", vec![2.0; quarter * 2]);
        assert_eq!(cache.vectors.len(), 3);
        assert_eq!(cache.vector_bytes, MAX_CACHED_VECTOR_BYTES);
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
        assert!(cache.get("c").is_none());
        assert!(cache.get("d").is_some());
        assert_eq!(cache.get("e").map(<[f32]>::len), Some(quarter * 2));
        let before = cache.vectors.keys().copied().collect::<Vec<_>>();
        cache.remember("oversized", vec![3.0; quarter * 4 + 1]);
        assert_eq!(cache.vectors.keys().copied().collect::<Vec<_>>(), before);
        assert_eq!(cache.vector_bytes, MAX_CACHED_VECTOR_BYTES);
        assert!(cache.get("oversized").is_none());
        assert_eq!(cache.get("e").expect("retained vector")[0], 2.0);
    }

    #[test]
    fn recency_clock_renormalization_preserves_eviction_order() {
        let docs = documents(MAX_CACHED_INPUTS + 1);
        let mut cache = ValidatedInputCache::default();
        for document in &docs[..MAX_CACHED_INPUTS] {
            cache.remember(&document.content, vec![1.0]);
        }
        cache.clock = u64::MAX;
        assert!(cache.get(&docs[0].content).is_some());
        assert!(cache.clock < u64::MAX);
        cache.remember(&docs[MAX_CACHED_INPUTS].content, vec![2.0]);
        assert!(cache.get(&docs[1].content).is_none());
        assert_eq!(cache.get(&docs[0].content), Some([1.0].as_slice()));
        assert_eq!(
            cache.get(&docs[MAX_CACHED_INPUTS].content),
            Some([2.0].as_slice())
        );
        assert_eq!(cache.vectors.len(), MAX_CACHED_INPUTS);
        assert_eq!(cache.vector_bytes, MAX_CACHED_INPUTS * 4);
    }

    #[test]
    fn cache_matches_an_independent_recency_queue_through_repeated_eviction_and_clock_wraps() {
        use std::collections::{BTreeSet, VecDeque};

        let hash = HashEmbedder::default_256();
        let docs = documents(193);
        let mut cache = ValidatedInputCache::default();
        let mut order = VecDeque::new();
        let mut random = 7_u64;
        for step in 0..6000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let number = ((random >> 32) as usize) % docs.len();
            let text = docs[number].content.as_str();
            let prior = order.iter().position(|id| *id == number);
            if step % 997 == 0 {
                cache.clock = u64::MAX;
            }
            if step % 4 == 0 {
                let expected = prior.map(|position| {
                    order.remove(position);
                    order.push_back(number);
                    hash.embed_sync(text)
                });
                assert_eq!(cache.get(text).map(<[f32]>::to_vec), expected);
            } else {
                if let Some(position) = prior {
                    order.remove(position);
                } else if order.len() == MAX_CACHED_INPUTS {
                    order.pop_front();
                }
                order.push_back(number);
                cache.remember(text, hash.embed_sync(text));
            }
            let expected = order
                .iter()
                .map(|id| docs[*id].content.as_str())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                cache.vectors.keys().copied().collect::<BTreeSet<_>>(),
                expected
            );
            assert_eq!(cache.vector_bytes, order.len() * 256 * 4);
            assert!(cache.vectors.len() <= MAX_CACHED_INPUTS);
            assert!(cache.vector_bytes <= MAX_CACHED_VECTOR_BYTES);
        }
    }

    #[test]
    fn repeated_late_phase_does_not_reembed_after_an_unrelated_prefix_fills_the_cache() -> TestResult
    {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut docs = documents(160);
            for number in 0..512 {
                let text = docs[128 + number % 32].content.clone();
                docs.push(IndexableDocument::new(format!("late_{number}"), text));
            }
            let expected = docs
                .iter()
                .map(|document| {
                    (
                        document.id.clone(),
                        embedder.inner.embed_sync(&document.content),
                    )
                })
                .collect::<Vec<_>>();
            let mut written = Vec::new();
            embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |entries| {
                assert!(entries.len() <= MAX_BATCH_DOCUMENTS);
                written.extend(entries);
                Ok(())
            })
            .await
            .map_err(|error| error.detail)?;
            assert_eq!(written, expected);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![32; 5]);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cancellation_after_cache_replacement_stops_before_another_cached_batch_is_written()
    -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |_runtime_cx| async move {
            let cx = asupersync::Cx::for_testing();
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut docs = documents(MAX_CACHED_INPUTS);
            for number in 0..96 {
                docs.push(IndexableDocument::new(
                    format!("late_{number}"),
                    "late repeated body",
                ));
            }
            let mut writes = 0;
            let result = embed_batches_with(&cx, &embedder, &docs, &identity, "fast", |_| {
                writes += 1;
                if writes == 6 {
                    cx.set_cancel_reason(asupersync::CancelReason::user("stop late cache reuse"));
                }
                Ok(())
            })
            .await;
            assert!(result.is_err());
            assert_eq!(writes, 6);
            assert_eq!(
                *embedder.sizes.lock().expect("sizes"),
                vec![32, 32, 32, 32, 1]
            );
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cache_replacement_in_a_native_staging_index_matches_full_rebuild() -> TestResult {
        use super::super::super::{
            IndexBuilder, compact_incremental_vector_index, hash_fallback_embedder_stack,
            open_fast_vector_index, open_fast_vector_index_read_only,
        };
        use std::path::Path;

        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(120), |cx| async move {
            let _root = root;
            let live = parent.join("live");
            let staged = parent.join("staged");
            let rebuilt = parent.join("rebuilt");
            let original = documents(160);
            let stack = hash_fallback_embedder_stack();
            IndexBuilder::new(&live)
                .with_embedder_stack(stack.clone())
                .add_documents(original.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            let read_vectors = |path: &Path| -> Result<BTreeMap<String, Vec<f32>>, String> {
                let index = open_fast_vector_index_read_only(path).map_err(|error| error.detail)?;
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
            let original_vectors = read_vectors(&live)?;
            std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
            super::super::copy_generation(&live, &staged).map_err(|error| error.to_string())?;
            let mut updated = original;
            for document in &mut updated {
                document.content.push_str(" revised");
            }
            for number in 0..96 {
                let text = updated[128 + number % 32].content.clone();
                updated.push(IndexableDocument::new(format!("late_{number}"), text));
            }
            let embedder = ObservedHash::new();
            let identity = embedder
                .identity()
                .map_err(|error| error.to_string())?
                .fingerprint();
            let mut index = open_fast_vector_index(&staged).map_err(|error| error.detail)?;
            upsert(&cx, &mut index, &embedder, &updated, &identity, "fast")
                .await
                .map_err(|error| error.detail)?;
            compact_incremental_vector_index(&mut index, "fast").map_err(|error| error.detail)?;
            drop(index);
            assert_eq!(*embedder.sizes.lock().expect("sizes"), vec![32; 5]);
            IndexBuilder::new(&rebuilt)
                .with_embedder_stack(stack)
                .add_documents(updated)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            let staged_vectors = read_vectors(&staged)?;
            assert_eq!(staged_vectors.len(), 256);
            assert_eq!(staged_vectors, read_vectors(&rebuilt)?);
            assert_eq!(read_vectors(&live)?, original_vectors);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ProviderFault {
        None,
        Refuse,
        ShortVector,
        NonFinite,
        ForeignResponse,
        Drift,
        Unavailable,
        Cancel,
        CancelAndRefuse,
    }

    // Genuine JL inference with same-dimensional, different-seed identities.
    // Bound entry points alone may run; faults alter only the declared boundary
    // under test. No model downloads or substitute vector store are involved.
    struct ContractHash {
        inner: HashEmbedder,
        alternate: HashEmbedder,
        native_batch: bool,
        fault: ProviderFault,
        fault_at: usize,
        changed: AtomicBool,
        single_calls: AtomicUsize,
        batch_calls: AtomicUsize,
    }

    fn provider_error() -> frankensearch::SearchError {
        frankensearch::SearchError::InvalidConfig {
            field: "test.bound-provider".to_owned(),
            value: "PRIVATE-PROVIDER-ERROR".to_owned(),
            reason: "bound inference refused".to_owned(),
        }
    }

    impl ContractHash {
        fn new(native_batch: bool, fault: ProviderFault, fault_at: usize) -> Self {
            Self {
                inner: HashEmbedder::jl_384(11),
                alternate: HashEmbedder::jl_384(12),
                native_batch,
                fault,
                fault_at,
                changed: AtomicBool::new(false),
                single_calls: AtomicUsize::new(0),
                batch_calls: AtomicUsize::new(0),
            }
        }

        fn finish(
            &self,
            cx: &asupersync::Cx,
            vectors: &mut [IdentityBoundEmbedding],
            call: usize,
        ) -> frankensearch::SearchResult<()> {
            if call != self.fault_at {
                return Ok(());
            }
            match self.fault {
                ProviderFault::None => {}
                ProviderFault::Refuse => return Err(provider_error()),
                ProviderFault::Drift | ProviderFault::Unavailable => {
                    self.changed.store(true, Ordering::SeqCst);
                }
                ProviderFault::Cancel | ProviderFault::CancelAndRefuse => {
                    cx.set_cancel_reason(asupersync::CancelReason::user("stop delta provider"));
                    if self.fault == ProviderFault::CancelAndRefuse {
                        return Err(provider_error());
                    }
                }
                fault => {
                    let vector = vectors.first_mut().expect("nonempty inference request");
                    match fault {
                        ProviderFault::ShortVector => {
                            let _ = vector.values.pop();
                        }
                        ProviderFault::NonFinite => vector.values[0] = f32::NAN,
                        ProviderFault::ForeignResponse => {
                            vector.identity = self.alternate.identity()?.clone();
                        }
                        _ => unreachable!("other fault arms were handled above"),
                    }
                }
            }
            Ok(())
        }
    }

    impl Embedder for ContractHash {
        fn embed<'a>(
            &'a self,
            _cx: &'a asupersync::Cx,
            _text: &'a str,
        ) -> SearchFuture<'a, Vec<f32>> {
            panic!("delta intake must not bypass the bound single implementation")
        }

        fn embed_bound<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            text: &'a str,
        ) -> SearchFuture<'a, IdentityBoundEmbedding> {
            Box::pin(async move {
                let call = self.single_calls.fetch_add(1, Ordering::SeqCst) + 1;
                let mut vector = self.inner.embed_bound(cx, text).await?;
                self.finish(cx, std::slice::from_mut(&mut vector), call)?;
                Ok(vector)
            })
        }

        fn embed_batch_bound<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            texts: &'a [&'a str],
        ) -> SearchFuture<'a, Vec<IdentityBoundEmbedding>> {
            assert!(self.native_batch, "non-equivalent bound batch must not run");
            Box::pin(async move {
                let call = self.batch_calls.fetch_add(1, Ordering::SeqCst) + 1;
                let mut vectors = self.inner.embed_batch_bound(cx, texts).await?;
                self.finish(cx, &mut vectors, call)?;
                Ok(vectors)
            })
        }

        fn bound_batch_is_native(&self) -> bool {
            self.native_batch
        }

        fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
            if self.changed.load(Ordering::SeqCst) {
                if self.fault == ProviderFault::Unavailable {
                    Err(provider_error())
                } else {
                    self.alternate.identity()
                }
            } else {
                self.inner.identity()
            }
        }

        fn dimension(&self) -> usize {
            self.inner.dimension()
        }

        fn id(&self) -> &str {
            "descriptor-unchanged"
        }

        fn model_name(&self) -> &str {
            "descriptor-unchanged"
        }

        fn is_semantic(&self) -> bool {
            self.inner.is_semantic()
        }

        fn category(&self) -> ModelCategory {
            self.inner.category()
        }
    }

    #[test]
    fn bound_single_and_native_batches_preserve_vectors_deduplication_and_wal_groups() -> TestResult
    {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            for native in [false, true] {
                let model = ContractHash::new(native, ProviderFault::None, 0);
                let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
                let mut docs = documents(65);
                for (number, document) in docs.iter_mut().enumerate() {
                    document.content = format!("shared input {}", number % 3);
                }
                let expected = docs
                    .iter()
                    .map(|doc| (doc.id.clone(), model.inner.embed_sync(&doc.content)))
                    .collect::<Vec<_>>();
                let mut written = Vec::new();
                let mut sizes = Vec::new();
                embed_batches_with(&cx, &model, &docs, &identity, "fast", |entries| {
                    sizes.push(entries.len());
                    written.extend(entries);
                    Ok(())
                })
                .await
                .map_err(|error| error.detail)?;
                assert_eq!(written, expected);
                assert_eq!(sizes, [32, 32, 1]);
                assert_eq!(
                    model.single_calls.load(Ordering::SeqCst),
                    if native { 0 } else { 3 }
                );
                assert_eq!(
                    model.batch_calls.load(Ordering::SeqCst),
                    usize::from(native)
                );
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn bound_single_refusal_stops_the_group_without_writing_its_valid_prefix() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            for (native, at) in [
                (false, 1),
                (false, 2),
                (false, 32),
                (false, 34),
                (true, 1),
                (true, 2),
            ] {
                let model = ContractHash::new(native, ProviderFault::Refuse, at);
                let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
                let mut writes = 0;
                let result =
                    embed_batches_with(&cx, &model, &documents(65), &identity, "fast", |_| {
                        writes += 1;
                        Ok(())
                    })
                    .await;
                let error = result.expect_err("bound provider refusal must survive batching");
                assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
                assert!(!error.detail.contains("PRIVATE-PROVIDER-ERROR"));
                assert_eq!(writes, if native { at - 1 } else { (at - 1) / 32 });
                assert_eq!(
                    model.single_calls.load(Ordering::SeqCst),
                    if native { 0 } else { at }
                );
                assert_eq!(
                    model.batch_calls.load(Ordering::SeqCst),
                    if native { at } else { 0 }
                );
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn custom_bound_responses_are_validated_in_both_inference_lanes() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            for native in [false, true] {
                for fault in [
                    ProviderFault::ShortVector,
                    ProviderFault::NonFinite,
                    ProviderFault::ForeignResponse,
                ] {
                    let at = if native { 1 } else { 2 };
                    let model = ContractHash::new(native, fault, at);
                    let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
                    let result =
                        embed_batches_with(&cx, &model, &documents(65), &identity, "fast", |_| {
                            panic!("malformed bound response cannot reach a WAL write")
                        })
                        .await;
                    let error = result.expect_err("invalid bound response");
                    assert_eq!(
                        error.reason,
                        if fault == ProviderFault::ForeignResponse {
                            IncrementalFallbackReason::CorpusRevisionMismatch
                        } else {
                            IncrementalFallbackReason::TierUnavailable
                        }
                    );
                    assert_eq!(
                        model.single_calls.load(Ordering::SeqCst),
                        if native { 0 } else { 2 }
                    );
                    assert_eq!(
                        model.batch_calls.load(Ordering::SeqCst),
                        usize::from(native)
                    );
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn descriptor_identical_drift_or_lost_identity_cannot_authorize_a_bound_response() -> TestResult
    {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            for native in [false, true] {
                for fault in [ProviderFault::Drift, ProviderFault::Unavailable] {
                    let model = ContractHash::new(native, fault, 1);
                    let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
                    assert_eq!(model.inner.dimension(), model.alternate.dimension());
                    let result =
                        embed_batches_with(&cx, &model, &documents(65), &identity, "fast", |_| {
                            panic!("producer drift must discard even otherwise valid vectors")
                        })
                        .await;
                    let error = result.expect_err("producer changed during bound inference");
                    assert_eq!(
                        error.reason,
                        IncrementalFallbackReason::CorpusRevisionMismatch
                    );
                    assert!(!error.detail.contains("PRIVATE-PROVIDER-ERROR"));
                    assert_eq!(model.id(), "descriptor-unchanged");
                    assert_eq!(model.model_name(), "descriptor-unchanged");
                    assert_eq!(
                        model.single_calls.load(Ordering::SeqCst),
                        usize::from(!native)
                    );
                    assert_eq!(
                        model.batch_calls.load(Ordering::SeqCst),
                        usize::from(native)
                    );
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cached_only_batches_and_final_completion_recheck_the_live_producer() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            for native in [false, true] {
                for change_at in [1, 3] {
                    let model = ContractHash::new(native, ProviderFault::None, 0);
                    let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
                    let mut docs = documents(65);
                    for document in &mut docs {
                        document.content = "one cached body".to_owned();
                    }
                    let mut writes = 0;
                    let result = embed_batches_with(&cx, &model, &docs, &identity, "fast", |_| {
                        writes += 1;
                        if writes == change_at {
                            model.changed.store(true, Ordering::SeqCst);
                        }
                        Ok(())
                    })
                    .await;
                    assert_eq!(
                        result.expect_err("cache is not producer authority").reason,
                        IncrementalFallbackReason::CorpusRevisionMismatch
                    );
                    assert_eq!(writes, change_at);
                    assert_eq!(
                        model.single_calls.load(Ordering::SeqCst),
                        usize::from(!native)
                    );
                    assert_eq!(
                        model.batch_calls.load(Ordering::SeqCst),
                        usize::from(native)
                    );
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cancellation_outranks_success_or_failure_from_either_bound_method() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |_runtime_cx| async move {
            for native in [false, true] {
                for fault in [ProviderFault::Cancel, ProviderFault::CancelAndRefuse] {
                    let cx = asupersync::Cx::for_testing();
                    let model = ContractHash::new(native, fault, 1);
                    let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
                    let result =
                        embed_batches_with(&cx, &model, &documents(65), &identity, "fast", |_| {
                            panic!("cancelled inference cannot write its successful prefix")
                        })
                        .await;
                    let error = result.expect_err("original caller cancellation");
                    assert!(error.detail.contains("interrupted"));
                    assert!(!error.detail.contains("PRIVATE-PROVIDER-ERROR"));
                    assert!(cx.checkpoint().is_err());
                    assert_eq!(
                        model.single_calls.load(Ordering::SeqCst),
                        usize::from(!native)
                    );
                    assert_eq!(
                        model.batch_calls.load(Ordering::SeqCst),
                        usize::from(native)
                    );
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn cancellation_during_the_final_wal_write_is_not_reported_as_success() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |_runtime_cx| async move {
            for count in [32, 65] {
                for fail in [false, true] {
                    let cx = asupersync::Cx::for_testing();
                    let model = ContractHash::new(true, ProviderFault::None, 0);
                    let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
                    let mut docs = documents(count);
                    for document in &mut docs {
                        document.content = "one cached body".to_owned();
                    }
                    let mut writes = 0;
                    let last_write = count.div_ceil(MAX_BATCH_DOCUMENTS);
                    let result = embed_batches_with(&cx, &model, &docs, &identity, "fast", |_| {
                        writes += 1;
                        if writes == last_write {
                            cx.set_cancel_reason(asupersync::CancelReason::user(
                                "stop final WAL write",
                            ));
                            if fail {
                                return Err(tier_error("PRIVATE-WRITER-ERROR".to_owned()));
                            }
                        }
                        Ok(())
                    })
                    .await;
                    let error = result.expect_err("private staging must not be accepted");
                    assert!(error.detail.contains("interrupted"));
                    assert!(!error.detail.contains("PRIVATE-WRITER-ERROR"));
                    assert_eq!(writes, last_write);
                    assert_eq!(model.batch_calls.load(Ordering::SeqCst), 1);
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn missing_producer_is_rejected_before_inference_and_cancelled_empty_work_stays_cancelled()
    -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |_runtime_cx| async move {
            let cx = asupersync::Cx::for_testing();
            let model = ContractHash::new(false, ProviderFault::Unavailable, 1);
            let identity = model.identity().map_err(|e| e.to_string())?.fingerprint();
            model.changed.store(true, Ordering::SeqCst);
            let result = embed_batches_with(&cx, &model, &documents(2), &identity, "fast", |_| {
                panic!("missing identity cannot enter inference or the writer")
            })
            .await;
            let error = result.expect_err("unavailable producer");
            assert_eq!(
                error.reason,
                IncrementalFallbackReason::CorpusRevisionMismatch
            );
            assert!(!error.detail.contains("PRIVATE-PROVIDER-ERROR"));
            cx.set_cancel_reason(asupersync::CancelReason::user("stop no-op delta"));
            let error = embed_batches_with(&cx, &model, &[], "unused", "fast", |_| {
                panic!("empty work must not write")
            })
            .await
            .expect_err("no-op must not renew a cancelled request");
            assert!(error.detail.contains("interrupted"));
            assert_eq!(model.single_calls.load(Ordering::SeqCst), 0);
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), 0);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    fn assert_staged_updates(repeat_content: bool, native_batch: bool) -> TestResult {
        use super::super::super::{
            IndexBuilder, compact_incremental_vector_index, hash_fallback_embedder_stack,
            open_fast_vector_index,
        };

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
            for (position, document) in updated[..65].iter_mut().enumerate() {
                if repeat_content && position % 2 == 0 {
                    document.content = "shared revised lesson".to_owned();
                } else {
                    document.content.push_str(" revised");
                }
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
            let mut embedder = ObservedHash::new();
            embedder.native_batch = native_batch;
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
            assert_eq!(
                *embedder.sizes.lock().expect("sizes"),
                if !native_batch {
                    Vec::<usize>::new()
                } else if repeat_content {
                    vec![17, 16]
                } else {
                    vec![32, 32, 1]
                }
            );
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
