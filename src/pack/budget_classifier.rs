//! Pure adaptive context-pack budget classifier.
//!
//! This module is deliberately side-effect free: callers provide already
//! collected retrieval and graph facts, and the classifier returns a
//! deterministic token budget plus contribution details.

use serde::Serialize;

pub const ADAPTIVE_BUDGET_SCHEMA_V1: &str = "ee.context.budget.v1";
pub const DEFAULT_ADAPTIVE_BASE_TOKENS: u32 = 1_000;
pub const DEFAULT_ADAPTIVE_MAX_TOKENS: u32 = super::DEFAULT_CONTEXT_MAX_TOKENS;
pub const RETRIEVAL_ENTROPY_SAMPLE_LIMIT: usize = 20;
pub const GRAPH_FANOUT_CAP: f64 = 3.0;

const RETRIEVAL_ENTROPY_WEIGHT: f64 = 0.5;
const GRAPH_FANOUT_WEIGHT: f64 = 0.3;
const TASK_KEYWORD_WEIGHT: f64 = 0.2;
const TASK_KEYWORD_MARKER_SCORE: f64 = 0.3;

const TASK_COMPLEXITY_MARKERS: &[&str] = &[
    "audit",
    "debug",
    "diagnose",
    "e2e",
    "fix",
    "hardening",
    "migrate",
    "performance",
    "refactor",
    "rewrite",
    "security",
    "test",
    "verify",
];

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveBudgetInput<'a> {
    pub query: &'a str,
    pub retrieval_scores: &'a [f32],
    pub graph_fanout: f64,
    pub max_tokens: u32,
}

impl<'a> AdaptiveBudgetInput<'a> {
    #[must_use]
    pub const fn new(query: &'a str, retrieval_scores: &'a [f32], graph_fanout: f64) -> Self {
        Self {
            query,
            retrieval_scores,
            graph_fanout,
            max_tokens: DEFAULT_ADAPTIVE_MAX_TOKENS,
        }
    }

    #[must_use]
    pub const fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveBudgetContributions {
    pub retrieval_entropy: f64,
    pub retrieval_entropy_multiplier: f64,
    pub graph_fanout: f64,
    pub graph_fanout_multiplier: f64,
    pub task_keyword_score: f64,
    pub task_keyword_multiplier: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveBudgetDecision {
    pub schema: &'static str,
    pub adaptive: bool,
    pub base_tokens: u32,
    pub max_tokens: u32,
    pub computed_tokens: u32,
    pub multiplier: f64,
    pub classifier_contributions: AdaptiveBudgetContributions,
}

#[must_use]
pub fn classify_adaptive_budget(input: AdaptiveBudgetInput<'_>) -> AdaptiveBudgetDecision {
    let max_tokens = input.max_tokens.max(1);
    let base_tokens = DEFAULT_ADAPTIVE_BASE_TOKENS.min(max_tokens);
    let retrieval_entropy = normalized_retrieval_entropy(input.retrieval_scores);
    let graph_fanout = capped_graph_fanout(input.graph_fanout);
    let task_keyword_score = task_keyword_score(input.query);
    let classifier_contributions = AdaptiveBudgetContributions {
        retrieval_entropy,
        retrieval_entropy_multiplier: retrieval_entropy * RETRIEVAL_ENTROPY_WEIGHT,
        graph_fanout,
        graph_fanout_multiplier: graph_fanout * GRAPH_FANOUT_WEIGHT,
        task_keyword_score,
        task_keyword_multiplier: task_keyword_score * TASK_KEYWORD_WEIGHT,
    };
    let multiplier = 1.0
        + classifier_contributions.retrieval_entropy_multiplier
        + classifier_contributions.graph_fanout_multiplier
        + classifier_contributions.task_keyword_multiplier;
    let scaled = (f64::from(base_tokens) * multiplier).ceil();
    let computed_tokens = if !scaled.is_finite() || scaled <= 0.0 {
        base_tokens
    } else if scaled >= f64::from(max_tokens) {
        max_tokens
    } else {
        scaled as u32
    };

    AdaptiveBudgetDecision {
        schema: ADAPTIVE_BUDGET_SCHEMA_V1,
        adaptive: true,
        base_tokens,
        max_tokens,
        computed_tokens,
        multiplier,
        classifier_contributions,
    }
}

#[must_use]
pub fn normalized_retrieval_entropy(retrieval_scores: &[f32]) -> f64 {
    let positive_scores = retrieval_scores
        .iter()
        .copied()
        .filter(|score| score.is_finite() && *score > 0.0)
        .take(RETRIEVAL_ENTROPY_SAMPLE_LIMIT)
        .map(f64::from)
        .collect::<Vec<_>>();
    if positive_scores.len() <= 1 {
        return 0.0;
    }
    let total = positive_scores.iter().sum::<f64>();
    if total <= 0.0 || !total.is_finite() {
        return 0.0;
    }
    let entropy = positive_scores
        .iter()
        .map(|score| {
            let probability = score / total;
            -probability * probability.ln()
        })
        .sum::<f64>();
    (entropy / (positive_scores.len() as f64).ln()).clamp(0.0, 1.0)
}

#[must_use]
pub fn capped_graph_fanout(graph_fanout: f64) -> f64 {
    if !graph_fanout.is_finite() || graph_fanout <= 0.0 {
        0.0
    } else {
        graph_fanout.min(GRAPH_FANOUT_CAP)
    }
}

#[must_use]
pub fn task_keyword_score(query: &str) -> f64 {
    let normalized = query
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>();
    let has_marker = normalized
        .split_whitespace()
        .any(|word| TASK_COMPLEXITY_MARKERS.binary_search(&word).is_ok());
    if has_marker {
        TASK_KEYWORD_MARKER_SCORE
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(left: f64, right: f64) {
        assert!(
            (left - right).abs() <= 0.000_001,
            "expected {left} to be close to {right}"
        );
    }

    #[test]
    fn entropy_is_zero_for_single_or_empty_retrieval() {
        assert_close(normalized_retrieval_entropy(&[]), 0.0);
        assert_close(normalized_retrieval_entropy(&[0.9]), 0.0);
        assert_close(normalized_retrieval_entropy(&[0.0, -1.0, f32::NAN]), 0.0);
    }

    #[test]
    fn entropy_is_high_for_uniform_top_scores() {
        let scores = vec![1.0_f32; RETRIEVAL_ENTROPY_SAMPLE_LIMIT];
        assert_close(normalized_retrieval_entropy(&scores), 1.0);
    }

    #[test]
    fn entropy_uses_only_stable_top_sample() {
        let mut scores = vec![1.0_f32; RETRIEVAL_ENTROPY_SAMPLE_LIMIT];
        scores.extend([1000.0, 1000.0, 1000.0]);
        assert_close(normalized_retrieval_entropy(&scores), 1.0);
    }

    #[test]
    fn graph_fanout_is_capped_and_sanitized() {
        assert_close(capped_graph_fanout(-1.0), 0.0);
        assert_close(capped_graph_fanout(f64::NAN), 0.0);
        assert_close(capped_graph_fanout(1.5), 1.5);
        assert_close(capped_graph_fanout(99.0), GRAPH_FANOUT_CAP);
    }

    #[test]
    fn task_keyword_score_detects_complexity_markers_as_words() {
        assert_close(task_keyword_score("please refactor the pack scorer"), 0.3);
        assert_close(
            task_keyword_score("rewrite-and-verify context behavior"),
            0.3,
        );
        assert_close(task_keyword_score("prefixrefactor should not match"), 0.0);
    }

    #[test]
    fn classifier_clamps_to_max_tokens() {
        let scores = vec![1.0_f32; RETRIEVAL_ENTROPY_SAMPLE_LIMIT];
        let decision = classify_adaptive_budget(
            AdaptiveBudgetInput::new("security performance refactor", &scores, 100.0)
                .with_max_tokens(1_200),
        );
        assert_eq!(decision.computed_tokens, 1_200);
        assert_eq!(decision.base_tokens, 1_000);
        assert_eq!(decision.max_tokens, 1_200);
    }

    #[test]
    fn classifier_honors_small_max_token_ceiling() {
        let decision = classify_adaptive_budget(
            AdaptiveBudgetInput::new("simple lookup", &[], 0.0).with_max_tokens(512),
        );
        assert_eq!(decision.base_tokens, 512);
        assert_eq!(decision.computed_tokens, 512);
    }

    #[test]
    fn classifier_reports_explainable_contributions() {
        let scores = [0.9_f32, 0.7, 0.5, 0.3];
        let decision = classify_adaptive_budget(AdaptiveBudgetInput::new(
            "audit context ranking",
            &scores,
            2.0,
        ));
        assert_eq!(decision.schema, ADAPTIVE_BUDGET_SCHEMA_V1);
        assert!(decision.adaptive);
        assert!(decision.computed_tokens > DEFAULT_ADAPTIVE_BASE_TOKENS);
        assert_close(
            decision.classifier_contributions.graph_fanout_multiplier,
            0.6,
        );
        assert_close(
            decision.classifier_contributions.task_keyword_multiplier,
            0.06,
        );
    }
}
