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

/// A bounded, redaction-safe record of an error the backend may suppress.
/// Never retain the backend's arbitrary model, path, query or error text.
/// The original error still goes to the searcher unchanged; this record is
/// only used if the searcher subsequently claims success or availability loss.
#[derive(Clone, Copy)]
enum ProducerRejection {
    Dimension { expected: usize, found: usize },
    FastIdentity,
    QualityIdentity,
    UnverifiableIdentity,
    Other,
}

impl ProducerRejection {
    fn from_error(error: &frankensearch::SearchError) -> Self {
        use frankensearch::SearchError as BackendError;
        match error {
            BackendError::DimensionMismatch { expected, found } => Self::Dimension {
                expected: *expected,
                found: *found,
            },
            BackendError::InvalidConfig { field, .. }
                if field == "search_activation.fast.producer_revision" =>
            {
                Self::FastIdentity
            }
            BackendError::InvalidConfig { field, .. }
                if field == "search_activation.quality.producer_revision" =>
            {
                Self::QualityIdentity
            }
            BackendError::UnverifiableRemoteSpace { .. } => Self::UnverifiableIdentity,
            _ => Self::Other,
        }
    }

    fn error(self) -> frankensearch::SearchError {
        use frankensearch::SearchError as BackendError;
        match self {
            Self::Dimension { expected, found } => {
                BackendError::DimensionMismatch { expected, found }
            }
            Self::UnverifiableIdentity => BackendError::UnverifiableRemoteSpace {
                producer: "selected embedding producer".to_owned(),
                reason: "producer identity could not be verified".to_owned(),
            },
            kind => BackendError::InvalidConfig {
                field: match kind {
                    Self::FastIdentity => "search_activation.fast.producer_revision",
                    Self::QualityIdentity => "search_activation.quality.producer_revision",
                    _ => "search.embedding_producer",
                }
                .to_owned(),
                value: "rejected".to_owned(),
                reason: "the embedding producer returned a non-recoverable error; its results were withheld".to_owned(),
            },
        }
    }
}

pub(super) struct ObservedEmbedder {
    inner: Arc<dyn Embedder>,
    failed: OnceLock<()>,
    rejected: OnceLock<ProducerRejection>,
    cancelled: OnceLock<()>,
}

impl ObservedEmbedder {
    pub(super) fn new(inner: Arc<dyn Embedder>) -> Self {
        Self {
            inner,
            failed: OnceLock::new(),
            rejected: OnceLock::new(),
            cancelled: OnceLock::new(),
        }
    }

    fn observe<T>(&self, result: SearchResult<T>) -> SearchResult<T> {
        if let Err(error) = &result {
            if matches!(error, frankensearch::SearchError::Cancelled { .. }) {
                let _ = self.cancelled.set(());
            } else if recoverable(error) {
                let _ = self.failed.set(());
            } else {
                let _ = self.rejected.set(ProducerRejection::from_error(error));
            }
        }
        result
    }

    pub(super) fn needs_recovery<T>(&self, result: &SearchResult<T>) -> bool {
        if self.cancelled.get().is_some() || self.rejected.get().is_some() {
            return false;
        }
        match result {
            Ok(_) => self.failed.get().is_some(),
            Err(error) => recoverable(error),
        }
    }

    /// Validate the complete search outcome before fallback, reranking or
    /// publication. Some backend paths keep lexical results after ANY producer
    /// error. That behavior cannot turn failed identity/dimension validation
    /// or cancellation into a successful ee search (even an empty one).
    pub(super) fn admit_result<T>(&self, result: SearchResult<T>) -> SearchResult<T> {
        if self.cancelled.get().is_some() {
            return Err(frankensearch::SearchError::Cancelled {
                phase: "embedding admission".to_owned(),
                reason: "the embedding producer cancelled this request".to_owned(),
            });
        }
        // A direct integrity/query/I/O failure stays the original failure.
        // Earlier inference trouble never hides a later index failure.
        if result.as_ref().is_err_and(|error| !recoverable(error)) {
            return result;
        }
        if let Some(rejected) = self.rejected.get() {
            return Err(rejected.error());
        }
        result
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
        self.observe(self.inner.identity())
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
        self.observe(self.inner.truncate_embedding(embedding, target_dim))
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
        rejection: Option<RejectionKind>,
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
                    Err(self.rejection.map_or_else(inference_error, rejection_error))
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
    fn exercise_runtime_recovery(
        lexical_available: bool,
        rejection: Option<RejectionKind>,
    ) -> Result<(), String> {
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
            rejection,
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
            for source in [
                SearchSourceMode::LexicalOnly,
                SearchSourceMode::SemanticOnly,
                SearchSourceMode::Hybrid,
            ] {
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
                    if source == SearchSourceMode::LexicalOnly {
                        if lexical_available {
                            let actual = result.map_err(|error| error.to_string())?;
                            assert_eq!(actual.applied, SearchSourceMode::LexicalOnly);
                            assert!(actual.degraded.is_empty());
                            assert_eq!(actual.hits.len(), usize::from(query == "quasarneedle"));
                            if let Some(hit) = actual.hits.first() {
                                assert_eq!(hit.doc_id, "mem_51000000000000000000000001");
                            }
                        } else {
                            assert!(result.is_err());
                        }
                        continue;
                    }
                    if let Some(rejection) = rejection {
                        let error = result.err().ok_or_else(|| {
                            format!("{rejection:?} became successful {source:?} retrieval")
                        })?;
                        if matches!(rejection, RejectionKind::Cancelled) {
                            assert!(matches!(error, SearchError::Cancelled(_)), "{error}");
                        } else {
                            assert!(!matches!(error, SearchError::SourceModeUnavailable { .. }));
                        }
                        continue;
                    }
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
        exercise_runtime_recovery(true, None)
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn missing_lexical_index_is_not_reported_as_successful_empty_recovery() -> Result<(), String> {
        exercise_runtime_recovery(false, None)
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

    #[derive(Clone, Copy, Debug)]
    enum RejectionKind {
        Dimension,
        FastIdentity,
        QualityIdentity,
        UnverifiableIdentity,
        MalformedResponse,
        Cancelled,
    }

    const REJECTIONS: [RejectionKind; 6] = [
        RejectionKind::Dimension,
        RejectionKind::FastIdentity,
        RejectionKind::QualityIdentity,
        RejectionKind::UnverifiableIdentity,
        RejectionKind::MalformedResponse,
        RejectionKind::Cancelled,
    ];

    fn rejection_error(kind: RejectionKind) -> BackendError {
        match kind {
            RejectionKind::Dimension => BackendError::DimensionMismatch {
                expected: 256,
                found: 128,
            },
            RejectionKind::UnverifiableIdentity => BackendError::UnverifiableRemoteSpace {
                producer: "private-producer-canary".to_owned(),
                reason: "private-identity-canary".to_owned(),
            },
            RejectionKind::Cancelled => BackendError::Cancelled {
                phase: "private-phase-canary".to_owned(),
                reason: "private-cancellation-canary".to_owned(),
            },
            kind => BackendError::InvalidConfig {
                field: match kind {
                    RejectionKind::FastIdentity => "search_activation.fast.producer_revision",
                    RejectionKind::QualityIdentity => "search_activation.quality.producer_revision",
                    _ => "remote_embedding.response",
                }
                .to_owned(),
                value: "private-response-canary".to_owned(),
                reason: "private-validation-canary".to_owned(),
            },
        }
    }

    #[test]
    fn swallowed_producer_rejections_cannot_become_success_or_lexical_recovery() {
        for kind in REJECTIONS {
            let observed = observer();
            let original = rejection_error(kind);
            let before = original.to_string();
            let forwarded = observed.observe::<()>(Err(original)).unwrap_err();
            assert_eq!(
                forwarded.to_string(),
                before,
                "producer errors pass through"
            );
            for response in [Ok(vec![1]), Ok(Vec::new()), Err(inference_error())] {
                assert!(!observed.needs_recovery(&response));
                let error = observed.admit_result(response).unwrap_err();
                assert!(!recoverable(&error), "{kind:?}");
                let diagnostic = format!("{error:?}");
                assert!(!diagnostic.contains("private-"), "{kind:?}: {diagnostic}");
            }
            assert!(
                observer().admit_result(Ok(17)).is_ok(),
                "request-local state"
            );
        }
    }

    #[test]
    fn permanent_failure_wins_over_inference_failure_in_either_order() {
        for kind in REJECTIONS {
            for permanent_first in [false, true] {
                let observed = observer();
                let failures = if permanent_first {
                    [rejection_error(kind), inference_error()]
                } else {
                    [inference_error(), rejection_error(kind)]
                };
                for error in failures {
                    assert!(observed.observe::<()>(Err(error)).is_err());
                }
                assert!(!observed.needs_recovery(&Ok(())));
                assert!(observed.admit_result(Ok(())).is_err());
                assert!(observed.admit_result::<()>(Err(inference_error())).is_err());
            }
        }
    }

    #[test]
    fn cancellation_dominates_other_observed_errors_without_exporting_its_reason() {
        for cancel_first in [false, true] {
            let observed = observer();
            let kinds = if cancel_first {
                [RejectionKind::Cancelled, RejectionKind::Dimension]
            } else {
                [RejectionKind::Dimension, RejectionKind::Cancelled]
            };
            for kind in kinds {
                let _ = observed.observe::<()>(Err(rejection_error(kind)));
            }
            let result = observed.admit_result::<()>(Err(inference_error()));
            assert!(matches!(result, Err(BackendError::Cancelled { .. })));
            assert!(!format!("{result:?}").contains("private-"));
        }
    }

    #[test]
    fn suppressed_dimension_and_identity_errors_keep_their_typed_repair_paths() {
        for kind in REJECTIONS {
            let observed = observer();
            let _ = observed.observe::<()>(Err(rejection_error(kind)));
            let error = observed.admit_result(Ok(())).unwrap_err();
            match kind {
                RejectionKind::Dimension => assert!(matches!(
                    error,
                    BackendError::DimensionMismatch {
                        expected: 256,
                        found: 128
                    }
                )),
                RejectionKind::FastIdentity | RejectionKind::QualityIdentity => {
                    let BackendError::InvalidConfig { field, .. } = error else {
                        panic!("lost identity repair class");
                    };
                    assert_eq!(
                        field,
                        match kind {
                            RejectionKind::FastIdentity =>
                                "search_activation.fast.producer_revision",
                            _ => "search_activation.quality.producer_revision",
                        }
                    );
                }
                RejectionKind::UnverifiableIdentity => assert!(matches!(
                    error,
                    BackendError::UnverifiableRemoteSpace { .. }
                )),
                RejectionKind::Cancelled => {
                    assert!(matches!(error, BackendError::Cancelled { .. }))
                }
                RejectionKind::MalformedResponse => assert!(!recoverable(&error)),
            }
        }
    }

    #[test]
    fn unclassified_producer_failures_also_fail_closed() {
        for error in [
            BackendError::Io(std::io::Error::other("private-io-canary")),
            BackendError::IndexCorrupted {
                path: "private-path".into(),
                detail: "private-body".into(),
            },
            BackendError::QueryParseError {
                query: "private-query".into(),
                detail: "private-body".into(),
            },
            BackendError::SearchTimeout {
                elapsed_ms: 30,
                budget_ms: 20,
            },
        ] {
            let observed = observer();
            let _ = observed.observe::<()>(Err(error));
            let refused = observed.admit_result(Ok(())).unwrap_err();
            assert!(!recoverable(&refused));
            assert!(!format!("{refused:?}").contains("private-"));
        }
    }

    #[test]
    fn unsuppressed_terminal_errors_and_healthy_results_are_unchanged() {
        let observed = observer();
        let _ = observed.observe::<()>(Err(inference_error()));
        for kind in REJECTIONS {
            let error = rejection_error(kind);
            let before = error.to_string();
            assert_eq!(
                observed
                    .admit_result::<()>(Err(error))
                    .unwrap_err()
                    .to_string(),
                before
            );
        }
        let healthy = observer();
        assert_eq!(healthy.admit_result(Ok(vec![3, 2, 1])).unwrap(), [3, 2, 1]);
        assert!(
            healthy
                .admit_result(Ok(Vec::<u8>::new()))
                .unwrap()
                .is_empty()
        );
        let outage = observed.admit_result(Ok(()));
        assert!(outage.is_ok() && observed.needs_recovery(&outage));
    }

    #[test]
    fn concurrent_producer_errors_cannot_clear_the_request_refusal() {
        let observed = observer();
        std::thread::scope(|scope| {
            for kind in REJECTIONS {
                let observed = &observed;
                scope.spawn(move || {
                    let _ = observed.observe::<()>(Err(rejection_error(kind)));
                    let _ = observed.observe::<()>(Err(inference_error()));
                    let _ = observed.observe(Ok(()));
                });
            }
        });
        assert!(!observed.needs_recovery(&Ok(())));
        assert!(matches!(
            observed.admit_result(Ok(())),
            Err(BackendError::Cancelled { .. })
        ));
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn real_search_never_publishes_hits_after_a_producer_rejection() -> Result<(), String> {
        for kind in REJECTIONS {
            for lexical in [false, true] {
                exercise_runtime_recovery(lexical, Some(kind))?;
            }
        }
        Ok(())
    }

    struct RejectedProducer(crate::search::HashEmbedder);

    impl Embedder for RejectedProducer {
        fn embed<'a>(
            &'a self,
            _cx: &'a asupersync::Cx,
            _text: &'a str,
        ) -> SearchFuture<'a, Vec<f32>> {
            Box::pin(async { Err(rejection_error(RejectionKind::Dimension)) })
        }

        fn identity(&self) -> SearchResult<&EmbeddingIdentityBundleV1> {
            Err(rejection_error(RejectionKind::UnverifiableIdentity))
        }

        fn truncate_embedding(
            &self,
            _embedding: &[f32],
            _target_dim: usize,
        ) -> SearchResult<Vec<f32>> {
            Err(rejection_error(RejectionKind::MalformedResponse))
        }

        fn dimension(&self) -> usize {
            self.0.dimension()
        }
        fn id(&self) -> &str {
            self.0.id()
        }
        fn model_name(&self) -> &str {
            self.0.model_name()
        }
        fn is_semantic(&self) -> bool {
            true
        }
        fn category(&self) -> ModelCategory {
            self.0.category()
        }
    }

    fn rejected_producer() -> ObservedEmbedder {
        ObservedEmbedder::new(Arc::new(RejectedProducer(
            crate::search::HashEmbedder::default_256(),
        )))
    }

    #[test]
    fn raw_and_bound_embedding_entrypoints_record_rejections() -> Result<(), String> {
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(10), |cx| async move {
            for entrypoint in 0..4 {
                let observed = rejected_producer();
                let texts = ["first", "second"];
                let result = match entrypoint {
                    0 => observed.embed(&cx, texts[0]).await.map(|_| ()),
                    1 => observed.embed_batch(&cx, &texts).await.map(|_| ()),
                    2 => observed.embed_bound(&cx, texts[0]).await.map(|_| ()),
                    _ => observed.embed_batch_bound(&cx, &texts).await.map(|_| ()),
                };
                assert!(result.is_err(), "entrypoint {entrypoint}");
                assert!(!observed.needs_recovery(&Ok(())));
                assert!(observed.admit_result(Ok(())).is_err());
            }
        })
        .map_err(|error| error.to_string())
    }

    #[test]
    fn identity_and_truncation_entrypoints_cannot_bypass_observation() {
        let identity = rejected_producer();
        assert!(identity.identity().is_err());
        assert!(matches!(
            identity.admit_result(Ok(())),
            Err(BackendError::UnverifiableRemoteSpace { .. })
        ));
        let truncation = rejected_producer();
        assert!(truncation.truncate_embedding(&[1.0, 2.0], 1).is_err());
        assert!(!truncation.needs_recovery(&Ok(())));
        assert!(truncation.admit_result(Ok(())).is_err());
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
