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
    let result = crate::core::run_cli_future(async {
        let cx = caller_cx
            .or_else(Cx::current)
            .ok_or(SemanticFailure::Unavailable)?;
        SemanticScores::build(&cx, &request.question, candidates, embedder.as_ref()).await
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
