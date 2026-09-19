//! Request-local observation of embedding failures and verified lexical recovery.
//!
//! Frankensearch may preserve lexical results after inference fails. Observe the
//! producer rather than guessing from zero semantic hits (a valid empty answer)
//! or parsing backend error prose. Identity, corruption and cancellation errors
//! remain errors even if inference also failed earlier in the request.

use std::sync::{Arc, OnceLock};

use frankensearch::core::generation::EmbeddingIdentityBundleV1;
use frankensearch::core::traits::{IdentityBoundEmbedding, ModelCategory, ModelTier};
use frankensearch::{Embedder, SearchFuture, SearchResult};

use super::{
    Deterministic, FrankensearchFinalScoreScale, SearchDegradation, SearchError, SearchHit,
    SearchSourceMode, Seed, SourceModeResolution, canonicalize_equivalent_component_scores,
    map_frankensearch_error, open_lexical_searcher, search_checkpoint,
    search_hits_from_scored_results, sort_search_hits_by_score_order,
};

const INFERENCE_UNAVAILABLE: &str = "the selected embedding model failed during retrieval";

pub(super) struct Retrieval {
    pub hits: Vec<SearchHit>,
    pub degraded: Vec<SearchDegradation>,
    pub applied: SearchSourceMode,
}

impl Retrieval {
    pub(super) fn resolve_mode(
        &self,
        requested: SearchSourceMode,
        strict: bool,
        previous: SourceModeResolution,
    ) -> Result<SourceModeResolution, SearchError> {
        if self.applied == previous.applied {
            return Ok(previous);
        }
        if strict {
            return Err(SearchError::SourceModeUnavailable {
                requested,
                reason: INFERENCE_UNAVAILABLE.to_owned(),
            });
        }
        Ok(SourceModeResolution {
            applied: self.applied,
            fallback_applied: true,
            unavailable_no_results: false,
        })
    }
}

/// Only inference availability failures authorize lexical recovery. In
/// particular, never recover producer identity, vector dimension, index
/// integrity, query syntax, I/O or cancellation errors here.
fn recoverable(error: &frankensearch::SearchError) -> bool {
    matches!(
        error,
        frankensearch::SearchError::EmbedderUnavailable { .. }
            | frankensearch::SearchError::EmbeddingFailed { .. }
            | frankensearch::SearchError::ModelNotFound { .. }
            | frankensearch::SearchError::ModelLoadFailed { .. }
    )
}

pub(super) struct ObservedEmbedder {
    inner: Arc<dyn Embedder>,
    failed: OnceLock<()>,
}

impl ObservedEmbedder {
    pub(super) fn new(inner: Arc<dyn Embedder>) -> Self {
        Self {
            inner,
            failed: OnceLock::new(),
        }
    }

    fn observe<T>(&self, result: SearchResult<T>) -> SearchResult<T> {
        if result.as_ref().is_err_and(recoverable) {
            let _ = self.failed.set(());
        }
        result
    }

    pub(super) fn needs_recovery<T>(&self, result: &SearchResult<T>) -> bool {
        match result {
            Ok(_) => self.failed.get().is_some(),
            Err(error) => recoverable(error),
        }
    }
}

impl Embedder for ObservedEmbedder {
    fn embed<'a>(&'a self, cx: &'a asupersync::Cx, text: &'a str) -> SearchFuture<'a, Vec<f32>> {
        Box::pin(async move { self.observe(self.inner.embed(cx, text).await) })
    }

    fn embed_batch<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        texts: &'a [&'a str],
    ) -> SearchFuture<'a, Vec<Vec<f32>>> {
        Box::pin(async move { self.observe(self.inner.embed_batch(cx, texts).await) })
    }

    fn embed_bound<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        text: &'a str,
    ) -> SearchFuture<'a, IdentityBoundEmbedding> {
        Box::pin(async move { self.observe(self.inner.embed_bound(cx, text).await) })
    }

    fn embed_batch_bound<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        texts: &'a [&'a str],
    ) -> SearchFuture<'a, Vec<IdentityBoundEmbedding>> {
        Box::pin(async move { self.observe(self.inner.embed_batch_bound(cx, texts).await) })
    }

    fn identity(&self) -> SearchResult<&EmbeddingIdentityBundleV1> {
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

    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    fn is_semantic(&self) -> bool {
        self.inner.is_semantic()
    }

    fn category(&self) -> ModelCategory {
        self.inner.category()
    }

    fn tier(&self) -> ModelTier {
        self.inner.tier()
    }

    fn supports_mrl(&self) -> bool {
        self.inner.supports_mrl()
    }

    fn truncate_embedding(&self, embedding: &[f32], target_dim: usize) -> SearchResult<Vec<f32>> {
        self.inner.truncate_embedding(embedding, target_dim)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn recover_lexical(
    cx: &asupersync::Cx,
    index_dir: &std::path::Path,
    query: &str,
    limit: usize,
    explain: bool,
    requested: SearchSourceMode,
    seed: Deterministic<Seed>,
) -> Result<Retrieval, SearchError> {
    search_checkpoint(cx)?;
    // Open the lexical arm from the generation the caller already pinned.
    // Missing or broken lexical data is not a successful empty recovery.
    let lexical = open_lexical_searcher(index_dir)
        .map_err(SearchError::Index)?
        .ok_or_else(|| SearchError::SourceModeUnavailable {
            requested,
            reason: "embedding inference failed and no lexical index is available".to_owned(),
        })?;
    let result = lexical.search(cx, query, limit).await;
    search_checkpoint(cx)?;
    let results =
        result.map_err(|error| map_frankensearch_error(cx, "Lexical recovery failed", error))?;
    let mut hits =
        search_hits_from_scored_results(results, explain, FrankensearchFinalScoreScale::Native);
    canonicalize_equivalent_component_scores(&mut hits, &seed);
    sort_search_hits_by_score_order(&mut hits);
    Ok(Retrieval {
        hits,
        degraded: vec![SearchDegradation::source_mode_fallback(
            requested,
            SearchSourceMode::LexicalOnly,
            INFERENCE_UNAVAILABLE,
        )],
        applied: SearchSourceMode::LexicalOnly,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use frankensearch::SearchError as BackendError;

    fn observer() -> ObservedEmbedder {
        ObservedEmbedder::new(Arc::new(crate::search::HashEmbedder::default_256()))
    }

    fn inference_error() -> BackendError {
        BackendError::EmbeddingFailed {
            model: "private-model-id".to_owned(),
            source: Box::new(std::io::Error::other("private-inference-payload")),
        }
    }

    #[cfg(feature = "lexical-bm25")]
    struct SwitchableEmbedder {
        hash: crate::search::HashEmbedder,
        failed: std::sync::atomic::AtomicBool,
    }

    #[cfg(feature = "lexical-bm25")]
    impl Embedder for SwitchableEmbedder {
        fn embed<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            text: &'a str,
        ) -> SearchFuture<'a, Vec<f32>> {
            Box::pin(async move {
                if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
                    Err(inference_error())
                } else {
                    self.hash.embed(cx, text).await
                }
            })
        }

        fn identity(&self) -> SearchResult<&EmbeddingIdentityBundleV1> {
            self.hash.identity()
        }

        fn dimension(&self) -> usize {
            self.hash.dimension()
        }
        fn id(&self) -> &str {
            self.hash.id()
        }
        fn model_name(&self) -> &str {
            self.hash.model_name()
        }
        fn is_semantic(&self) -> bool {
            true
        }
        fn category(&self) -> ModelCategory {
            self.hash.category()
        }
    }

    #[cfg(feature = "lexical-bm25")]
    fn exercise_runtime_recovery(lexical_available: bool) -> Result<(), String> {
        use super::super::{
            SearchFusionWeights, SearchPerformanceTrace, SearchRerankRuntime,
            search_sync_with_performance,
        };
        use crate::search::{EmbedderStack, IndexBuilder, IndexableDocument, TwoTierConfig};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let index = root.join("index");
        let embedder = Arc::new(SwitchableEmbedder {
            hash: crate::search::HashEmbedder::default_256(),
            failed: std::sync::atomic::AtomicBool::new(false),
        });
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
            let docs = vec![
                IndexableDocument::new(
                    "mem_51000000000000000000000001",
                    "quasarneedle release recovery instructions",
                ),
                IndexableDocument::new(
                    "mem_51000000000000000000000002",
                    "unrelated gardening notes",
                ),
            ];
            IndexBuilder::new(&index)
                .with_embedder_stack(EmbedderStack::from_parts(embedder.clone(), None))
                .add_documents(docs.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            if lexical_available {
                crate::core::index::build_lexical_tier(&cx, &index, &docs)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            embedder
                .failed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            for source in [SearchSourceMode::SemanticOnly, SearchSourceMode::Hybrid] {
                for query in ["quasarneedle", "absentzyxneedle"] {
                    let mut trace = SearchPerformanceTrace::default();
                    let result = search_sync_with_performance(
                        &cx,
                        &index,
                        query,
                        5,
                        TwoTierConfig::default(),
                        true,
                        source,
                        Deterministic::from_seed(17).shared_child("runtime-recovery"),
                        SearchRerankRuntime::disabled(),
                        SearchFusionWeights::default(),
                        Some(embedder.clone()),
                        &mut trace,
                    )
                    .await;
                    if !lexical_available {
                        assert!(matches!(
                            result,
                            Err(SearchError::SourceModeUnavailable { .. })
                        ));
                        continue;
                    }
                    let actual = result.map_err(|error| error.to_string())?;
                    assert_eq!(actual.applied, SearchSourceMode::LexicalOnly);
                    assert_eq!(actual.degraded.len(), 1);
                    assert_eq!(actual.degraded[0].code, "source_mode_fallback");
                    assert!(!actual.degraded[0].message.contains("private"));
                    let expected = usize::from(query == "quasarneedle");
                    assert_eq!(actual.hits.len(), expected);
                    if let Some(hit) = actual.hits.first() {
                        assert_eq!(hit.doc_id, "mem_51000000000000000000000001");
                        assert_eq!(hit.source, super::super::ScoreSource::Lexical);
                        assert!(hit.lexical_score.is_some_and(|score| score > 0.0));
                    }
                    let before = SourceModeResolution {
                        applied: source,
                        fallback_applied: false,
                        unavailable_no_results: false,
                    };
                    assert!(
                        actual
                            .resolve_mode(source, false, before)
                            .map_err(|error| error.to_string())?
                            .fallback_applied
                    );
                    assert!(matches!(
                        actual.resolve_mode(source, true, before),
                        Err(SearchError::SourceModeUnavailable { .. })
                    ));
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn real_search_recovers_positive_and_negative_queries_after_inference_failure()
    -> Result<(), String> {
        exercise_runtime_recovery(true)
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn missing_lexical_index_is_not_reported_as_successful_empty_recovery() -> Result<(), String> {
        exercise_runtime_recovery(false)
    }

    #[test]
    fn observes_swallowed_inference_failure_without_exposing_details() {
        let observed = observer();
        assert!(observed.observe::<()>(Err(inference_error())).is_err());
        assert!(observed.needs_recovery(&Ok(Vec::<f32>::new())));
        assert!(!INFERENCE_UNAVAILABLE.contains("private"));
        let fresh = observer();
        assert!(!fresh.needs_recovery(&Ok(Vec::<f32>::new())));
    }

    #[test]
    fn all_inference_availability_errors_allow_recovery() {
        for error in [
            inference_error(),
            BackendError::EmbedderUnavailable {
                model: "model".to_owned(),
                reason: "unloaded".to_owned(),
            },
            BackendError::ModelNotFound {
                name: "model".to_owned(),
            },
            BackendError::ModelLoadFailed {
                path: "model".into(),
                source: Box::new(std::io::Error::other("unavailable")),
            },
        ] {
            assert!(observer().needs_recovery::<()>(&Err(error)));
        }
    }

    #[test]
    fn cancellation_and_integrity_errors_override_previous_inference_failure() {
        let observed = observer();
        let _ = observed.observe::<()>(Err(inference_error()));
        for error in [
            BackendError::Cancelled {
                phase: "embed".to_owned(),
                reason: "stop".to_owned(),
            },
            BackendError::IndexCorrupted {
                path: "index".into(),
                detail: "CRC".to_owned(),
            },
            BackendError::DimensionMismatch {
                expected: 256,
                found: 128,
            },
            BackendError::InvalidConfig {
                field: "search_activation.fast.producer_revision".to_owned(),
                value: "private".to_owned(),
                reason: "identity mismatch".to_owned(),
            },
            BackendError::QueryParseError {
                query: "private".to_owned(),
                detail: "invalid".to_owned(),
            },
            BackendError::Io(std::io::Error::other("unreadable index")),
            BackendError::SearchTimeout {
                elapsed_ms: 30,
                budget_ms: 20,
            },
        ] {
            assert!(!observed.needs_recovery::<()>(&Err(error)));
        }
    }

    #[test]
    fn observation_preserves_producer_identity_and_metadata() -> Result<(), String> {
        let observed = observer();
        let inner = &observed.inner;
        assert_eq!(
            observed.identity().map_err(|e| e.to_string())?,
            inner.identity().map_err(|e| e.to_string())?
        );
        assert_eq!(observed.dimension(), inner.dimension());
        assert_eq!(observed.id(), inner.id());
        assert_eq!(observed.model_name(), inner.model_name());
        assert_eq!(observed.is_ready(), inner.is_ready());
        assert_eq!(observed.is_semantic(), inner.is_semantic());
        assert_eq!(observed.category(), inner.category());
        assert_eq!(observed.tier(), inner.tier());
        assert_eq!(observed.supports_mrl(), inner.supports_mrl());
        assert_eq!(
            observed
                .truncate_embedding(&[1.0, 2.0], 1)
                .map_err(|e| e.to_string())?,
            inner
                .truncate_embedding(&[1.0, 2.0], 1)
                .map_err(|e| e.to_string())?
        );
        Ok(())
    }

    #[test]
    fn runtime_source_mode_refuses_strict_requests_and_preserves_successful_modes()
    -> Result<(), String> {
        for requested in [SearchSourceMode::Hybrid, SearchSourceMode::SemanticOnly] {
            let previous = SourceModeResolution {
                applied: requested,
                fallback_applied: false,
                unavailable_no_results: false,
            };
            let recovered = Retrieval {
                hits: Vec::new(),
                degraded: Vec::new(),
                applied: SearchSourceMode::LexicalOnly,
            };
            let actual = recovered
                .resolve_mode(requested, false, previous)
                .map_err(|e| e.to_string())?;
            assert_eq!(actual.applied, SearchSourceMode::LexicalOnly);
            assert!(actual.fallback_applied);
            assert!(matches!(
                recovered.resolve_mode(requested, true, previous),
                Err(SearchError::SourceModeUnavailable { .. })
            ));
            let healthy = Retrieval {
                applied: requested,
                ..recovered
            };
            assert_eq!(
                healthy
                    .resolve_mode(requested, true, previous)
                    .map_err(|e| e.to_string())?,
                previous
            );
        }
        Ok(())
    }
}
