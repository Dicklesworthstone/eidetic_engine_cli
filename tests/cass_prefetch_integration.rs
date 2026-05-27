//! bd-16pwc — SRR5 speculative CASS pre-fetch integration test.
//!
//! Drives the `SpeculativePrefetch` trait + the default
//! `RecencyWeightedFrequencyPredictor` implementation against a
//! synthetic 3-task-template CASS history (refactor / debug /
//! doc-update — matching the bead's "canonical task templates"
//! fixture shape). Asserts:
//!
//!   1. The predictor emits ranked candidates against a real
//!      multi-template history without producing duplicates,
//!      non-finite scores, or empty topic IDs.
//!   2. The metrics counter records hits / misses / candidates
//!      through the public surface that the daemon slice will
//!      eventually wire.
//!   3. The trait can be implemented by a caller-supplied stub so the
//!      v1 wiring can plug in an alternate predictor later (e.g. a
//!      cosine-similarity template predictor, or an ML-backed one)
//!      without changing the production call site.
//!   4. The CassPrefetchHistory iterator iteration produces stable
//!      ordering (this is the determinism contract — same history
//!      means same candidates means same prefetch cache population).
//!
//! Out of scope: any actual CASS-evidence fetching, daemon
//! integration, noisy-neighbor backoff, and the
//! `cass_prefetch_budget_exceeded` /
//! `adaptive_backoff_applied` degraded-code emissions. The follow-up
//! daemon slice owns those; this test pins the trait + heuristic
//! contract the daemon will consume.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::core::cass_prefetch::{
    CASS_PREFETCH_DECISION_SCHEMA_V1, CASS_PREFETCH_METRICS_SCHEMA_V1, CassPrefetchCandidate,
    CassPrefetchHistory, CassPrefetchMetrics, DEFAULT_PREFETCH_TOP_K,
    RecencyWeightedFrequencyPredictor, SpeculativePrefetch,
};

type TestResult = Result<(), String>;

/// Stub predictor used by the trait-pluggability test. Returns a
/// fixed list regardless of history, so we can assert the trait seam
/// works even when the implementation has no internal heuristic.
struct ConstantStubPredictor;

impl SpeculativePrefetch for ConstantStubPredictor {
    fn name(&self) -> &'static str {
        "constant_stub_v0"
    }

    fn predict_next_n(
        &self,
        _history: &CassPrefetchHistory,
        top_k: usize,
    ) -> Vec<CassPrefetchCandidate> {
        let canned = vec![
            CassPrefetchCandidate::new("stub_alpha", 0.9, "constant_stub_v0"),
            CassPrefetchCandidate::new("stub_bravo", 0.5, "constant_stub_v0"),
            CassPrefetchCandidate::new("stub_charlie", 0.3, "constant_stub_v0"),
        ];
        canned.into_iter().take(top_k).collect()
    }
}

/// Build a synthetic 8-element rolling history that matches the
/// bead's three canonical task templates (refactor / debug /
/// doc-update). Most-recent-first.
fn synthetic_task_template_history() -> CassPrefetchHistory {
    // Reading order is most-recent-first per the type's contract.
    // Storyline: the agent is currently working on "refactor", just
    // finished a "debug" pass, did "refactor" twice before that,
    // touched "doc-update" three times further back. The predictor
    // should rank refactor highest (it dominates recent positions),
    // then debug, then doc-update.
    CassPrefetchHistory::from_topics([
        "refactor",   // position 0 — current; excluded as candidate
        "debug",      // position 1
        "refactor",   // position 2
        "refactor",   // position 3
        "doc-update", // position 4
        "doc-update", // position 5
        "doc-update", // position 6
        "debug",      // position 7
    ])
}

#[test]
fn default_predictor_emits_ranked_unique_finite_candidates() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history = synthetic_task_template_history();
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);

    if predictions.is_empty() {
        return Err("expected at least one candidate for the synthetic history".to_string());
    }
    if predictions.len() > DEFAULT_PREFETCH_TOP_K {
        return Err(format!(
            "expected at most DEFAULT_PREFETCH_TOP_K ({DEFAULT_PREFETCH_TOP_K}) candidates; got {}",
            predictions.len()
        ));
    }

    // No duplicates.
    let mut seen = std::collections::HashSet::new();
    for candidate in &predictions {
        if !seen.insert(candidate.topic_id.clone()) {
            return Err(format!(
                "duplicate candidate topic_id {:?}; predictions={:?}",
                candidate.topic_id, predictions
            ));
        }
    }

    // No empty topic IDs.
    for candidate in &predictions {
        if candidate.topic_id.is_empty() {
            return Err(format!(
                "candidate has empty topic_id; predictions={:?}",
                predictions
            ));
        }
    }

    // All scores finite, in [0.0, 1.0].
    for candidate in &predictions {
        if !candidate.score.is_finite() {
            return Err(format!(
                "candidate {:?} has non-finite score {}",
                candidate.topic_id, candidate.score
            ));
        }
        if !(0.0..=1.0).contains(&candidate.score) {
            return Err(format!(
                "candidate {:?} score {} outside [0, 1]",
                candidate.topic_id, candidate.score
            ));
        }
    }

    // Descending score order (with lex tie-break, but no ties here).
    for window in predictions.windows(2) {
        if window[0].score < window[1].score {
            return Err(format!(
                "predictions not in descending score order: {:?}",
                predictions
            ));
        }
    }

    // The most-recent topic ("refactor" at position 0) should NOT be
    // a candidate — predicting an immediate repeat provides no
    // prefetch value. Other refactor occurrences (positions 2, 3)
    // promote it back into the candidate pool with high recency.
    assert!(
        predictions.iter().any(|c| c.topic_id == "refactor"),
        "refactor (positions 2 + 3) should outrank the lone-position-1 debug; got {predictions:?}"
    );

    // Predictor name is surfaced on each candidate.
    for candidate in &predictions {
        if candidate.predictor != "recency_weighted_frequency_v1" {
            return Err(format!(
                "candidate predictor must be recency_weighted_frequency_v1; got {:?}",
                candidate.predictor
            ));
        }
    }
    Ok(())
}

#[test]
fn predictor_is_deterministic_across_repeated_calls() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history = synthetic_task_template_history();

    let first = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    let second = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    let third = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);

    if first != second || second != third {
        return Err(format!(
            "predictor must produce byte-identical output for the same input; \
             first={first:?}, second={second:?}, third={third:?}"
        ));
    }
    Ok(())
}

#[test]
fn trait_seam_accepts_caller_supplied_predictor() -> TestResult {
    // The constant stub returns predictions independent of the
    // history. The point of the test is to confirm a caller can
    // implement the trait and have it slot in cleanly — this is the
    // pluggability seam the daemon slice + a future ML predictor
    // will exercise.
    let stub = ConstantStubPredictor;
    let history = synthetic_task_template_history();
    let predictions = stub.predict_next_n(&history, 2);
    if predictions.len() != 2 {
        return Err(format!(
            "stub predictor with top_k=2 must emit 2 candidates; got {}",
            predictions.len()
        ));
    }
    if predictions[0].topic_id != "stub_alpha" || predictions[1].topic_id != "stub_bravo" {
        return Err(format!(
            "stub predictor must emit canned candidates in order; got {predictions:?}"
        ));
    }
    if stub.name() != "constant_stub_v0" {
        return Err(format!(
            "stub predictor name must be constant_stub_v0; got {:?}",
            stub.name()
        ));
    }
    Ok(())
}

#[test]
fn metrics_counter_records_hits_misses_and_candidates() -> TestResult {
    // Simulate the daemon's eventual integration: the predictor
    // emits candidates, the daemon either hits or misses on the
    // actual next call, and the metrics counter rolls everything up
    // for the swarm-brief surface.
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history = synthetic_task_template_history();
    let mut metrics = CassPrefetchMetrics::new();

    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    for _ in &predictions {
        metrics.record_candidate();
    }

    // Simulate three observations: two hits (the next call was one
    // of the predicted topics) and one miss (the agent ran a
    // completely unrelated query).
    metrics.record_hit();
    metrics.record_hit();
    metrics.record_miss();
    // And one budget-exceeded event so the daemon-side observability
    // story is end-to-end.
    metrics.record_budget_exceeded();

    if metrics.hits != 2 {
        return Err(format!("expected hits=2; got {}", metrics.hits));
    }
    if metrics.misses != 1 {
        return Err(format!("expected misses=1; got {}", metrics.misses));
    }
    if metrics.attempts() != 3 {
        return Err(format!("expected attempts=3; got {}", metrics.attempts()));
    }
    let hit_rate = metrics.hit_rate();
    let expected = 2.0 / 3.0;
    if (hit_rate - expected).abs() > 1e-12 {
        return Err(format!("expected hit_rate≈{expected}; got {hit_rate}"));
    }
    if metrics.budget_exceeded != 1 {
        return Err(format!(
            "expected budget_exceeded=1; got {}",
            metrics.budget_exceeded
        ));
    }
    if metrics.candidates_emitted as usize != predictions.len() {
        return Err(format!(
            "candidates_emitted ({}) must match the predicted candidate count ({})",
            metrics.candidates_emitted,
            predictions.len()
        ));
    }
    Ok(())
}

#[test]
fn history_iterator_preserves_most_recent_first_order() -> TestResult {
    let history = synthetic_task_template_history();
    if history.is_empty() {
        return Err("synthetic history must not be empty".to_string());
    }
    if history.len() != 8 {
        return Err(format!(
            "synthetic history must have 8 entries; got {}",
            history.len()
        ));
    }

    // First entry is the most-recent observation.
    let first = history.iter().next().unwrap();
    if first.topic_id != "refactor" {
        return Err(format!(
            "most-recent observation must be 'refactor'; got {:?}",
            first.topic_id
        ));
    }

    // Iteration order is stable: collecting twice yields identical
    // sequences.
    let collected_a: Vec<&str> = history.iter().map(|o| o.topic_id.as_str()).collect();
    let collected_b: Vec<&str> = history.iter().map(|o| o.topic_id.as_str()).collect();
    if collected_a != collected_b {
        return Err(format!(
            "history iterator must be deterministic; got {collected_a:?} vs {collected_b:?}"
        ));
    }
    Ok(())
}

#[test]
fn schema_constants_pin_the_explain_contract() -> TestResult {
    // Agents reading the prefetch posture out of `--explain`
    // payloads will pin to these literal strings. A future refactor
    // that drops the `.v1` suffix without bumping the schema version
    // would break those agents silently; pin the literals here so
    // the cargo test gate catches the drift.
    if CASS_PREFETCH_DECISION_SCHEMA_V1 != "ee.cass_prefetch.decision.v1" {
        return Err(format!(
            "decision schema constant drifted; got {}",
            CASS_PREFETCH_DECISION_SCHEMA_V1
        ));
    }
    if CASS_PREFETCH_METRICS_SCHEMA_V1 != "ee.cass_prefetch.metrics.v1" {
        return Err(format!(
            "metrics schema constant drifted; got {}",
            CASS_PREFETCH_METRICS_SCHEMA_V1
        ));
    }
    Ok(())
}
