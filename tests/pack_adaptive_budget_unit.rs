//! Integration coverage for the adaptive context-pack budget classifier
//! (bd-1prrl.4 swarmx.7).
//!
//! These tests exercise `classify_adaptive_budget` from `tests/` as a
//! consumer of the public `ee::pack::budget_classifier` surface, so any
//! accidental rename/move of the classifier API breaks the integration
//! suite, not just the inline unit module. The inline tests in
//! `src/pack/budget_classifier.rs` cover the math; these tests pin the
//! public-call shape and the explainable-contribution contract that the
//! `ee context --explain --json` budget block depends on.

use ee::pack::budget_classifier::{
    ADAPTIVE_BUDGET_SCHEMA_V1, AdaptiveBudgetInput, DEFAULT_ADAPTIVE_BASE_TOKENS, GRAPH_FANOUT_CAP,
    RETRIEVAL_ENTROPY_SAMPLE_LIMIT, classify_adaptive_budget,
};

const FLOAT_EPSILON: f64 = 1e-6;

fn assert_close(left: f64, right: f64, ctx: &str) {
    assert!(
        (left - right).abs() <= FLOAT_EPSILON,
        "{ctx}: expected {left} close to {right} (delta {})",
        (left - right).abs()
    );
}

#[test]
fn trivial_query_with_empty_retrieval_lands_at_base_tokens() {
    let decision = classify_adaptive_budget(
        AdaptiveBudgetInput::new("show memory mem_release_policy", &[], 0.0).with_max_tokens(4_000),
    );

    assert_eq!(decision.schema, ADAPTIVE_BUDGET_SCHEMA_V1);
    assert!(decision.adaptive);
    assert_eq!(decision.base_tokens, DEFAULT_ADAPTIVE_BASE_TOKENS);
    assert_eq!(decision.computed_tokens, DEFAULT_ADAPTIVE_BASE_TOKENS);
    assert_close(decision.multiplier, 1.0, "trivial multiplier");
    assert_close(
        decision.classifier_contributions.retrieval_entropy,
        0.0,
        "trivial entropy",
    );
    assert_close(
        decision.classifier_contributions.graph_fanout,
        0.0,
        "trivial graph fanout",
    );
    assert_close(
        decision.classifier_contributions.task_keyword_score,
        0.0,
        "trivial task keyword",
    );
}

#[test]
fn complex_query_uniform_topk_and_high_fanout_inflates_budget_within_ceiling() {
    let scores = vec![0.75_f32; RETRIEVAL_ENTROPY_SAMPLE_LIMIT];
    let decision = classify_adaptive_budget(
        AdaptiveBudgetInput::new("audit refactor migrate security performance", &scores, 12.0)
            .with_max_tokens(8_000),
    );

    assert_eq!(decision.schema, ADAPTIVE_BUDGET_SCHEMA_V1);
    assert!(decision.adaptive);
    assert_eq!(decision.base_tokens, DEFAULT_ADAPTIVE_BASE_TOKENS);
    assert_eq!(decision.max_tokens, 8_000);
    assert!(
        decision.computed_tokens > DEFAULT_ADAPTIVE_BASE_TOKENS,
        "complex query should inflate beyond base tokens; computed={}",
        decision.computed_tokens
    );
    assert!(
        decision.computed_tokens <= decision.max_tokens,
        "computed tokens must never exceed max_tokens; got {} > {}",
        decision.computed_tokens,
        decision.max_tokens
    );
    assert_close(
        decision.classifier_contributions.graph_fanout,
        GRAPH_FANOUT_CAP,
        "high fanout must clamp to the cap",
    );
    assert!(
        decision.classifier_contributions.retrieval_entropy > 0.95,
        "uniform top-k should produce near-1.0 normalized entropy; got {}",
        decision.classifier_contributions.retrieval_entropy
    );
    assert!(
        decision.classifier_contributions.task_keyword_score > 0.0,
        "task keyword markers must contribute"
    );
}

#[test]
fn classify_adaptive_budget_is_deterministic_for_same_input() {
    let scores = vec![0.9_f32, 0.6, 0.3, 0.1];
    let input = AdaptiveBudgetInput::new("debug regression in pack scoring", &scores, 1.5);
    let left = classify_adaptive_budget(input.clone());
    let right = classify_adaptive_budget(input);
    assert_eq!(left, right);
}
