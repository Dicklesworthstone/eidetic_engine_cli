//! Local semantic span scoring for extractive answers (ADR 0067).
//!
//! Build one complete query-specific score table before the admission cap.
//! Partial model results never mix scoring regimes. Only a verified, already
//! installed local model may run here: no downloads, remote requests, index
//! hydration, migrations, or model-registry writes belong to an ask read.

use std::collections::{BTreeMap, BTreeSet};

use asupersync::Cx;
use frankensearch::SearchError;
use frankensearch::core::generation::EmbeddingIdentityBundleV1;
use frankensearch::core::traits::IdentityBoundEmbedding;

use crate::db::DbConnection;
use crate::models::DomainError;
use crate::search::Embedder;

use super::{
    AskCandidate, AskReport, AskRequest, SPAN_W1_LEXICAL, SPAN_W2_SEMANTIC, SPAN_W3_TRUST,
    evaluate_ask, evaluate_ask_scored, jaccard_similarity, native, question_coverage,
    segment_spans, selection, tokenize_for_ask, trust_tilt,
};

const EMBEDDING_BATCH_SIZE: usize = 64;
// Standalone library callers need a bounded production context, not an
// ambient context left over from a CLI or transport. Enclosing requests retain
// their own deadline and cancellation capability instead of receiving this one.
const STANDALONE_SCORING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SemanticFailure {
    Unavailable,
    Cancelled,
}

/// Evaluate an already-admitted corpus with the workspace's cached local model.
///
/// The caller must obtain `candidates` and native metadata from the same scoped
/// source snapshot. Model similarity is relevance, never authority. The pure
/// [`evaluate_ask`] entry point remains explicitly lexical for deterministic
/// evaluators and callers that do not select a workspace/model.
///
/// Missing, rejected, failed, or malformed models fall back to the complete
/// lexical evaluation with `semantic_degraded = true`. Cancellation withholds
/// the operation instead of being turned into an ordinary corpus miss.
/// Query and span outputs must retain one complete immutable embedding
/// identity, not just a model name and dimension. A detected change discards
/// all intermediate semantic scores. Batching is used only when the producer
/// guarantees that its batch and single-output identity contracts agree.
pub fn evaluate_ask_with_local_model(
    connection: &DbConnection,
    workspace_id: &str,
    request: &AskRequest,
    candidates: &[AskCandidate],
) -> Result<AskReport, DomainError> {
    // Validate the entire input before sending even local inference any body.
    // A zero admission budget still executes the existing identity scan.
    if !super::request_is_valid(request)
        || !native::validate_sources(request, candidates)
        || selection::select_candidates(request, &[], candidates, 0).is_err()
    {
        return Ok(evaluate_ask(request, candidates));
    }
    if candidates.is_empty() || tokenize_for_ask(&request.question).is_empty() {
        return Ok(evaluate_ask(request, candidates));
    }

    // Preserve an enclosing request's cancellation capability when the CLI
    // handler is called by an agent transport. A standalone invocation uses
    // the context installed by the established runtime bridge.
    let caller_cx = Cx::current();
    if caller_cx.as_ref().is_some_and(|cx| checkpoint(cx).is_err()) {
        return finish_evaluation(request, candidates, Err(SemanticFailure::Cancelled));
    }
    let selected = crate::core::index::local_read_only_embedder(connection, workspace_id);
    // A cancelled model load must not become a successful lexical answer just
    // because model selection returned unavailable rather than an embedder.
    if caller_cx.as_ref().is_some_and(|cx| checkpoint(cx).is_err()) {
        return finish_evaluation(request, candidates, Err(SemanticFailure::Cancelled));
    }
    let embedder = match selected {
        Ok(Some(embedder)) => embedder,
        Ok(None) | Err(_) => return Ok(evaluate_ask(request, candidates)),
    };
    evaluate_with_prepared_model(request, candidates, embedder.as_ref(), caller_cx)
}

fn evaluate_with_prepared_model(
    request: &AskRequest,
    candidates: &[AskCandidate],
    embedder: &dyn Embedder,
    caller_cx: Option<Cx>,
) -> Result<AskReport, DomainError> {
    let result =
        crate::core::run_cli_with_cx(STANDALONE_SCORING_TIMEOUT, |runtime_cx| async move {
            // run_cli_future drives a future but does not mint a request context.
            // Looking up Cx::current inside it made every standalone invocation
            // silently lexical, even with a verified local model already selected.
            // Never replace an existing caller's cancelled context with a fresh one.
            let cx = caller_cx.unwrap_or(runtime_cx);
            // Dependencies that consult the active context must see the same
            // cancellation and capabilities as the explicit inference argument,
            // never the bridge's fresh bootstrap context in place of the caller.
            let _ambient = Cx::set_current(Some(cx.clone()));
            SemanticScores::build(&cx, &request.question, candidates, embedder).await
        });
    finish_evaluation(
        request,
        candidates,
        result.unwrap_or(Err(SemanticFailure::Unavailable)),
    )
}

fn finish_evaluation(
    request: &AskRequest,
    candidates: &[AskCandidate],
    result: Result<SemanticScores<'_>, SemanticFailure>,
) -> Result<AskReport, DomainError> {
    match result {
        Ok(scores) => Ok(evaluate_ask_scored(
            request,
            candidates,
            &|terms, text, confidence, trust| scores.score(terms, text, confidence, trust),
            false,
        )),
        Err(SemanticFailure::Cancelled) => Err(DomainError::Storage {
            message: "Ask semantic scoring was cancelled; answer withheld".to_owned(),
            repair: Some("Retry ee ask when the operation is no longer cancelled.".to_owned()),
        }),
        Err(SemanticFailure::Unavailable) => Ok(evaluate_ask(request, candidates)),
    }
}

/// Freeze the complete producer, space, input and storage contract. Keep the
/// descriptor checks too, but never manufacture compatibility from names or
/// dimensions. These checks supplement local artifact admission; an embedder
/// that lies about its own immutable identity still violates that boundary.
struct SemanticSpace {
    identity: EmbeddingIdentityBundleV1,
    id: String,
    model_name: String,
    dimension: usize,
}

impl SemanticSpace {
    fn capture(cx: &Cx, embedder: &dyn Embedder) -> Result<Self, SemanticFailure> {
        let identity = embedder.identity().cloned();
        checkpoint(cx)?;
        let identity = identity.map_err(|_| SemanticFailure::Unavailable)?;
        identity
            .validate()
            .map_err(|_| SemanticFailure::Unavailable)?;
        let space = Self {
            identity,
            id: embedder.id().to_owned(),
            model_name: embedder.model_name().to_owned(),
            dimension: embedder.dimension(),
        };
        space.check(cx, embedder)?;
        Ok(space)
    }

    fn check(&self, cx: &Cx, embedder: &dyn Embedder) -> Result<(), SemanticFailure> {
        let consistent = !self.id.trim().is_empty()
            && self.dimension != 0
            && embedder.is_semantic()
            && embedder.dimension() == self.dimension
            && embedder.id() == self.id.as_str()
            && embedder.model_name() == self.model_name.as_str()
            && usize::try_from(self.identity.space.dimension).ok() == Some(self.dimension)
            && embedder
                .identity()
                .is_ok_and(|identity| identity == &self.identity);
        // Identity-aware adapters may discover cancellation while checking
        // their state. Preserve it even when that check reports another error.
        checkpoint(cx)?;
        if !consistent {
            // Descriptors and identity fields can contain private paths. Do
            // not expose either frozen/changed values or provider errors.
            return Err(SemanticFailure::Unavailable);
        }
        Ok(())
    }

    fn validate_output(&self, output: &IdentityBoundEmbedding) -> Result<(), SemanticFailure> {
        // A custom bound implementation can override the default trait guard.
        // Validate its actual response before any cosine comparison or reuse.
        if output.identity != self.identity {
            return Err(SemanticFailure::Unavailable);
        }
        output.validate().map_err(|_| SemanticFailure::Unavailable)
    }
}

async fn embed_one(
    cx: &Cx,
    embedder: &dyn Embedder,
    space: &SemanticSpace,
    text: &str,
) -> Result<Vec<f32>, SemanticFailure> {
    space.check(cx, embedder)?;
    let outcome = embedder.embed_bound(cx, text).await;
    checkpoint(cx)?;
    let output = outcome.map_err(|error| inference_failure(cx, &error))?;
    space.check(cx, embedder)?;
    space.validate_output(&output)?;
    Ok(output.values)
}

/// Preserve the producer's single-output semantics unless it explicitly
/// guarantees native bound-batch equivalence. Move validated value buffers;
/// retain no duplicate coordinate matrix or identity metadata in the scorer.
async fn embed_spans(
    cx: &Cx,
    embedder: &dyn Embedder,
    space: &SemanticSpace,
    texts: &[&str],
) -> Result<Vec<Vec<f32>>, SemanticFailure> {
    space.check(cx, embedder)?;
    let native_batch = embedder.bound_batch_is_native();
    checkpoint(cx)?;
    if !native_batch {
        let mut values = Vec::with_capacity(texts.len());
        for text in texts {
            values.push(embed_one(cx, embedder, space, text).await?);
        }
        return Ok(values);
    }
    let outcome = embedder.embed_batch_bound(cx, texts).await;
    checkpoint(cx)?;
    let outputs = outcome.map_err(|error| inference_failure(cx, &error))?;
    space.check(cx, embedder)?;
    if outputs.len() != texts.len() {
        return Err(SemanticFailure::Unavailable);
    }
    outputs
        .into_iter()
        .map(|output| {
            checkpoint(cx)?;
            space.validate_output(&output)?;
            Ok(output.values)
        })
        .collect()
}

/// Borrow the admitted bytes. Retain scalars, not a corpus-sized vector matrix.
/// Equal text can reuse inference, but its different source identities, trust,
/// confidence and derivation groups remain distinct throughout composition.
struct SemanticScores<'a> {
    by_text: BTreeMap<&'a str, f32>,
}

impl<'a> SemanticScores<'a> {
    async fn build(
        cx: &Cx,
        question: &str,
        candidates: &'a [AskCandidate],
        embedder: &dyn Embedder,
    ) -> Result<Self, SemanticFailure> {
        checkpoint(cx)?;
        let space = SemanticSpace::capture(cx, embedder)?;
        let query = embed_one(cx, embedder, &space, question).await?;
        let query_norm = vector_norm_squared(&query, space.dimension)
            .filter(|norm| *norm > 0.0)
            .ok_or(SemanticFailure::Unavailable)?;

        let texts: BTreeSet<&'a str> = candidates
            .iter()
            .flat_map(|candidate| {
                segment_spans(&candidate.content)
                    .into_iter()
                    .map(move |(start, end)| &candidate.content[start..end])
            })
            .collect();
        if texts.is_empty() {
            return Err(SemanticFailure::Unavailable);
        }
        let texts: Vec<_> = texts.into_iter().collect();
        let mut by_text = BTreeMap::new();
        for batch in texts.chunks(EMBEDDING_BATCH_SIZE) {
            let vectors = embed_spans(cx, embedder, &space, batch).await?;
            append_batch(&mut by_text, batch, &vectors, &query, query_norm)?;
        }
        space.check(cx, embedder)?;
        Ok(Self { by_text })
    }

    fn score(&self, terms: &[String], text: &str, confidence: f32, trust: &str) -> f32 {
        // The table covers every segment before selection and composition.
        // An unexpected lookup cannot gain semantic evidence from another span.
        let similarity = self.by_text.get(text).copied().unwrap_or(0.0);
        let span_terms = tokenize_for_ask(text);
        let lexical = 0.5 * question_coverage(terms, &span_terms)
            + 0.5 * jaccard_similarity(terms, &span_terms);
        (SPAN_W1_LEXICAL * lexical
            + SPAN_W2_SEMANTIC * similarity
            + SPAN_W3_TRUST * (confidence * trust_tilt(trust)))
        .clamp(0.0, 1.0)
    }
}

fn checkpoint(cx: &Cx) -> Result<(), SemanticFailure> {
    cx.checkpoint().map_err(|_| SemanticFailure::Cancelled)
}

fn inference_failure(cx: &Cx, error: &SearchError) -> SemanticFailure {
    if matches!(error, SearchError::Cancelled { .. }) || checkpoint(cx).is_err() {
        SemanticFailure::Cancelled
    } else {
        SemanticFailure::Unavailable
    }
}

/// Reject missing/extra, wrong-space, and non-finite vectors before publishing
/// any score in the batch. A later failed batch discards the request's table.
fn append_batch<'a>(
    scores: &mut BTreeMap<&'a str, f32>,
    texts: &[&'a str],
    vectors: &[Vec<f32>],
    query: &[f32],
    query_norm: f64,
) -> Result<(), SemanticFailure> {
    if texts.len() != vectors.len() {
        return Err(SemanticFailure::Unavailable);
    }
    let scored: Result<Vec<_>, _> = texts
        .iter()
        .copied()
        .zip(vectors)
        .map(|(text, vector)| {
            cosine(query, query_norm, vector)
                .map(|score| (text, score))
                .ok_or(SemanticFailure::Unavailable)
        })
        .collect();
    scores.extend(scored?);
    Ok(())
}

fn vector_norm_squared(vector: &[f32], dimension: usize) -> Option<f64> {
    if dimension == 0 || vector.len() != dimension || !vector.iter().all(|v| v.is_finite()) {
        return None;
    }
    // f64 intermediates keep even finite f32::MAX components from overflowing.
    Some(vector.iter().map(|v| f64::from(*v).powi(2)).sum())
}

fn cosine(query: &[f32], query_norm: f64, vector: &[f32]) -> Option<f32> {
    if !query_norm.is_finite() || query_norm <= 0.0 {
        return None;
    }
    let norm = vector_norm_squared(vector, query.len())?;
    if norm == 0.0 {
        // Out-of-vocabulary spans have no semantic support, not positive 0.5
        // support from shifting the cosine range to [0, 1].
        return Some(0.0);
    }
    let dot: f64 = query
        .iter()
        .zip(vector)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum();
    let similarity = dot / (query_norm.sqrt() * norm.sqrt());
    similarity
        .is_finite()
        .then(|| similarity.clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
#[path = "ask_semantic_tests.rs"]
mod tests;

#[cfg(test)]
mod runtime_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // Execute real hash inference to test the runtime boundary without a model
    // download. The semantic flag selects the scoring branch under test; this
    // fixture does not claim a neural model was loaded or prove neural quality.
    struct RuntimeProbe {
        hash: crate::search::HashEmbedder,
        calls: AtomicUsize,
        cancel_after_query: bool,
    }

    impl RuntimeProbe {
        fn new(cancel_after_query: bool) -> Self {
            Self {
                hash: crate::search::HashEmbedder::default_256(),
                calls: AtomicUsize::new(0),
                cancel_after_query,
            }
        }
    }

    impl Embedder for RuntimeProbe {
        fn embed<'a>(
            &'a self,
            cx: &'a Cx,
            text: &'a str,
        ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
            Box::pin(async move {
                assert!(
                    Cx::current().is_some(),
                    "the bridge must install its context"
                );
                self.calls.fetch_add(1, Ordering::SeqCst);
                let result = self.hash.embed(cx, text).await;
                if self.cancel_after_query {
                    cx.set_cancel_reason(asupersync::CancelReason::user("private-cancel-reason"));
                    assert!(
                        Cx::current().is_some_and(|active| active.checkpoint().is_err()),
                        "explicit and active contexts must share cancellation"
                    );
                }
                result
            })
        }

        fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
            self.hash.identity()
        }
        fn bound_batch_is_native(&self) -> bool {
            true
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
        fn category(&self) -> frankensearch::ModelCategory {
            self.hash.category()
        }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum SpaceChange {
        Never,
        Query,
        Batch(usize),
    }

    // Real hash inference with a controlled descriptor transition. As with
    // RuntimeProbe, the semantic flag tests the runtime/scoring boundary only;
    // this fixture is not evidence of a loaded or evaluated neural model.
    struct DriftingProbe {
        hash: crate::search::HashEmbedder,
        initial_id: &'static str,
        change_at: SpaceChange,
        name_only: bool,
        cancel_on_change: bool,
        changed: AtomicBool,
        query_calls: AtomicUsize,
        batch_calls: AtomicUsize,
    }

    impl DriftingProbe {
        fn new(change_at: SpaceChange) -> Self {
            Self {
                hash: crate::search::HashEmbedder::default_256(),
                initial_id: "runtime-test-space",
                change_at,
                name_only: false,
                cancel_on_change: false,
                changed: AtomicBool::new(false),
                query_calls: AtomicUsize::new(0),
                batch_calls: AtomicUsize::new(0),
            }
        }

        fn finish_inference(&self, cx: &Cx, stage: SpaceChange) {
            if self.change_at == stage {
                self.changed.store(true, Ordering::SeqCst);
                if self.cancel_on_change {
                    cx.set_cancel_reason(asupersync::CancelReason::user(
                        "PRIVATE-SPACE-CANCEL-CANARY",
                    ));
                }
            }
        }
    }

    impl Embedder for DriftingProbe {
        fn embed<'a>(
            &'a self,
            cx: &'a Cx,
            text: &'a str,
        ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
            Box::pin(async move {
                self.query_calls.fetch_add(1, Ordering::SeqCst);
                let result = self.hash.embed(cx, text).await;
                self.finish_inference(cx, SpaceChange::Query);
                result
            })
        }

        fn embed_batch<'a>(
            &'a self,
            cx: &'a Cx,
            texts: &'a [&'a str],
        ) -> frankensearch::SearchFuture<'a, Vec<Vec<f32>>> {
            Box::pin(async move {
                let batch = self.batch_calls.fetch_add(1, Ordering::SeqCst) + 1;
                let result = self.hash.embed_batch(cx, texts).await;
                self.finish_inference(cx, SpaceChange::Batch(batch));
                result
            })
        }

        fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
            self.hash.identity()
        }
        fn bound_batch_is_native(&self) -> bool {
            true
        }
        fn dimension(&self) -> usize {
            self.hash.dimension()
        }

        fn id(&self) -> &str {
            if self.changed.load(Ordering::SeqCst) && !self.name_only {
                "PRIVATE-MODEL-ID-CANARY"
            } else {
                self.initial_id
            }
        }

        fn model_name(&self) -> &str {
            if self.changed.load(Ordering::SeqCst) && self.name_only {
                "PRIVATE-MODEL-NAME-CANARY"
            } else {
                self.hash.model_name()
            }
        }

        fn is_semantic(&self) -> bool {
            true
        }

        fn category(&self) -> frankensearch::ModelCategory {
            self.hash.category()
        }
    }

    fn fixture() -> (AskRequest, Vec<AskCandidate>) {
        let text = "Run cargo fmt on source before release.";
        (
            AskRequest {
                question: text.to_owned(),
                min_confidence: 0.4,
                ..AskRequest::default()
            },
            vec![AskCandidate {
                memory_id: "runtime-source".to_owned(),
                content: text.to_owned(),
                confidence: 1.0,
                trust_class: "human_explicit".to_owned(),
                provenance_uri: Some("manual://runtime-test".to_owned()),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                team_provenance: None,
            }],
        )
    }

    #[test]
    fn standalone_scoring_runs_inference_without_an_ambient_context() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = fixture();
        let model = RuntimeProbe::new(false);
        let report = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
        assert!(!report.semantic_degraded);
        assert!(!report.abstained);
        assert_eq!(report.citations.len(), 1);
        assert_eq!(report.citations[0].text, rows[0].content);
        assert!(
            model.calls.load(Ordering::SeqCst) >= 2,
            "query and source inference"
        );
        assert!(
            Cx::current().is_none(),
            "no ambient context leaks to the caller"
        );
    }

    #[test]
    fn existing_caller_cancellation_is_not_replaced_by_a_new_budget() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = fixture();
        let model = RuntimeProbe::new(false);
        let caller = Cx::for_testing();
        caller.set_cancel_reason(asupersync::CancelReason::user("private-caller-reason"));
        let error = evaluate_with_prepared_model(&request, &rows, &model, Some(caller.clone()))
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(!error.to_string().contains("private-caller-reason"));
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert!(caller.checkpoint().is_err());
        assert!(Cx::current().is_none());
    }

    #[test]
    fn cancellation_after_query_inference_withholds_the_answer_and_restores_context() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = fixture();
        let model = RuntimeProbe::new(true);
        let error = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(!error.to_string().contains("private-cancel-reason"));
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        assert!(Cx::current().is_none());
    }

    #[test]
    fn a_nonsemantic_model_still_uses_the_complete_lexical_evaluation() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = fixture();
        let hash = crate::search::HashEmbedder::default_256();
        let actual = evaluate_with_prepared_model(&request, &rows, &hash, None).unwrap();
        let expected = evaluate_ask(&request, &rows);
        assert!(actual.semantic_degraded);
        assert_eq!(
            super::super::ask_data_json(&actual),
            super::super::ask_data_json(&expected)
        );
        assert!(Cx::current().is_none());
    }

    #[test]
    fn concurrent_standalone_calls_own_independent_contexts_and_identical_answers() {
        let model = RuntimeProbe::new(false);
        let reports = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let model = &model;
                    scope.spawn(move || {
                        assert!(Cx::current().is_none());
                        let (request, rows) = fixture();
                        let report =
                            evaluate_with_prepared_model(&request, &rows, model, None).unwrap();
                        assert!(!report.semantic_degraded);
                        assert!(Cx::current().is_none());
                        super::super::ask_data_json(&report)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(reports.len(), 4);
        assert!(reports.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(model.calls.load(Ordering::SeqCst) >= 8);
    }

    #[test]
    fn caller_context_is_active_during_inference_and_the_previous_context_is_restored() {
        let previous = Cx::for_testing();
        let _ambient = Cx::set_current(Some(previous.clone()));
        let caller = Cx::for_testing();
        let (request, rows) = fixture();
        let model = RuntimeProbe::new(true);
        let error = evaluate_with_prepared_model(&request, &rows, &model, Some(caller.clone()))
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(caller.checkpoint().is_err());
        assert!(previous.checkpoint().is_ok());
        assert!(Cx::current().is_some_and(|active| active.checkpoint().is_ok()));
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    }

    fn multi_batch_fixture() -> (AskRequest, Vec<AskCandidate>) {
        let (request, source) = fixture();
        let rows: Vec<_> = (0..EMBEDDING_BATCH_SIZE * 2 + 1)
            .map(|index| {
                let mut row = source[0].clone();
                row.memory_id = format!("runtime-source-{index}");
                row.content = format!("Run cargo fmt on source before release variant{index}.");
                row
            })
            .collect();
        let texts: BTreeSet<_> = rows
            .iter()
            .flat_map(|row| {
                segment_spans(&row.content)
                    .into_iter()
                    .map(move |(start, end)| &row.content[start..end])
            })
            .collect();
        assert_eq!(texts.len(), EMBEDDING_BATCH_SIZE * 2 + 1);
        (request, rows)
    }

    fn assert_complete_lexical_fallback(
        request: &AskRequest,
        rows: &[AskCandidate],
        actual: &AskReport,
    ) {
        assert!(actual.semantic_degraded);
        let output = super::super::ask_data_json(actual);
        assert_eq!(
            output,
            super::super::ask_data_json(&evaluate_ask(request, rows))
        );
        let text = output.to_string();
        assert!(!text.contains("PRIVATE-MODEL-ID-CANARY"));
        assert!(!text.contains("PRIVATE-MODEL-NAME-CANARY"));
        assert!(!text.contains("PRIVATE-IDENTITY"));
    }

    #[test]
    fn stable_model_space_preserves_semantic_scores_across_all_batches() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        let model = DriftingProbe::new(SpaceChange::Never);
        let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
        let reference = RuntimeProbe::new(false);
        let expected = evaluate_with_prepared_model(&request, &rows, &reference, None).unwrap();
        assert!(!actual.semantic_degraded);
        assert_eq!(
            super::super::ask_data_json(&actual),
            super::super::ask_data_json(&expected)
        );
        assert_eq!(model.query_calls.load(Ordering::SeqCst), 1);
        assert_eq!(model.batch_calls.load(Ordering::SeqCst), 3);
        assert!(Cx::current().is_none());
    }

    #[test]
    fn same_dimension_model_change_after_query_stops_before_source_inference() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        let model = DriftingProbe::new(SpaceChange::Query);
        let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
        assert_complete_lexical_fallback(&request, &rows, &actual);
        assert_eq!(model.dimension(), 256);
        assert!(model.is_semantic());
        assert_eq!(model.query_calls.load(Ordering::SeqCst), 1);
        assert_eq!(model.batch_calls.load(Ordering::SeqCst), 0);
        assert!(Cx::current().is_none());
    }

    #[test]
    fn later_model_drift_discards_every_previously_computed_semantic_score() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        for changed_batch in [1, 2, 3] {
            let model = DriftingProbe::new(SpaceChange::Batch(changed_batch));
            let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
            assert_complete_lexical_fallback(&request, &rows, &actual);
            assert_eq!(model.query_calls.load(Ordering::SeqCst), 1);
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), changed_batch);
            assert!(Cx::current().is_none());
        }
    }

    #[test]
    fn changed_model_name_cannot_keep_semantic_posture_under_an_unchanged_id() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        for stage in [SpaceChange::Query, SpaceChange::Batch(2)] {
            let mut model = DriftingProbe::new(stage);
            model.name_only = true;
            let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
            assert_complete_lexical_fallback(&request, &rows, &actual);
            assert_eq!(model.id(), model.initial_id);
            assert_eq!(model.dimension(), 256);
            let expected_batches = if stage == SpaceChange::Query { 0 } else { 2 };
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), expected_batches);
        }
        assert!(Cx::current().is_none());
    }

    #[test]
    fn missing_model_identity_is_refused_before_any_inference() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = fixture();
        for blank in ["", " \t\n"] {
            let mut model = DriftingProbe::new(SpaceChange::Never);
            model.initial_id = blank;
            let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
            assert_complete_lexical_fallback(&request, &rows, &actual);
            assert_eq!(model.query_calls.load(Ordering::SeqCst), 0);
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), 0);
        }
        assert!(Cx::current().is_none());
    }

    #[test]
    fn cancellation_wins_over_concurrent_model_identity_drift_without_disclosure() {
        let previous = Cx::for_testing();
        let _ambient = Cx::set_current(Some(previous.clone()));
        let (request, rows) = multi_batch_fixture();
        for stage in [SpaceChange::Query, SpaceChange::Batch(2)] {
            let mut model = DriftingProbe::new(stage);
            model.cancel_on_change = true;
            let caller = Cx::for_testing();
            let error = evaluate_with_prepared_model(&request, &rows, &model, Some(caller.clone()))
                .unwrap_err();
            assert!(error.to_string().contains("cancelled"));
            assert!(!error.to_string().contains("PRIVATE-SPACE-CANCEL-CANARY"));
            assert!(!error.to_string().contains("PRIVATE-MODEL-ID-CANARY"));
            assert!(caller.checkpoint().is_err());
            assert!(previous.checkpoint().is_ok());
            assert!(Cx::current().is_some_and(|active| active.checkpoint().is_ok()));
            let expected_batches = if stage == SpaceChange::Query { 0 } else { 2 };
            assert_eq!(model.query_calls.load(Ordering::SeqCst), 1);
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), expected_batches);
        }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum IdentityFault {
        None,
        Missing,
        Invalid,
        MismatchedDimension,
        Changed(SpaceChange),
        ForeignOutput(SpaceChange),
        ShortBatch,
        ExtraBatch,
        NonfiniteBatch,
        WrongDimensionBatch,
    }

    // Successful vectors come from the real hash provider. Faults affect only
    // contracts or selected response validation; this is not neural-quality
    // evidence. Raw entry points deliberately panic: no bound override may be
    // bypassed by ask, including a producer that cannot attest batch equivalence.
    struct BoundProbe {
        hash: crate::search::HashEmbedder,
        alternate: EmbeddingIdentityBundleV1,
        fault: IdentityFault,
        native_batch: bool,
        cancel_at: SpaceChange,
        changed: AtomicBool,
        single_calls: AtomicUsize,
        batch_calls: AtomicUsize,
    }

    impl BoundProbe {
        fn new(fault: IdentityFault, native_batch: bool) -> Self {
            let hash = crate::search::HashEmbedder::default_256();
            let mut alternate = hash.identity().unwrap().clone();
            alternate.storage.format = "in-memory-PRIVATE-IDENTITY-CANARY".to_owned();
            alternate.validate().unwrap();
            if fault == IdentityFault::Invalid {
                alternate.space.dimension = 0;
            }
            Self {
                hash,
                alternate,
                fault,
                native_batch,
                cancel_at: SpaceChange::Never,
                changed: AtomicBool::new(false),
                single_calls: AtomicUsize::new(0),
                batch_calls: AtomicUsize::new(0),
            }
        }

        fn finish(&self, cx: &Cx, stage: SpaceChange, output: &mut IdentityBoundEmbedding) {
            if self.fault == IdentityFault::Changed(stage) {
                self.changed.store(true, Ordering::SeqCst);
            }
            if self.fault == IdentityFault::ForeignOutput(stage) {
                output.identity = self.alternate.clone();
                // The regression is incompatible valid identity, not simply
                // a malformed descriptor or an invalid vector dimension.
                output.validate().unwrap();
            }
            if self.cancel_at == stage {
                cx.set_cancel_reason(asupersync::CancelReason::user(
                    "PRIVATE-IDENTITY-CANCEL-CANARY",
                ));
            }
        }
    }

    impl Embedder for BoundProbe {
        fn embed<'a>(
            &'a self,
            _cx: &'a Cx,
            _text: &'a str,
        ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
            panic!("ask must call the identity-bound single-output entry point")
        }

        fn embed_batch<'a>(
            &'a self,
            _cx: &'a Cx,
            _texts: &'a [&'a str],
        ) -> frankensearch::SearchFuture<'a, Vec<Vec<f32>>> {
            panic!("ask must never bypass a bound producer with raw batch inference")
        }

        fn embed_bound<'a>(
            &'a self,
            cx: &'a Cx,
            text: &'a str,
        ) -> frankensearch::SearchFuture<'a, IdentityBoundEmbedding> {
            Box::pin(async move {
                let call = self.single_calls.fetch_add(1, Ordering::SeqCst);
                let stage = if call == 0 {
                    SpaceChange::Query
                } else {
                    // In the non-native lane, Batch(n) selects the nth span.
                    SpaceChange::Batch(call)
                };
                let mut output = self.hash.embed_bound(cx, text).await?;
                self.finish(cx, stage, &mut output);
                Ok(output)
            })
        }

        fn embed_batch_bound<'a>(
            &'a self,
            cx: &'a Cx,
            texts: &'a [&'a str],
        ) -> frankensearch::SearchFuture<'a, Vec<IdentityBoundEmbedding>> {
            assert!(self.native_batch, "unattested batch semantics must not run");
            Box::pin(async move {
                let batch = self.batch_calls.fetch_add(1, Ordering::SeqCst) + 1;
                let mut outputs = self.hash.embed_batch_bound(cx, texts).await?;
                if let Some(last) = outputs.last_mut() {
                    self.finish(cx, SpaceChange::Batch(batch), last);
                    match self.fault {
                        IdentityFault::NonfiniteBatch => last.values[0] = f32::NAN,
                        IdentityFault::WrongDimensionBatch => {
                            let _ = last.values.pop();
                        }
                        _ => {}
                    }
                }
                match self.fault {
                    IdentityFault::ShortBatch => {
                        let _ = outputs.pop();
                    }
                    IdentityFault::ExtraBatch => {
                        let extra = outputs[0].clone();
                        outputs.push(extra);
                    }
                    _ => {}
                }
                Ok(outputs)
            })
        }

        fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
            if self.fault == IdentityFault::Missing {
                return Err(SearchError::InvalidConfig {
                    field: "PRIVATE-IDENTITY-CANARY".to_owned(),
                    value: "PRIVATE-IDENTITY-CANARY".to_owned(),
                    reason: "PRIVATE-IDENTITY-CANARY".to_owned(),
                });
            }
            if self.fault == IdentityFault::Invalid || self.changed.load(Ordering::SeqCst) {
                Ok(&self.alternate)
            } else {
                self.hash.identity()
            }
        }
        fn bound_batch_is_native(&self) -> bool {
            self.native_batch
        }
        fn dimension(&self) -> usize {
            self.hash.dimension() + usize::from(self.fault == IdentityFault::MismatchedDimension)
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
        fn category(&self) -> frankensearch::ModelCategory {
            self.hash.category()
        }
    }

    #[test]
    fn bound_single_and_native_batch_paths_preserve_identical_semantic_answers() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        let reference = RuntimeProbe::new(false);
        let expected = evaluate_with_prepared_model(&request, &rows, &reference, None).unwrap();
        for native in [false, true] {
            let model = BoundProbe::new(IdentityFault::None, native);
            let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
            assert!(!actual.semantic_degraded);
            assert_eq!(
                super::super::ask_data_json(&actual),
                super::super::ask_data_json(&expected)
            );
            assert_eq!(
                model.batch_calls.load(Ordering::SeqCst),
                if native { 3 } else { 0 }
            );
            assert_eq!(
                model.single_calls.load(Ordering::SeqCst),
                if native { 1 } else { rows.len() + 1 }
            );
        }
        assert!(Cx::current().is_none());
    }

    #[test]
    fn full_identity_is_required_before_even_query_inference() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = fixture();
        for fault in [
            IdentityFault::Missing,
            IdentityFault::Invalid,
            IdentityFault::MismatchedDimension,
        ] {
            let model = BoundProbe::new(fault, true);
            let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
            assert_complete_lexical_fallback(&request, &rows, &actual);
            assert_eq!(model.single_calls.load(Ordering::SeqCst), 0);
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn identical_descriptors_cannot_hide_full_identity_or_bound_output_drift() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        for stage in [
            SpaceChange::Query,
            SpaceChange::Batch(1),
            SpaceChange::Batch(2),
            SpaceChange::Batch(3),
        ] {
            for fault in [
                IdentityFault::Changed(stage),
                IdentityFault::ForeignOutput(stage),
            ] {
                let model = BoundProbe::new(fault, true);
                let before = (
                    model.id().to_owned(),
                    model.model_name().to_owned(),
                    model.dimension(),
                );
                let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
                assert_complete_lexical_fallback(&request, &rows, &actual);
                assert_eq!(
                    before,
                    (
                        model.id().to_owned(),
                        model.model_name().to_owned(),
                        model.dimension()
                    )
                );
                assert_eq!(model.single_calls.load(Ordering::SeqCst), 1);
                let batches = match stage {
                    SpaceChange::Batch(n) => n,
                    _ => 0,
                };
                assert_eq!(model.batch_calls.load(Ordering::SeqCst), batches);
            }
        }
    }

    #[test]
    fn single_bound_fault_stops_before_the_next_span_without_native_batching() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        for fault in [
            IdentityFault::Changed(SpaceChange::Batch(2)),
            IdentityFault::ForeignOutput(SpaceChange::Batch(2)),
        ] {
            let model = BoundProbe::new(fault, false);
            let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
            assert_complete_lexical_fallback(&request, &rows, &actual);
            assert_eq!(model.single_calls.load(Ordering::SeqCst), 3);
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn custom_bound_batches_cannot_publish_malformed_values_or_partial_coverage() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        for fault in [
            IdentityFault::ShortBatch,
            IdentityFault::ExtraBatch,
            IdentityFault::NonfiniteBatch,
            IdentityFault::WrongDimensionBatch,
        ] {
            let model = BoundProbe::new(fault, true);
            let actual = evaluate_with_prepared_model(&request, &rows, &model, None).unwrap();
            assert_complete_lexical_fallback(&request, &rows, &actual);
            assert_eq!(model.single_calls.load(Ordering::SeqCst), 1);
            assert_eq!(model.batch_calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn cancellation_wins_over_foreign_bound_output_in_both_inference_lanes() {
        let _ambient = Cx::set_current(None);
        let (request, rows) = multi_batch_fixture();
        for native in [false, true] {
            for stage in [SpaceChange::Query, SpaceChange::Batch(2)] {
                let mut model = BoundProbe::new(IdentityFault::ForeignOutput(stage), native);
                model.cancel_at = stage;
                let caller = Cx::for_testing();
                let error =
                    evaluate_with_prepared_model(&request, &rows, &model, Some(caller.clone()))
                        .unwrap_err();
                assert!(caller.checkpoint().is_err());
                assert!(error.to_string().contains("cancelled"));
                assert!(!error.to_string().contains("PRIVATE-IDENTITY"));
                assert!(Cx::current().is_none());
            }
        }
    }
}
