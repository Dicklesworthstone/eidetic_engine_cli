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
    CassPrefetchHistory, CassPrefetchMetrics, DEFAULT_PREFETCH_HISTORY_WINDOW,
    DEFAULT_PREFETCH_TOP_K, RecencyWeightedFrequencyPredictor, SpeculativePrefetch,
};

type TestResult = Result<(), String>;

const TEST_AGENT_SCOPE: &str = "integration-agent";

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
    CassPrefetchHistory::from_topics(
        TEST_AGENT_SCOPE,
        [
            "refactor",   // position 0 — current; excluded as candidate
            "debug",      // position 1
            "refactor",   // position 2
            "refactor",   // position 3
            "doc-update", // position 4
            "doc-update", // position 5
            "doc-update", // position 6
            "debug",      // position 7
        ],
    )
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

    // The most-recent topic ("refactor" at position 0) is excluded as a
    // candidate everywhere in the retained history: predicting an immediate
    // repeat provides no prefetch value, even when older repeats exist.
    if predictions
        .iter()
        .any(|candidate| candidate.topic_id == "refactor")
    {
        return Err(format!(
            "current topic must stay excluded from candidates; got {predictions:?}"
        ));
    }
    if !predictions
        .iter()
        .any(|candidate| candidate.topic_id == "debug")
        || !predictions
            .iter()
            .any(|candidate| candidate.topic_id == "doc-update")
    {
        return Err(format!(
            "synthetic history should emit debug and doc-update candidates; got {predictions:?}"
        ));
    }

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
fn all_same_topic_history_predicts_nothing() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history =
        CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, ["refactor", "refactor", "refactor"]);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    if !predictions.is_empty() {
        return Err(format!(
            "all-same-topic history must not predict an immediate repeat; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn single_element_history_predicts_nothing() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, ["only_topic"]);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    if !predictions.is_empty() {
        return Err(format!(
            "single-element history must not emit candidates; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn empty_topic_ids_are_never_emitted() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, ["current", "", "alpha", ""]);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    if predictions
        .iter()
        .any(|candidate| candidate.topic_id.is_empty())
    {
        return Err(format!(
            "empty topic_id must be ignored rather than emitted; got {predictions:?}"
        ));
    }
    if !predictions
        .iter()
        .any(|candidate| candidate.topic_id == "alpha")
    {
        return Err(format!(
            "non-empty older topic should remain eligible after empty topics are ignored; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn default_window_boundary_emits_finite_normalized_candidates() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history = CassPrefetchHistory::from_topics(
        TEST_AGENT_SCOPE,
        [
            "current", "alpha", "bravo", "alpha", "charlie", "bravo", "delta", "alpha", "echo",
            "bravo",
        ],
    );
    if history.len() != DEFAULT_PREFETCH_HISTORY_WINDOW {
        return Err(format!(
            "test fixture must have DEFAULT_PREFETCH_HISTORY_WINDOW ({DEFAULT_PREFETCH_HISTORY_WINDOW}) entries; got {}",
            history.len()
        ));
    }

    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    assert_finite_normalized_predictions("default window boundary", &predictions)
}

#[test]
fn history_exceeding_default_window_still_emits_finite_normalized_scores() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let topics: Vec<String> = (0..(DEFAULT_PREFETCH_HISTORY_WINDOW * 2))
        .map(|index| match index {
            0 => "current".to_string(),
            value if value % 3 == 0 => "alpha".to_string(),
            value if value % 3 == 1 => "bravo".to_string(),
            _ => "charlie".to_string(),
        })
        .collect();
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, topics);
    if history.len() <= DEFAULT_PREFETCH_HISTORY_WINDOW {
        return Err(format!(
            "test fixture must exceed DEFAULT_PREFETCH_HISTORY_WINDOW ({DEFAULT_PREFETCH_HISTORY_WINDOW}); got {}",
            history.len()
        ));
    }

    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    assert_finite_normalized_predictions("history exceeding default window", &predictions)
}

#[test]
fn tied_score_predictions_use_lex_tie_break_at_integration_boundary() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new().with_half_life(1.0e308);
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, ["current", "zeta", "alpha"]);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    if predictions.len() != 2 {
        return Err(format!(
            "tie fixture should emit two candidates; got {predictions:?}"
        ));
    }
    if predictions[0].score.total_cmp(&predictions[1].score) != std::cmp::Ordering::Equal {
        return Err(format!(
            "huge half-life fixture should produce tied scores; got {predictions:?}"
        ));
    }
    if predictions[0].topic_id != "alpha" || predictions[1].topic_id != "zeta" {
        return Err(format!(
            "tied candidates must sort lexicographically; got {predictions:?}"
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
fn aggressive_min_score_drops_low_confidence_candidates_at_integration_boundary() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new().with_min_score(0.45);
    let history = CassPrefetchHistory::from_topics(
        TEST_AGENT_SCOPE,
        ["current", "alpha", "alpha", "alpha", "noise", "noise"],
    );

    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    if !predictions
        .iter()
        .any(|candidate| candidate.topic_id == "alpha")
    {
        return Err(format!(
            "dominant alpha topic must survive aggressive min_score; got {predictions:?}"
        ));
    }
    if predictions
        .iter()
        .any(|candidate| candidate.topic_id == "noise")
    {
        return Err(format!(
            "low-confidence noise topic must be dropped by min_score; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn non_finite_predictor_options_fall_back_to_finite_defaults() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new()
        .with_half_life(f64::NAN)
        .with_min_score(f64::NAN);
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, ["current", "alpha", "alpha"]);

    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    assert_finite_normalized_predictions("non-finite predictor options", &predictions)?;
    if !predictions
        .iter()
        .any(|candidate| candidate.topic_id == "alpha")
    {
        return Err(format!(
            "alpha should remain eligible after non-finite options fall back; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn empty_history_or_zero_top_k_returns_empty_at_integration_boundary() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let empty_history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, Vec::<&str>::new());
    let empty_predictions = predictor.predict_next_n(&empty_history, DEFAULT_PREFETCH_TOP_K);
    if !empty_predictions.is_empty() {
        return Err(format!(
            "empty history must emit no candidates; got {empty_predictions:?}"
        ));
    }

    let history = synthetic_task_template_history();
    let zero_top_k_predictions = predictor.predict_next_n(&history, 0);
    if !zero_top_k_predictions.is_empty() {
        return Err(format!(
            "top_k=0 must emit no candidates; got {zero_top_k_predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn metrics_serialize_under_camel_case_schema() -> TestResult {
    let mut metrics = CassPrefetchMetrics::new();
    metrics.record_candidate();
    metrics.record_budget_exceeded();
    metrics.record_history_too_short();
    metrics.record_stale_generation_drop();
    metrics.record_history_oversized();

    let value = serde_json::to_value(metrics)
        .map_err(|error| format!("serialize CassPrefetchMetrics: {error}"))?;
    let Some(object) = value.as_object() else {
        return Err(format!(
            "metrics must serialize to a JSON object; got {value:?}"
        ));
    };

    for forbidden in [
        "measured_against_revision",
        "candidates_emitted",
        "budget_exceeded",
        "history_too_short",
        "stale_generation_drop",
        "history_oversized",
    ] {
        if object.contains_key(forbidden) {
            return Err(format!(
                "metrics JSON must use camelCase, but found snake_case field {forbidden:?}: {value}"
            ));
        }
    }

    let expected_fields = [
        ("measuredAgainstRevision", serde_json::json!("unknown")),
        ("hits", serde_json::json!(0)),
        ("misses", serde_json::json!(0)),
        ("candidatesEmitted", serde_json::json!(1)),
        ("budgetExceeded", serde_json::json!(1)),
        ("historyTooShort", serde_json::json!(1)),
        ("staleGenerationDrop", serde_json::json!(1)),
        ("historyOversized", serde_json::json!(1)),
    ];
    for (field, expected) in expected_fields {
        let Some(actual) = value.get(field) else {
            return Err(format!("metrics JSON missing field {field:?}: {value}"));
        };
        if actual != &expected {
            return Err(format!(
                "metrics JSON field {field:?} expected {expected:?}, got {actual}: {value}"
            ));
        }
    }
    Ok(())
}

#[test]
fn metrics_hit_rate_edges_cover_zero_miss_only_and_hit_only() -> TestResult {
    let zero_attempts = CassPrefetchMetrics::new();
    if (zero_attempts.hit_rate() - 0.0).abs() > f64::EPSILON {
        return Err(format!(
            "zero-attempt hit_rate must be 0.0; got {}",
            zero_attempts.hit_rate()
        ));
    }

    let mut miss_only = CassPrefetchMetrics::new();
    miss_only.record_miss();
    miss_only.record_miss();
    if (miss_only.hit_rate() - 0.0).abs() > f64::EPSILON {
        return Err(format!(
            "miss-only hit_rate must be 0.0; got {}",
            miss_only.hit_rate()
        ));
    }

    let mut hit_only = CassPrefetchMetrics::new();
    hit_only.record_hit();
    hit_only.record_hit();
    if (hit_only.hit_rate() - 1.0).abs() > f64::EPSILON {
        return Err(format!(
            "hit-only hit_rate must be 1.0; got {}",
            hit_only.hit_rate()
        ));
    }
    Ok(())
}

fn assert_finite_normalized_predictions(
    fixture_name: &str,
    predictions: &[CassPrefetchCandidate],
) -> TestResult {
    if predictions.is_empty() {
        return Err(format!("{fixture_name} should emit at least one candidate"));
    }
    for candidate in predictions {
        if candidate.topic_id.is_empty() {
            return Err(format!(
                "{fixture_name} emitted empty topic_id; predictions={predictions:?}"
            ));
        }
        if !candidate.score.is_finite() || !(0.0..=1.0).contains(&candidate.score) {
            return Err(format!(
                "{fixture_name} emitted invalid score for {:?}: {}; predictions={predictions:?}",
                candidate.topic_id, candidate.score
            ));
        }
    }
    Ok(())
}

#[test]
fn history_iterator_preserves_most_recent_first_order() -> TestResult {
    let history = synthetic_task_template_history();
    if history.agent_scope != TEST_AGENT_SCOPE {
        return Err(format!(
            "synthetic history must carry TEST_AGENT_SCOPE; got {}",
            history.agent_scope
        ));
    }
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
    let Some(first) = history.iter().next() else {
        return Err("synthetic history must expose a first observation".to_string());
    };
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

// ---------------------------------------------------------------------------
// bd-3k1oe — input-distribution edge coverage. The happy-path tests above all
// reuse synthetic_task_template_history(); these pin the predictor's behavior on
// history-length boundaries and pathological inputs (all-same-topic, single
// element, exactly the window, far over the window, empty topic ids) so a
// regression can't silently emit an immediate-repeat, leak an empty topic id,
// or panic / emit non-finite scores on degenerate input.
// ---------------------------------------------------------------------------

/// Shared invariant check: at most top_k candidates, no empty/duplicate topic
/// ids, all scores finite in [0, 1], descending score order. Vacuously true for
/// an empty prediction set.
fn assert_candidates_well_formed(predictions: &[CassPrefetchCandidate], label: &str) -> TestResult {
    if predictions.len() > DEFAULT_PREFETCH_TOP_K {
        return Err(format!(
            "{label}: more than top_k ({DEFAULT_PREFETCH_TOP_K}) candidates: {}",
            predictions.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for candidate in predictions {
        if candidate.topic_id.is_empty() {
            return Err(format!(
                "{label}: empty topic_id in output: {predictions:?}"
            ));
        }
        if !seen.insert(candidate.topic_id.clone()) {
            return Err(format!(
                "{label}: duplicate topic_id {:?}",
                candidate.topic_id
            ));
        }
        if !candidate.score.is_finite() || !(0.0..=1.0).contains(&candidate.score) {
            return Err(format!(
                "{label}: score {} for {:?} is non-finite or outside [0, 1]",
                candidate.score, candidate.topic_id
            ));
        }
    }
    for window in predictions.windows(2) {
        if window[0].score < window[1].score {
            return Err(format!(
                "{label}: not in descending score order: {predictions:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn all_same_topic_history_yields_no_candidates_bd_3k1oe() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history =
        CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, ["refactor", "refactor", "refactor"]);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    // Most-recent topic and every duplicate of it are excluded; nothing else
    // remains, so predicting an immediate repeat must never happen.
    if !predictions.is_empty() {
        return Err(format!(
            "all-same-topic history must yield no candidates; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn single_element_history_yields_no_candidates_bd_3k1oe() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, ["only_topic"]);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    if !predictions.is_empty() {
        return Err(format!(
            "single-element history must yield no candidates; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn exact_window_history_is_well_formed_bd_3k1oe() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    // Exactly DEFAULT_PREFETCH_HISTORY_WINDOW elements, most-recent-first.
    let topics = ["a", "b", "c", "b", "c", "a", "b", "c", "a", "b"];
    if topics.len() != DEFAULT_PREFETCH_HISTORY_WINDOW {
        return Err(format!(
            "fixture must be exactly the window size {DEFAULT_PREFETCH_HISTORY_WINDOW}; got {}",
            topics.len()
        ));
    }
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, topics);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    assert_candidates_well_formed(&predictions, "exact-window")?;
    if predictions.is_empty() {
        return Err("a full-window multi-topic history should emit candidates".to_string());
    }
    // Most-recent topic ("a") stays excluded even though older repeats exist.
    if predictions
        .iter()
        .any(|candidate| candidate.topic_id == "a")
    {
        return Err(format!(
            "most-recent topic must stay excluded; got {predictions:?}"
        ));
    }
    Ok(())
}

#[test]
fn over_window_history_degrades_gracefully_bd_3k1oe() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    // 100 elements — far over the window. The producer owns trimming; the
    // predictor must not panic and must never emit non-finite / subnormal-garbage
    // scores for deep positions (recency weights decay toward zero and are
    // filtered, not leaked). Admission-bounding to empty is also graceful.
    let cycle = ["alpha", "bravo", "charlie", "delta"];
    let topics: Vec<&str> = (0..100).map(|i| cycle[i % cycle.len()]).collect();
    let history = CassPrefetchHistory::from_topics(TEST_AGENT_SCOPE, topics);
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    assert_candidates_well_formed(&predictions, "over-window")?;
    Ok(())
}

#[test]
fn empty_topic_ids_never_appear_in_candidates_bd_3k1oe() -> TestResult {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    // Empty topic ids are legal inputs (CassPrefetchObservation::new("")). They
    // must never surface as candidates — the predictor skips empty topics.
    let history = CassPrefetchHistory::from_topics(
        TEST_AGENT_SCOPE,
        [
            "current",
            "",
            "valid_topic",
            "",
            "valid_topic",
            "another",
            "",
        ],
    );
    let predictions = predictor.predict_next_n(&history, DEFAULT_PREFETCH_TOP_K);
    assert_candidates_well_formed(&predictions, "empty-topic-input")?;
    if predictions
        .iter()
        .any(|candidate| candidate.topic_id.is_empty())
    {
        return Err(format!(
            "empty topic_id must never leak into candidates; got {predictions:?}"
        ));
    }
    Ok(())
}
