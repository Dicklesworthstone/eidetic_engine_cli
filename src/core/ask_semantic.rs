//! Local semantic span scoring for extractive answers (ADR 0067).
//!
//! Build one complete query-specific score table before the admission cap.
//! Partial model results never mix scoring regimes. Only a verified, already
//! installed local model may run here: no downloads, remote requests, index
//! hydration, migrations, or model-registry writes belong to an ask read.

use std::collections::{BTreeMap, BTreeSet};

use asupersync::Cx;
use frankensearch::SearchError;

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
    let embedder = match crate::core::index::local_read_only_embedder(connection, workspace_id) {
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
    let result = crate::core::run_cli_with_cx(STANDALONE_SCORING_TIMEOUT, |runtime_cx| async move {
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
        if !embedder.is_semantic() || embedder.dimension() == 0 {
            return Err(SemanticFailure::Unavailable);
        }
        let dimension = embedder.dimension();
        let query = embedder
            .embed(cx, question)
            .await
            .map_err(|error| inference_failure(cx, &error))?;
        checkpoint(cx)?;
        let query_norm = vector_norm_squared(&query, dimension)
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
            checkpoint(cx)?;
            let vectors = embedder
                .embed_batch(cx, batch)
                .await
                .map_err(|error| inference_failure(cx, &error))?;
            checkpoint(cx)?;
            // A lazy backend may silently switch to hashes. Even a dimension-
            // compatible fallback must never be reported as semantic inference.
            if !embedder.is_semantic() || embedder.dimension() != dimension {
                return Err(SemanticFailure::Unavailable);
            }
            append_batch(&mut by_text, batch, &vectors, &query, query_norm)?;
        }
        checkpoint(cx)?;
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
                assert!(Cx::current().is_some(), "the bridge must install its context");
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
}
