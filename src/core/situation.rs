//! Situation classifier, comparison, and link-planning surfaces (EE-421).
//!
//! Provides commands for:
//! - Classifying task text with deterministic situation signals
//! - Comparing situation-shaped task text for shared routing evidence
//! - Planning reviewed situation links without mutating state implicitly

use super::build_info;
pub use crate::models::{
    ROUTING_DECISION_SCHEMA_V1, RoutingDecision, SITUATION_CLASSIFY_SCHEMA_V1,
    SITUATION_EXPLAIN_SCHEMA_V1, SITUATION_LINK_SCHEMA_V1, SITUATION_SHOW_SCHEMA_V1,
    SituationCategory, SituationConfidence as ConfidenceLevel, SituationLink,
    SituationLinkRelation, SituationReplayPolicy, SituationRoutingSurface,
};

pub const SITUATION_FIXTURE_METRICS_SCHEMA_V1: &str = "ee.situation.fixture_metrics.v1";
pub const SITUATION_COMPARE_SCHEMA_V1: &str = "ee.situation.compare.v1";
pub const SITUATION_LINK_DRY_RUN_SCHEMA_V1: &str = "ee.situation.link_dry_run.v1";
pub const SITUATION_HEURISTIC_SOURCE_V1: &str = "ee.situation.heuristics.v1";
const LINK_RECOMMENDATION_MIN_SCORE: f32 = 0.45;
const DRY_RUN_CREATED_AT: &str = "1970-01-01T00:00:00Z";

// ============================================================================
// Classification Result
// ============================================================================

/// Result of classifying task text.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassifyResult {
    pub version: &'static str,
    pub input_text: String,
    pub category: SituationCategory,
    pub confidence: ConfidenceLevel,
    pub confidence_score: f32,
    pub signals: Vec<ClassificationSignal>,
    pub alternative_categories: Vec<(SituationCategory, f32)>,
    pub routing_decisions: Vec<RoutingDecision>,
}

/// A signal that contributed to classification.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassificationSignal {
    pub signal_type: &'static str,
    pub pattern: String,
    pub weight: f32,
}

impl ClassifyResult {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Situation: {}\n", self.category.as_str());
        output.push_str(&format!(
            "Confidence: {} ({:.0}%)\n",
            self.confidence,
            self.confidence_score * 100.0
        ));
        output.push_str(&format!("Description: {}\n", self.category.description()));

        if !self.signals.is_empty() {
            output.push_str("\nSignals:\n");
            for signal in &self.signals {
                output.push_str(&format!(
                    "  - {}: \"{}\" (weight: {:.2})\n",
                    signal.signal_type, signal.pattern, signal.weight
                ));
            }
        }

        if !self.alternative_categories.is_empty() {
            output.push_str("\nAlternative categories:\n");
            for (cat, score) in &self.alternative_categories {
                output.push_str(&format!("  - {}: {:.0}%\n", cat.as_str(), score * 100.0));
            }
        }

        if !self.routing_decisions.is_empty() {
            output.push_str("\nRouting decisions:\n");
            for decision in &self.routing_decisions {
                output.push_str(&format!(
                    "  - {}: {} ({})\n",
                    decision.surface.as_str(),
                    routing_decision_target(decision),
                    decision.replay_policy.as_str()
                ));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "CLASSIFY|{}|{}|{:.2}",
            self.category.as_str(),
            self.confidence.as_str(),
            self.confidence_score
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let signals: Vec<serde_json::Value> = self
            .signals
            .iter()
            .map(|s| {
                serde_json::json!({
                    "signalType": s.signal_type,
                    "pattern": s.pattern,
                    "weight": stable_score_json(s.weight),
                    "sourceKind": "static_keyword_catalog",
                    "sourceId": SITUATION_HEURISTIC_SOURCE_V1,
                    "evidenceIds": [],
                })
            })
            .collect();

        let alternatives: Vec<serde_json::Value> = self
            .alternative_categories
            .iter()
            .map(|(cat, score)| {
                serde_json::json!({
                    "category": cat.as_str(),
                    "score": stable_score_json(*score),
                })
            })
            .collect();

        serde_json::json!({
            "command": "situation classify",
            "version": self.version,
            "inputText": self.input_text,
            "inputHash": stable_hash_id("situation_input", &self.input_text),
            "classificationMode": "heuristic_tagging",
            "heuristic": true,
            "decisioningAllowed": false,
            "plannerEligible": false,
            "sourceKind": "static_keyword_catalog",
            "sourceId": SITUATION_HEURISTIC_SOURCE_V1,
            "evidenceIds": [],
            "category": self.category.as_str(),
            "categoryDescription": self.category.description(),
            "confidence": self.confidence.as_str(),
            "confidenceScore": stable_score_json(self.confidence_score),
            "signals": signals,
            "alternativeCategories": alternatives,
            "routingDecisions": routing_decisions_json(&self.routing_decisions),
            "degraded": [],
            "provenance": [
                {
                    "sourceKind": "static_keyword_catalog",
                    "sourceId": SITUATION_HEURISTIC_SOURCE_V1,
                    "evidenceIds": []
                }
            ],
        })
    }
}

// ============================================================================
// Fixture Families And Metrics
// ============================================================================

/// Deterministic fixture case for situation classification evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SituationFixtureCase {
    pub id: &'static str,
    pub family: &'static str,
    pub task_text: &'static str,
    pub expected_category: SituationCategory,
    pub expected_fixture_ids: &'static [&'static str],
    pub expected_alternative_categories: &'static [SituationCategory],
}

/// Per-case result from evaluating a situation fixture.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationFixtureCaseResult {
    pub id: String,
    pub family: String,
    pub task_text: String,
    pub expected_category: SituationCategory,
    pub observed_category: SituationCategory,
    pub classification_correct: bool,
    pub expected_fixture_ids: Vec<String>,
    pub observed_fixture_ids: Vec<String>,
    pub routing_hits: u32,
    pub routing_expected: u32,
    pub expected_alternative_categories: Vec<SituationCategory>,
    pub observed_alternative_categories: Vec<SituationCategory>,
    pub alternative_hits: u32,
    pub alternative_expected: u32,
}

/// Aggregated metrics for one fixture family.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationFixtureFamilyMetric {
    pub family: String,
    pub case_count: u32,
    pub classification_precision: f32,
    pub routing_usefulness: f32,
    pub alternative_recall: Option<f32>,
}

/// Aggregated situation fixture evaluation metrics.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationFixtureEvaluation {
    pub schema: &'static str,
    pub version: &'static str,
    pub case_count: u32,
    pub classification_precision: f32,
    pub routing_usefulness: f32,
    pub alternative_recall: Option<f32>,
    pub families: Vec<SituationFixtureFamilyMetric>,
    pub cases: Vec<SituationFixtureCaseResult>,
}

impl SituationFixtureEvaluation {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let families: Vec<serde_json::Value> = self
            .families
            .iter()
            .map(|family| {
                serde_json::json!({
                    "family": &family.family,
                    "caseCount": family.case_count,
                    "classificationPrecision": stable_score_json(family.classification_precision),
                    "routingUsefulness": stable_score_json(family.routing_usefulness),
                    "alternativeRecall": optional_stable_score_json(family.alternative_recall),
                })
            })
            .collect();
        let cases: Vec<serde_json::Value> =
            self.cases.iter().map(fixture_case_result_json).collect();

        serde_json::json!({
            "schema": self.schema,
            "version": self.version,
            "caseCount": self.case_count,
            "classificationPrecision": stable_score_json(self.classification_precision),
            "routingUsefulness": stable_score_json(self.routing_usefulness),
            "alternativeRecall": optional_stable_score_json(self.alternative_recall),
            "families": families,
            "cases": cases,
        })
    }
}

#[derive(Clone, Debug)]
struct FamilyAccumulator {
    family: String,
    case_count: u32,
    classification_hits: u32,
    routing_hits: u32,
    routing_expected: u32,
    alternative_hits: u32,
    alternative_expected: u32,
}

impl FamilyAccumulator {
    fn new(family: &str) -> Self {
        Self {
            family: family.to_string(),
            case_count: 0,
            classification_hits: 0,
            routing_hits: 0,
            routing_expected: 0,
            alternative_hits: 0,
            alternative_expected: 0,
        }
    }

    fn metric(self) -> SituationFixtureFamilyMetric {
        SituationFixtureFamilyMetric {
            family: self.family,
            case_count: self.case_count,
            classification_precision: ratio(self.classification_hits, self.case_count),
            routing_usefulness: ratio(self.routing_hits, self.routing_expected),
            alternative_recall: optional_ratio(self.alternative_hits, self.alternative_expected),
        }
    }
}

/// Built-in deterministic fixtures for situation model quality checks.
#[must_use]
pub fn built_in_situation_fixture_cases() -> Vec<SituationFixtureCase> {
    vec![
        SituationFixtureCase {
            id: "classification_bug_fix",
            family: "classification_precision",
            task_text: "fix broken login crash",
            expected_category: SituationCategory::BugFix,
            expected_fixture_ids: &["fixture.situation.bug_fix", "fixture.preflight.standard"],
            expected_alternative_categories: &[],
        },
        SituationFixtureCase {
            id: "classification_feature",
            family: "classification_precision",
            task_text: "implement new search feature support",
            expected_category: SituationCategory::Feature,
            expected_fixture_ids: &["fixture.situation.feature", "fixture.preflight.standard"],
            expected_alternative_categories: &[],
        },
        SituationFixtureCase {
            id: "classification_refactor",
            family: "classification_precision",
            task_text: "refactor clean auth module",
            expected_category: SituationCategory::Refactor,
            expected_fixture_ids: &["fixture.situation.refactor", "fixture.preflight.standard"],
            expected_alternative_categories: &[],
        },
        SituationFixtureCase {
            id: "classification_documentation",
            family: "classification_precision",
            task_text: "write docs to explain configuration options",
            expected_category: SituationCategory::Documentation,
            expected_fixture_ids: &[
                "fixture.situation.documentation",
                "fixture.preflight.minimal",
            ],
            expected_alternative_categories: &[SituationCategory::Configuration],
        },
        SituationFixtureCase {
            id: "routing_deployment",
            family: "routing_usefulness",
            task_text: "release production deployment",
            expected_category: SituationCategory::Deployment,
            expected_fixture_ids: &["fixture.situation.deployment", "fixture.preflight.full"],
            expected_alternative_categories: &[
                SituationCategory::Release,
                SituationCategory::Review,
            ],
        },
        SituationFixtureCase {
            id: "routing_review",
            family: "routing_usefulness",
            task_text: "review audit changed files",
            expected_category: SituationCategory::Review,
            expected_fixture_ids: &["fixture.situation.review", "fixture.preflight.standard"],
            expected_alternative_categories: &[],
        },
        SituationFixtureCase {
            id: "alternative_release_bug",
            family: "alternative_recall",
            task_text: "fix failing release workflow",
            expected_category: SituationCategory::BugFix,
            expected_fixture_ids: &["fixture.situation.bug_fix", "fixture.preflight.standard"],
            expected_alternative_categories: &[SituationCategory::Release],
        },
        SituationFixtureCase {
            id: "alternative_testing_config",
            family: "alternative_recall",
            task_text: "add integration tests to verify config",
            expected_category: SituationCategory::Testing,
            expected_fixture_ids: &["fixture.situation.testing", "fixture.preflight.summary"],
            expected_alternative_categories: &[
                SituationCategory::Configuration,
                SituationCategory::Feature,
            ],
        },
        SituationFixtureCase {
            id: "low_confidence_unknown",
            family: "low_confidence",
            task_text: "triage ambiguous work",
            expected_category: SituationCategory::Unknown,
            expected_fixture_ids: &["fixture.situation.unknown", "fixture.preflight.summary"],
            expected_alternative_categories: &[],
        },
    ]
}

/// Evaluate built-in situation fixtures.
#[must_use]
pub fn evaluate_built_in_situation_fixtures() -> SituationFixtureEvaluation {
    evaluate_situation_fixtures(&built_in_situation_fixture_cases())
}

/// Evaluate deterministic situation fixture cases.
#[must_use]
pub fn evaluate_situation_fixtures(cases: &[SituationFixtureCase]) -> SituationFixtureEvaluation {
    let version = build_info().version;
    let mut results = Vec::with_capacity(cases.len());
    let mut families: Vec<FamilyAccumulator> = Vec::new();
    let mut classification_hits = 0;
    let mut routing_hits = 0;
    let mut routing_expected = 0;
    let mut alternative_hits = 0;
    let mut alternative_expected = 0;

    for case in cases {
        let classification = classify_task(case.task_text);
        let classification_correct = classification.category == case.expected_category;
        let observed_fixture_ids = observed_fixture_ids(&classification);
        let case_routing_hits = case
            .expected_fixture_ids
            .iter()
            .filter(|expected| {
                observed_fixture_ids
                    .iter()
                    .any(|observed| observed == **expected)
            })
            .count() as u32;
        let case_routing_expected = case.expected_fixture_ids.len() as u32;
        let observed_alternatives: Vec<SituationCategory> = classification
            .alternative_categories
            .iter()
            .map(|(category, _)| *category)
            .collect();
        let case_alternative_hits = case
            .expected_alternative_categories
            .iter()
            .filter(|expected| observed_alternatives.contains(expected))
            .count() as u32;
        let case_alternative_expected = case.expected_alternative_categories.len() as u32;

        if classification_correct {
            classification_hits += 1;
        }
        routing_hits += case_routing_hits;
        routing_expected += case_routing_expected;
        alternative_hits += case_alternative_hits;
        alternative_expected += case_alternative_expected;

        if let Some(family) = accumulator_for(&mut families, case.family) {
            family.case_count += 1;
            if classification_correct {
                family.classification_hits += 1;
            }
            family.routing_hits += case_routing_hits;
            family.routing_expected += case_routing_expected;
            family.alternative_hits += case_alternative_hits;
            family.alternative_expected += case_alternative_expected;
        }

        results.push(SituationFixtureCaseResult {
            id: case.id.to_string(),
            family: case.family.to_string(),
            task_text: case.task_text.to_string(),
            expected_category: case.expected_category,
            observed_category: classification.category,
            classification_correct,
            expected_fixture_ids: case
                .expected_fixture_ids
                .iter()
                .map(|fixture| (*fixture).to_string())
                .collect(),
            observed_fixture_ids,
            routing_hits: case_routing_hits,
            routing_expected: case_routing_expected,
            expected_alternative_categories: case.expected_alternative_categories.to_vec(),
            observed_alternative_categories: observed_alternatives,
            alternative_hits: case_alternative_hits,
            alternative_expected: case_alternative_expected,
        });
    }

    SituationFixtureEvaluation {
        schema: SITUATION_FIXTURE_METRICS_SCHEMA_V1,
        version,
        case_count: cases.len() as u32,
        classification_precision: ratio(classification_hits, cases.len() as u32),
        routing_usefulness: ratio(routing_hits, routing_expected),
        alternative_recall: optional_ratio(alternative_hits, alternative_expected),
        families: families
            .into_iter()
            .map(FamilyAccumulator::metric)
            .collect(),
        cases: results,
    }
}

fn accumulator_for<'a>(
    families: &'a mut Vec<FamilyAccumulator>,
    family: &str,
) -> Option<&'a mut FamilyAccumulator> {
    if !families.iter().any(|entry| entry.family == family) {
        families.push(FamilyAccumulator::new(family));
    }

    families.iter_mut().find(|entry| entry.family == family)
}

fn observed_fixture_ids(classification: &ClassifyResult) -> Vec<String> {
    classification
        .routing_decisions
        .iter()
        .filter(|decision| decision.surface == SituationRoutingSurface::FixtureFamily)
        .flat_map(|decision| decision.fixture_ids.iter().cloned())
        .collect()
}

fn ratio(numerator: u32, denominator: u32) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f32 / denominator as f32
    }
}

fn optional_ratio(numerator: u32, denominator: u32) -> Option<f32> {
    if denominator == 0 {
        None
    } else {
        Some(ratio(numerator, denominator))
    }
}

fn optional_stable_score_json(score: Option<f32>) -> serde_json::Value {
    score.map_or(serde_json::Value::Null, |value| {
        serde_json::json!(stable_score_json(value))
    })
}

fn category_values(categories: &[SituationCategory]) -> Vec<&'static str> {
    categories
        .iter()
        .map(|category| category.as_str())
        .collect()
}

fn fixture_case_result_json(result: &SituationFixtureCaseResult) -> serde_json::Value {
    serde_json::json!({
        "id": &result.id,
        "family": &result.family,
        "taskText": &result.task_text,
        "expectedCategory": result.expected_category.as_str(),
        "observedCategory": result.observed_category.as_str(),
        "classificationCorrect": result.classification_correct,
        "expectedFixtureIds": &result.expected_fixture_ids,
        "observedFixtureIds": &result.observed_fixture_ids,
        "routingHits": result.routing_hits,
        "routingExpected": result.routing_expected,
        "expectedAlternativeCategories": category_values(&result.expected_alternative_categories),
        "observedAlternativeCategories": category_values(&result.observed_alternative_categories),
        "alternativeHits": result.alternative_hits,
        "alternativeExpected": result.alternative_expected,
    })
}

// ============================================================================
// Situation Show
// ============================================================================

/// Situation details for show command.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationDetails {
    pub version: &'static str,
    pub situation_id: String,
    pub category: SituationCategory,
    pub original_text: String,
    pub created_at: String,
    pub context_hints: Vec<String>,
    pub related_memories: Vec<String>,
}

impl SituationDetails {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Situation: {}\n", self.situation_id);
        output.push_str(&format!("Category: {}\n", self.category.as_str()));
        output.push_str(&format!("Created: {}\n", self.created_at));
        output.push_str(&format!("Text: {}\n", self.original_text));

        if !self.context_hints.is_empty() {
            output.push_str("\nContext hints:\n");
            for hint in &self.context_hints {
                output.push_str(&format!("  - {hint}\n"));
            }
        }

        if !self.related_memories.is_empty() {
            output.push_str("\nRelated memories:\n");
            for mem in &self.related_memories {
                output.push_str(&format!("  - {mem}\n"));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "SHOW|{}|{}|{}",
            self.situation_id,
            self.category.as_str(),
            self.context_hints.len()
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "situation show",
            "version": self.version,
            "situationId": self.situation_id,
            "category": self.category.as_str(),
            "originalText": self.original_text,
            "createdAt": self.created_at,
            "contextHints": self.context_hints,
            "relatedMemories": self.related_memories,
        })
    }
}

// ============================================================================
// Situation Explain
// ============================================================================

/// Explanation of a situation.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationExplanation {
    pub version: &'static str,
    pub situation_id: String,
    pub category: SituationCategory,
    pub explanation: String,
    pub recommendations: Vec<String>,
    pub relevant_rules: Vec<String>,
    pub potential_risks: Vec<String>,
}

impl SituationExplanation {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Situation: {}\n", self.situation_id);
        output.push_str(&format!("Category: {}\n\n", self.category.as_str()));
        output.push_str(&format!("Explanation:\n{}\n", self.explanation));

        if !self.recommendations.is_empty() {
            output.push_str("\nRecommendations:\n");
            for rec in &self.recommendations {
                output.push_str(&format!("  - {rec}\n"));
            }
        }

        if !self.relevant_rules.is_empty() {
            output.push_str("\nRelevant rules:\n");
            for rule in &self.relevant_rules {
                output.push_str(&format!("  - {rule}\n"));
            }
        }

        if !self.potential_risks.is_empty() {
            output.push_str("\nPotential risks:\n");
            for risk in &self.potential_risks {
                output.push_str(&format!("  - {risk}\n"));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "EXPLAIN|{}|{}|{}",
            self.situation_id,
            self.category.as_str(),
            self.recommendations.len()
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "situation explain",
            "version": self.version,
            "situationId": self.situation_id,
            "category": self.category.as_str(),
            "explanation": self.explanation,
            "recommendations": self.recommendations,
            "relevantRules": self.relevant_rules,
            "potentialRisks": self.potential_risks,
        })
    }
}

// ============================================================================
// Situation Compare And Link Dry Run
// ============================================================================

/// Options for deterministic `ee situation compare --dry-run --json` planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SituationCompareOptions {
    pub source_situation_id: Option<String>,
    pub target_situation_id: Option<String>,
    pub source_text: String,
    pub target_text: String,
    pub evidence_ids: Vec<String>,
    pub created_at: Option<String>,
}

impl SituationCompareOptions {
    #[must_use]
    pub fn new(source_text: impl Into<String>, target_text: impl Into<String>) -> Self {
        Self {
            source_situation_id: None,
            target_situation_id: None,
            source_text: source_text.into(),
            target_text: target_text.into(),
            evidence_ids: Vec::new(),
            created_at: None,
        }
    }

    #[must_use]
    pub fn source_situation_id(mut self, situation_id: impl Into<String>) -> Self {
        self.source_situation_id = Some(situation_id.into());
        self
    }

    #[must_use]
    pub fn target_situation_id(mut self, situation_id: impl Into<String>) -> Self {
        self.target_situation_id = Some(situation_id.into());
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub fn created_at(mut self, created_at: impl Into<String>) -> Self {
        self.created_at = Some(created_at.into());
        self
    }
}

/// Compact side of a situation comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationCompareSide {
    pub situation_id: String,
    pub text: String,
    pub category: SituationCategory,
    pub confidence: ConfidenceLevel,
    pub confidence_score: f32,
    pub signal_patterns: Vec<String>,
    pub alternative_categories: Vec<SituationCategory>,
}

impl SituationCompareSide {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "situationId": &self.situation_id,
            "text": &self.text,
            "category": self.category.as_str(),
            "confidence": self.confidence.as_str(),
            "confidenceScore": stable_score_json(self.confidence_score),
            "signalPatterns": &self.signal_patterns,
            "alternativeCategories": category_values(&self.alternative_categories),
        })
    }
}

/// Shared evidence used to score a situation comparison.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SituationCompareOverlap {
    pub signal_patterns: Vec<String>,
    pub alternative_categories: Vec<SituationCategory>,
    pub routing_targets: Vec<String>,
}

impl SituationCompareOverlap {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "signalPatterns": &self.signal_patterns,
            "alternativeCategories": category_values(&self.alternative_categories),
            "routingTargets": &self.routing_targets,
        })
    }
}

/// Deterministic comparison report for two task situations.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationCompareReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub dry_run: bool,
    pub source: SituationCompareSide,
    pub target: SituationCompareSide,
    pub relation: SituationLinkRelation,
    pub confidence: ConfidenceLevel,
    pub confidence_score: f32,
    pub recommended: bool,
    pub overlap: SituationCompareOverlap,
    pub evidence_ids: Vec<String>,
    pub reasons: Vec<String>,
}

impl SituationCompareReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": self.command,
            "dryRun": self.dry_run,
            "source": self.source.data_json(),
            "target": self.target.data_json(),
            "relation": self.relation.as_str(),
            "confidence": self.confidence.as_str(),
            "confidenceScore": stable_score_json(self.confidence_score),
            "recommended": self.recommended,
            "overlap": self.overlap.data_json(),
            "evidenceIds": &self.evidence_ids,
            "reasons": &self.reasons,
        })
    }
}

/// Curation candidate that would back a dry-run situation link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SituationLinkCurationPlan {
    pub candidate_id: String,
    pub action: &'static str,
    pub status: &'static str,
    pub requires_review: bool,
    pub reason: String,
}

impl SituationLinkCurationPlan {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "candidateId": &self.candidate_id,
            "action": self.action,
            "status": self.status,
            "requiresReview": self.requires_review,
            "reason": &self.reason,
        })
    }
}

/// Dry-run plan for `ee situation link --dry-run --json`.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationLinkDryRunReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub dry_run: bool,
    pub would_write: bool,
    pub compare: SituationCompareReport,
    pub planned_link: Option<SituationLink>,
    pub curation_candidate: SituationLinkCurationPlan,
}

impl SituationLinkDryRunReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": self.command,
            "dryRun": self.dry_run,
            "wouldWrite": self.would_write,
            "compare": self.compare.data_json(),
            "plannedLink": self.planned_link.as_ref().map(situation_link_json),
            "curationCandidate": self.curation_candidate.data_json(),
        })
    }
}

/// Compare two situation texts and recommend an explainable relation.
#[must_use]
pub fn compare_situations(options: &SituationCompareOptions) -> SituationCompareReport {
    let source = classify_task(&options.source_text);
    let target = classify_task(&options.target_text);
    let source_side = compare_side(
        options.source_situation_id.as_deref(),
        "source",
        &options.source_text,
        &source,
    );
    let target_side = compare_side(
        options.target_situation_id.as_deref(),
        "target",
        &options.target_text,
        &target,
    );
    let overlap = compare_overlap(&source, &target);
    let confidence_score = link_confidence_score(&source, &target, &overlap);
    let confidence = confidence_for_score(confidence_score);
    let relation = relation_for(&source, &target, &overlap);
    let recommended = confidence_score >= LINK_RECOMMENDATION_MIN_SCORE;
    let reasons = compare_reasons(&source, &target, &overlap, confidence_score, recommended);

    SituationCompareReport {
        schema: SITUATION_COMPARE_SCHEMA_V1,
        command: "situation compare",
        dry_run: true,
        source: source_side,
        target: target_side,
        relation,
        confidence,
        confidence_score,
        recommended,
        overlap,
        evidence_ids: stable_strings(&options.evidence_ids),
        reasons,
    }
}

/// Build a non-mutating curation-backed link proposal for two situations.
#[must_use]
pub fn plan_situation_link_dry_run(options: &SituationCompareOptions) -> SituationLinkDryRunReport {
    let compare = compare_situations(options);
    let created_at = options.created_at.as_deref().unwrap_or(DRY_RUN_CREATED_AT);
    let planned_link = compare.recommended.then(|| {
        let mut link = SituationLink::new(
            stable_link_id(&compare),
            compare.source.situation_id.clone(),
            compare.target.situation_id.clone(),
            compare.relation,
            created_at,
        )
        .with_confidence(compare.confidence, compare.confidence_score);
        for evidence_id in &compare.evidence_ids {
            link = link.with_evidence(evidence_id.as_str());
        }
        link
    });
    let curation_candidate = SituationLinkCurationPlan {
        candidate_id: stable_curation_candidate_id(&compare),
        action: "propose_situation_link",
        status: "dry_run",
        requires_review: true,
        reason: curation_reason(&compare),
    };

    SituationLinkDryRunReport {
        schema: SITUATION_LINK_DRY_RUN_SCHEMA_V1,
        command: "situation link",
        dry_run: true,
        would_write: false,
        compare,
        planned_link,
        curation_candidate,
    }
}

// ============================================================================
// Classification Logic
// ============================================================================

/// Classify task text into a situation category.
#[must_use]
pub fn classify_task(text: &str) -> ClassifyResult {
    let version = build_info().version;
    let lower = text.to_lowercase();

    let mut scores: Vec<(SituationCategory, f32, Vec<ClassificationSignal>)> = Vec::new();

    // Bug fix signals
    let mut bug_signals = Vec::new();
    let mut bug_score: f32 = 0.0;
    for pattern in [
        "fix", "bug", "error", "issue", "broken", "crash", "fail", "wrong",
    ] {
        if lower.contains(pattern) {
            bug_score += 0.3;
            bug_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.3,
            });
        }
    }
    scores.push((SituationCategory::BugFix, bug_score.min(1.0), bug_signals));

    // Feature signals
    let mut feat_signals = Vec::new();
    let mut feat_score: f32 = 0.0;
    for pattern in [
        "add",
        "new",
        "feature",
        "implement",
        "create",
        "build",
        "support",
    ] {
        if lower.contains(pattern) {
            feat_score += 0.3;
            feat_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.3,
            });
        }
    }
    scores.push((
        SituationCategory::Feature,
        feat_score.min(1.0),
        feat_signals,
    ));

    // Refactor signals
    let mut refactor_signals = Vec::new();
    let mut refactor_score: f32 = 0.0;
    for pattern in [
        "refactor",
        "clean",
        "reorganize",
        "restructure",
        "simplify",
        "extract",
    ] {
        if lower.contains(pattern) {
            refactor_score += 0.4;
            refactor_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.4,
            });
        }
    }
    scores.push((
        SituationCategory::Refactor,
        refactor_score.min(1.0),
        refactor_signals,
    ));

    // Investigation signals
    let mut invest_signals = Vec::new();
    let mut invest_score: f32 = 0.0;
    for pattern in [
        "investigate",
        "debug",
        "understand",
        "explore",
        "why",
        "how",
        "what",
    ] {
        if lower.contains(pattern) {
            invest_score += 0.3;
            invest_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.3,
            });
        }
    }
    scores.push((
        SituationCategory::Investigation,
        invest_score.min(1.0),
        invest_signals,
    ));

    // Documentation signals
    let mut doc_signals = Vec::new();
    let mut doc_score: f32 = 0.0;
    for pattern in [
        "document", "readme", "comment", "explain", "describe", "docs",
    ] {
        if lower.contains(pattern) {
            doc_score += 0.4;
            doc_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.4,
            });
        }
    }
    scores.push((
        SituationCategory::Documentation,
        doc_score.min(1.0),
        doc_signals,
    ));

    // Testing signals
    let mut test_signals = Vec::new();
    let mut test_score: f32 = 0.0;
    for pattern in [
        "test",
        "spec",
        "assert",
        "verify",
        "coverage",
        "unit",
        "integration",
    ] {
        if lower.contains(pattern) {
            test_score += 0.35;
            test_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Testing,
        test_score.min(1.0),
        test_signals,
    ));

    // Configuration signals
    let mut config_signals = Vec::new();
    let mut config_score: f32 = 0.0;
    for pattern in [
        "config",
        "setting",
        "env",
        "variable",
        "option",
        "parameter",
    ] {
        if lower.contains(pattern) {
            config_score += 0.35;
            config_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Configuration,
        config_score.min(1.0),
        config_signals,
    ));

    // Deployment signals (infrastructure rollout, not version cuts; the
    // "release" / "changelog" patterns belong to the Release category below).
    let mut deploy_signals = Vec::new();
    let mut deploy_score: f32 = 0.0;
    for pattern in [
        "deploy",
        "rollout",
        "rollback",
        "publish",
        "ship",
        "production",
        "staging",
        "canary",
    ] {
        if lower.contains(pattern) {
            deploy_score += 0.35;
            deploy_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Deployment,
        deploy_score.min(1.0),
        deploy_signals,
    ));

    // Release signals (version cut, changelog, tag, release workflow).
    // Multiple distinct release-flavored keywords compound to clear the 0.7
    // confidence threshold required by eidetic_engine_cli-oofg.
    let mut release_signals = Vec::new();
    let mut release_score: f32 = 0.0;
    release_score += push_non_overlapping_keyword_signals(
        &lower,
        &[
            "version bump",
            "version_bump",
            "bump version",
            "cut release",
            "cargo publish",
            "release notes",
            "release workflow",
            "changelog",
            "semver",
            "release",
            "tag",
        ],
        0.4,
        &mut release_signals,
    );
    scores.push((
        SituationCategory::Release,
        release_score.min(1.0),
        release_signals,
    ));

    // Exploration signals (open-ended discovery; distinct from
    // Investigation, which targets a specific failure or unknown).
    let mut explore_signals = Vec::new();
    let mut explore_score: f32 = 0.0;
    for pattern in [
        "explore",
        "exploration",
        "spike",
        "prototype",
        "proof of concept",
        "research",
        "discovery",
        "feasibility",
        "evaluate",
        "what if",
    ] {
        if lower.contains(pattern) {
            explore_score += 0.4;
            explore_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.4,
            });
        }
    }
    scores.push((
        SituationCategory::Exploration,
        explore_score.min(1.0),
        explore_signals,
    ));

    // Incident-response signals (reactive triage of a live failure).
    let mut incident_signals = Vec::new();
    let mut incident_score: f32 = 0.0;
    for pattern in [
        "incident",
        "outage",
        "oncall",
        "on-call",
        "p0",
        "p1",
        "sev1",
        "sev2",
        "hotfix",
        "postmortem",
        "post-mortem",
        "regression",
        "live failure",
    ] {
        if lower.contains(pattern) {
            incident_score += 0.45;
            incident_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.45,
            });
        }
    }
    scores.push((
        SituationCategory::IncidentResponse,
        incident_score.min(1.0),
        incident_signals,
    ));

    // Review signals
    let mut review_signals = Vec::new();
    let mut review_score: f32 = 0.0;
    for pattern in ["review", "pr", "feedback", "approve", "check", "audit"] {
        if lower.contains(pattern) {
            review_score += 0.35;
            review_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Review,
        review_score.min(1.0),
        review_signals,
    ));

    // Sort by score descending. `total_cmp` over the previous
    // `partial_cmp(...).unwrap_or(Equal)` matches the determinism
    // hardening shipped at `src/core/conformal.rs:160` and
    // `src/core/focus_suggest.rs:566`. Today every `*_score` is a
    // pure sum of positive constants and could not become NaN, but
    // a future signal that divides or takes a log would silently
    // break the byte-identical JSON contract from AGENTS.md (same
    // DB + indexes + config + query ⇒ stable JSON output). Use the
    // total ordering now so the determinism guarantee survives any
    // such refactor.
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));

    let (category, raw_confidence_score, signals) = scores
        .first()
        .filter(|(_, score, _)| *score > 0.0)
        .cloned()
        .unwrap_or((SituationCategory::Unknown, 0.0, Vec::new()));

    // Honesty rule: a single keyword is task-shaped guessing, not evidence.
    // We cap single-signal confidence at 0.49 (below the medium threshold) so
    // the surface stays honest. But multi-signal hits inside the same category
    // (e.g. "release", "changelog", "tag") are real evidence — three distinct
    // tokens that all align on the same category cannot be coincidence — so we
    // let the score climb into the high band. This is what
    // eidetic_engine_cli-oofg's "release-flavored task >= 0.7" gate verifies.
    let signal_count = signals.len();
    let (confidence_score, confidence) = if signal_count >= 3 {
        let lifted = raw_confidence_score.min(0.95);
        (lifted, confidence_band_for(lifted))
    } else if signal_count == 2 && raw_confidence_score >= 0.7 {
        let lifted = raw_confidence_score.min(0.85);
        (lifted, confidence_band_for(lifted))
    } else {
        (raw_confidence_score.min(0.49), ConfidenceLevel::Low)
    };

    let alternative_categories: Vec<(SituationCategory, f32)> = scores
        .iter()
        .skip(1)
        .filter(|(_, score, _)| *score > 0.0)
        .take(3)
        .map(|(cat, score, _)| (*cat, *score))
        .collect();
    let routing_decisions = if text.trim().is_empty() {
        Vec::new()
    } else {
        route_situation_with_alternatives(
            category,
            confidence,
            confidence_score,
            &alternative_categories,
        )
    };

    ClassifyResult {
        version,
        input_text: text.to_string(),
        category,
        confidence,
        confidence_score,
        signals,
        alternative_categories,
        routing_decisions,
    }
}

fn push_non_overlapping_keyword_signals(
    lower_text: &str,
    patterns: &[&'static str],
    weight: f32,
    signals: &mut Vec<ClassificationSignal>,
) -> f32 {
    let mut score = 0.0;
    let mut matched_patterns: Vec<&str> = Vec::new();

    for &pattern in patterns {
        if lower_text.contains(pattern)
            && !matched_patterns
                .iter()
                .any(|matched| matched.contains(pattern))
        {
            score += weight;
            matched_patterns.push(pattern);
            signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight,
            });
        }
    }

    score
}

/// Map a numeric confidence score to the discrete `ConfidenceLevel` band
/// using the thresholds defined on `SituationConfidence::threshold`.
fn confidence_band_for(score: f32) -> ConfidenceLevel {
    if score >= ConfidenceLevel::High.threshold() {
        ConfidenceLevel::High
    } else if score >= ConfidenceLevel::Medium.threshold() {
        ConfidenceLevel::Medium
    } else {
        ConfidenceLevel::Low
    }
}

/// Build deterministic route decisions for downstream surfaces.
#[must_use]
pub fn route_situation(
    category: SituationCategory,
    confidence: ConfidenceLevel,
    confidence_score: f32,
) -> Vec<RoutingDecision> {
    route_situation_with_alternatives(category, confidence, confidence_score, &[])
}

/// Build deterministic heuristic route hints for downstream surfaces.
#[must_use]
pub fn route_situation_with_alternatives(
    category: SituationCategory,
    confidence: ConfidenceLevel,
    confidence_score: f32,
    alternative_categories: &[(SituationCategory, f32)],
) -> Vec<RoutingDecision> {
    let mut decisions = Vec::new();
    let situation_id = stable_hash_id(
        "situation_route",
        &format!(
            "{}:{}:{:.3}",
            category.as_str(),
            confidence.as_str(),
            stable_score_json(confidence_score)
        ),
    );
    let fixture_ids = fixture_route_ids(category, alternative_categories);
    let preflight_profile = preflight_profile_for(category);

    let mut fixture_decision = RoutingDecision::new(
        stable_hash_id(
            "route",
            &format!("{}:{}:fixture_family", situation_id, category.as_str()),
        ),
        situation_id.clone(),
        SituationRoutingSurface::FixtureFamily,
        DRY_RUN_CREATED_AT,
    )
    .with_confidence(confidence, confidence_score)
    .replay_policy(SituationReplayPolicy::DryRunOnly)
    .with_reason(format!(
        "primary category {} selects fixture family {}",
        category.as_str(),
        situation_fixture_id(category)
    ))
    .with_reason(format!(
        "preflight fixture {} selected for {} tasks",
        preflight_fixture_id(category),
        category.as_str()
    ));

    for fixture_id in fixture_ids {
        fixture_decision = fixture_decision.with_fixture(fixture_id);
    }

    decisions.push(fixture_decision);

    decisions.push(
        RoutingDecision::new(
            stable_hash_id(
                "route",
                &format!("{}:{}:preflight_profile", situation_id, category.as_str()),
            ),
            situation_id.clone(),
            SituationRoutingSurface::PreflightProfile,
            DRY_RUN_CREATED_AT,
        )
        .with_confidence(confidence, confidence_score)
        .preflight_profile(preflight_profile)
        .replay_policy(SituationReplayPolicy::DryRunOnly)
        .with_reason(format!(
            "category {} maps to {} preflight coverage",
            category.as_str(),
            preflight_profile
        )),
    );

    for high_risk_category in high_risk_categories(category, alternative_categories) {
        let tripwire_id = tripwire_candidate_id(high_risk_category);
        decisions.push(
            RoutingDecision::new(
                stable_hash_id(
                    "route",
                    &format!(
                        "{}:{}:tripwire_candidate",
                        situation_id,
                        high_risk_category.as_str()
                    ),
                ),
                situation_id.clone(),
                SituationRoutingSurface::TripwireCandidate,
                DRY_RUN_CREATED_AT,
            )
            .with_confidence(confidence, confidence_score)
            .with_tripwire_candidate(tripwire_id)
            .replay_policy(SituationReplayPolicy::DryRunOnly)
            .with_reason(format!(
                "high-risk category {} requires {}",
                high_risk_category.as_str(),
                tripwire_id
            )),
        );
    }

    if confidence == ConfidenceLevel::Low {
        decisions.push(
            RoutingDecision::new(
                stable_hash_id(
                    "route",
                    &format!("{}:{}:manual_review", situation_id, category.as_str()),
                ),
                situation_id,
                SituationRoutingSurface::ManualReview,
                DRY_RUN_CREATED_AT,
            )
            .with_confidence(confidence, confidence_score)
            .replay_policy(SituationReplayPolicy::DryRunOnly)
            .with_reason(
                "low-confidence heuristic classification requires human review before mutation",
            ),
        );
    }

    decisions
}

fn fixture_route_ids(
    category: SituationCategory,
    alternative_categories: &[(SituationCategory, f32)],
) -> Vec<String> {
    let mut fixture_ids = vec![
        situation_fixture_id(category).to_string(),
        preflight_fixture_id(category).to_string(),
    ];

    if category != SituationCategory::Unknown {
        for (alternative, _) in alternative_categories {
            let fixture_id = situation_fixture_id(*alternative).to_string();
            if !fixture_ids.contains(&fixture_id) {
                fixture_ids.push(fixture_id);
            }
        }
    }

    fixture_ids
}

fn situation_fixture_id(category: SituationCategory) -> &'static str {
    match category {
        SituationCategory::BugFix => "fixture.situation.bug_fix",
        SituationCategory::Feature => "fixture.situation.feature",
        SituationCategory::Refactor => "fixture.situation.refactor",
        SituationCategory::Investigation => "fixture.situation.investigation",
        SituationCategory::Documentation => "fixture.situation.documentation",
        SituationCategory::Testing => "fixture.situation.testing",
        SituationCategory::Configuration => "fixture.situation.configuration",
        SituationCategory::Deployment => "fixture.situation.deployment",
        SituationCategory::Release => "fixture.situation.release",
        SituationCategory::Exploration => "fixture.situation.exploration",
        SituationCategory::IncidentResponse => "fixture.situation.incident_response",
        SituationCategory::Review => "fixture.situation.review",
        SituationCategory::Unknown => "fixture.situation.unknown",
    }
}

fn preflight_fixture_id(category: SituationCategory) -> &'static str {
    match category {
        SituationCategory::Deployment
        | SituationCategory::Release
        | SituationCategory::IncidentResponse => "fixture.preflight.full",
        SituationCategory::Documentation => "fixture.preflight.minimal",
        SituationCategory::Testing
        | SituationCategory::Exploration
        | SituationCategory::Unknown => "fixture.preflight.summary",
        SituationCategory::BugFix
        | SituationCategory::Feature
        | SituationCategory::Refactor
        | SituationCategory::Investigation
        | SituationCategory::Configuration
        | SituationCategory::Review => "fixture.preflight.standard",
    }
}

fn preflight_profile_for(category: SituationCategory) -> &'static str {
    match preflight_fixture_id(category) {
        "fixture.preflight.full" => "preflight.full",
        "fixture.preflight.minimal" => "preflight.minimal",
        "fixture.preflight.summary" => "preflight.summary",
        _ => "preflight.standard",
    }
}

fn high_risk_categories(
    category: SituationCategory,
    alternative_categories: &[(SituationCategory, f32)],
) -> Vec<SituationCategory> {
    let mut categories = Vec::new();
    if is_high_risk_category(category) {
        categories.push(category);
    }
    for (alternative, _) in alternative_categories {
        if is_high_risk_category(*alternative) && !categories.contains(alternative) {
            categories.push(*alternative);
        }
    }
    categories
}

fn is_high_risk_category(category: SituationCategory) -> bool {
    matches!(
        category,
        SituationCategory::Deployment
            | SituationCategory::Release
            | SituationCategory::IncidentResponse
    )
}

fn tripwire_candidate_id(category: SituationCategory) -> &'static str {
    match category {
        SituationCategory::Deployment => "tripwire.deployment_readiness",
        SituationCategory::Release => "tripwire.release_readiness",
        SituationCategory::IncidentResponse => "tripwire.incident_response",
        _ => "tripwire.manual_review",
    }
}

fn category_list(categories: &[SituationCategory]) -> String {
    categories
        .iter()
        .map(|category| category.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn routing_decision_target(decision: &RoutingDecision) -> &str {
    decision
        .selected_profile
        .as_deref()
        .or(decision.retrieval_profile.as_deref())
        .or(decision.preflight_profile.as_deref())
        .or_else(|| decision.procedure_candidate_ids.first().map(String::as_str))
        .or_else(|| decision.fixture_ids.first().map(String::as_str))
        .or_else(|| decision.tripwire_candidate_ids.first().map(String::as_str))
        .unwrap_or("manual_review")
}

fn routing_decisions_json(decisions: &[RoutingDecision]) -> Vec<serde_json::Value> {
    decisions.iter().map(routing_decision_json).collect()
}

fn routing_decision_json(decision: &RoutingDecision) -> serde_json::Value {
    serde_json::json!({
        "schema": ROUTING_DECISION_SCHEMA_V1,
        "routingId": &decision.routing_id,
        "situationId": &decision.situation_id,
        "surface": decision.surface.as_str(),
        "confidence": decision.confidence.as_str(),
        "confidenceScore": stable_score_json(decision.confidence_score),
        "selectedProfile": &decision.selected_profile,
        "retrievalProfile": &decision.retrieval_profile,
        "preflightProfile": &decision.preflight_profile,
        "procedureCandidateIds": &decision.procedure_candidate_ids,
        "fixtureIds": &decision.fixture_ids,
        "tripwireCandidateIds": &decision.tripwire_candidate_ids,
        "replayPolicy": decision.replay_policy.as_str(),
        "reasons": &decision.reasons,
        "createdAt": &decision.created_at,
    })
}

fn compare_side(
    situation_id: Option<&str>,
    fallback_prefix: &str,
    text: &str,
    classification: &ClassifyResult,
) -> SituationCompareSide {
    SituationCompareSide {
        situation_id: situation_id
            .map(str::to_owned)
            .unwrap_or_else(|| stable_transient_id(fallback_prefix, text)),
        text: text.to_string(),
        category: classification.category,
        confidence: classification.confidence,
        confidence_score: classification.confidence_score,
        signal_patterns: stable_signal_patterns(classification),
        alternative_categories: classification
            .alternative_categories
            .iter()
            .map(|(category, _)| *category)
            .collect(),
    }
}

fn compare_overlap(source: &ClassifyResult, target: &ClassifyResult) -> SituationCompareOverlap {
    SituationCompareOverlap {
        signal_patterns: shared_signal_patterns(source, target),
        alternative_categories: shared_alternative_categories(source, target),
        routing_targets: shared_routing_targets(source, target),
    }
}

fn stable_signal_patterns(classification: &ClassifyResult) -> Vec<String> {
    stable_strings(
        &classification
            .signals
            .iter()
            .map(|signal| signal.pattern.clone())
            .collect::<Vec<_>>(),
    )
}

fn shared_signal_patterns(source: &ClassifyResult, target: &ClassifyResult) -> Vec<String> {
    let target_patterns = stable_signal_patterns(target);
    stable_signal_patterns(source)
        .into_iter()
        .filter(|pattern| target_patterns.contains(pattern))
        .collect()
}

fn shared_alternative_categories(
    source: &ClassifyResult,
    target: &ClassifyResult,
) -> Vec<SituationCategory> {
    let mut shared = Vec::new();
    let source_categories: Vec<_> = source
        .alternative_categories
        .iter()
        .map(|(category, _)| *category)
        .collect();
    let target_categories: Vec<_> = target
        .alternative_categories
        .iter()
        .map(|(category, _)| *category)
        .collect();

    if source_categories.contains(&target.category) {
        shared.push(target.category);
    }
    if target_categories.contains(&source.category) && !shared.contains(&source.category) {
        shared.push(source.category);
    }
    for category in source_categories {
        if target_categories.contains(&category) && !shared.contains(&category) {
            shared.push(category);
        }
    }
    shared.sort_by_key(|category| category.as_str());
    shared
}

fn shared_routing_targets(source: &ClassifyResult, target: &ClassifyResult) -> Vec<String> {
    let target_targets = routing_targets(target);
    routing_targets(source)
        .into_iter()
        .filter(|target_name| target_targets.contains(target_name))
        .collect()
}

fn routing_targets(classification: &ClassifyResult) -> Vec<String> {
    stable_strings(
        &classification
            .routing_decisions
            .iter()
            .map(|decision| {
                format!(
                    "{}:{}",
                    decision.surface.as_str(),
                    routing_decision_target(decision)
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn link_confidence_score(
    source: &ClassifyResult,
    target: &ClassifyResult,
    overlap: &SituationCompareOverlap,
) -> f32 {
    let mut score: f32 = 0.0;
    if source.category == target.category && source.category != SituationCategory::Unknown {
        score += 0.45;
    }
    if !overlap.alternative_categories.is_empty() {
        score += 0.25;
    }
    if !overlap.signal_patterns.is_empty() {
        score += (overlap.signal_patterns.len() as f32 * 0.10).min(0.20);
    }
    if !overlap.routing_targets.is_empty() {
        score += (overlap.routing_targets.len() as f32 * 0.04).min(0.10);
    }
    if source.confidence == ConfidenceLevel::Low || target.confidence == ConfidenceLevel::Low {
        score -= 0.15;
    }
    let capped_score = score.clamp(0.0, 1.0);
    if source.confidence == ConfidenceLevel::Low || target.confidence == ConfidenceLevel::Low {
        capped_score.min(LINK_RECOMMENDATION_MIN_SCORE - 0.001)
    } else {
        capped_score
    }
}

fn confidence_for_score(score: f32) -> ConfidenceLevel {
    if score >= 0.75 {
        ConfidenceLevel::High
    } else if score >= LINK_RECOMMENDATION_MIN_SCORE {
        ConfidenceLevel::Medium
    } else {
        ConfidenceLevel::Low
    }
}

fn relation_for(
    source: &ClassifyResult,
    target: &ClassifyResult,
    overlap: &SituationCompareOverlap,
) -> SituationLinkRelation {
    if source.category == target.category && source.category != SituationCategory::Unknown {
        SituationLinkRelation::Similar
    } else if !overlap.alternative_categories.is_empty() {
        SituationLinkRelation::CoOccurs
    } else if source.confidence == ConfidenceLevel::Low || target.confidence == ConfidenceLevel::Low
    {
        SituationLinkRelation::Contrasts
    } else {
        SituationLinkRelation::CoOccurs
    }
}

fn compare_reasons(
    source: &ClassifyResult,
    target: &ClassifyResult,
    overlap: &SituationCompareOverlap,
    score: f32,
    recommended: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if source.category == target.category && source.category != SituationCategory::Unknown {
        reasons.push(format!(
            "both situations classify as {}",
            source.category.as_str()
        ));
    }
    if !overlap.alternative_categories.is_empty() {
        reasons.push(format!(
            "classification alternatives overlap through {}",
            category_list(&overlap.alternative_categories)
        ));
    }
    if !overlap.signal_patterns.is_empty() {
        reasons.push(format!(
            "shared signal pattern(s): {}",
            overlap.signal_patterns.join(", ")
        ));
    }
    if !overlap.routing_targets.is_empty() {
        reasons.push(format!(
            "shared routing target(s): {}",
            overlap.routing_targets.join(", ")
        ));
    }
    reasons.push(
        "heuristic tags are not sufficient evidence for automatic situation link recommendation"
            .to_string(),
    );
    if recommended {
        reasons.push(format!(
            "score {:.3} meets dry-run link recommendation threshold {:.3}",
            stable_score_json(score),
            LINK_RECOMMENDATION_MIN_SCORE
        ));
    } else {
        reasons.push(format!(
            "score {:.3} stays below dry-run link recommendation threshold {:.3}",
            stable_score_json(score),
            LINK_RECOMMENDATION_MIN_SCORE
        ));
    }
    reasons
}

fn curation_reason(compare: &SituationCompareReport) -> String {
    if compare.recommended {
        format!(
            "dry-run only: propose {} situation link for human review before durable mutation",
            compare.relation.as_str()
        )
    } else {
        "dry-run only: do not create a situation link without stronger shared evidence".to_string()
    }
}

fn stable_transient_id(prefix: &str, text: &str) -> String {
    stable_hash_id(prefix, text)
}

fn stable_link_id(compare: &SituationCompareReport) -> String {
    stable_hash_id(
        "sitlink",
        &format!(
            "{}:{}:{}",
            compare.source.situation_id,
            compare.target.situation_id,
            compare.relation.as_str()
        ),
    )
}

fn stable_curation_candidate_id(compare: &SituationCompareReport) -> String {
    stable_hash_id(
        "curation",
        &format!(
            "{}:{}:{}:{}",
            compare.source.situation_id,
            compare.target.situation_id,
            compare.relation.as_str(),
            compare.recommended
        ),
    )
}

fn stable_hash_id(prefix: &str, seed: &str) -> String {
    let hash = blake3::hash(seed.as_bytes()).to_hex().to_string();
    format!("{prefix}_{}", &hash[..16])
}

fn stable_strings(values: &[String]) -> Vec<String> {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted.dedup();
    sorted
}

fn situation_link_json(link: &SituationLink) -> serde_json::Value {
    serde_json::json!({
        "schema": SITUATION_LINK_SCHEMA_V1,
        "linkId": &link.link_id,
        "sourceSituationId": &link.source_situation_id,
        "targetSituationId": &link.target_situation_id,
        "relation": link.relation.as_str(),
        "directed": link.directed,
        "confidence": link.confidence.as_str(),
        "confidenceScore": stable_score_json(link.confidence_score),
        "evidenceIds": &link.evidence_ids,
        "createdAt": &link.created_at,
    })
}

fn stable_score_json(score: f32) -> f64 {
    if score.is_finite() {
        (f64::from(score) * 1_000.0).round() / 1_000.0
    } else {
        0.0
    }
}

/// Show details for a persisted situation.
pub fn show_situation(
    connection: &crate::db::DbConnection,
    situation_id: &str,
) -> Result<Option<SituationDetails>, crate::models::DomainError> {
    get_situation_record_details(connection, situation_id)
}

/// Explain a persisted situation.
pub fn explain_situation(
    connection: &crate::db::DbConnection,
    situation_id: &str,
) -> Result<Option<SituationExplanation>, crate::models::DomainError> {
    explain_situation_record(connection, situation_id)
}

// ============================================================================
// Persisted situation records (bd-1tp6p.2.1)
// ============================================================================

/// Schema version stamped into persisted situation records and used as
/// part of the idempotence fingerprint (bd-1tp6p.1 contract).
pub const SITUATION_RECORD_SCHEMA_VERSION: &str = "ee.situation.record.v1";

/// Display placeholder when a record stores no redacted original text.
const SITUATION_TEXT_REDACTED_PLACEHOLDER: &str = "[redacted]";

/// Domain input for the idempotent create-or-adopt write path. The
/// caller (the bd-1tp6p.2.2 adoption use case) supplies redaction,
/// hashing, classification serialization, and timestamps; the
/// repository owns id derivation and fingerprint idempotence.
#[derive(Clone, Debug)]
pub struct AdoptSituationRecordInput {
    pub workspace_scope: String,
    pub input_hash: String,
    pub original_text_redacted: Option<String>,
    pub category: SituationCategory,
    pub confidence: ConfidenceLevel,
    pub confidence_score: f64,
    pub signals_json: String,
    pub alternative_categories_json: String,
    pub routing_decisions_json: String,
    pub context_hints: Vec<String>,
    pub provenance_json: String,
    pub adopted_by: Option<String>,
    pub adoption_reason: Option<String>,
    pub created_at: String,
    pub adopted_at: String,
    pub classifier_algorithm: String,
    pub classifier_version: String,
    pub build_version: String,
}

/// Outcome of the idempotent adoption write: the persisted record's
/// domain view plus whether an existing record satisfied the request.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationAdoption {
    pub situation_id: String,
    pub details: SituationDetails,
    pub already_existed: bool,
}

/// Deterministic record id derived from the idempotence fingerprint
/// (SIT-STOR-MUST-003): the same workspace scope, input hash,
/// classifier algorithm, and schema version always yield the same id.
#[must_use]
pub fn deterministic_situation_record_id(
    workspace_scope: &str,
    input_hash: &str,
    classifier_algorithm: &str,
    schema_version: &str,
) -> String {
    let digest = blake3::hash(
        format!("{workspace_scope}|{input_hash}|{classifier_algorithm}|{schema_version}")
            .as_bytes(),
    )
    .to_hex()
    .to_string();
    format!("sit_{}", &digest[..26])
}

fn situation_storage_error(
    action: &str,
    error: impl std::fmt::Display,
) -> crate::models::DomainError {
    crate::models::DomainError::Storage {
        message: format!("Failed to {action} situation record: {error}"),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn situation_record_corrupt_error(situation_id: &str, column: &str) -> crate::models::DomainError {
    crate::models::DomainError::Storage {
        message: format!(
            "Persisted situation record `{situation_id}` has a corrupt `{column}` column; the stored JSON does not parse."
        ),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn stored_record_details(
    record: &crate::db::StoredSituationRecord,
) -> Result<SituationDetails, crate::models::DomainError> {
    let context_hints: Vec<String> = serde_json::from_str(&record.context_hints_json)
        .map_err(|_| situation_record_corrupt_error(&record.situation_id, "context_hints_json"))?;
    let category = record
        .category
        .parse::<SituationCategory>()
        .map_err(|_| situation_record_corrupt_error(&record.situation_id, "category"))?;
    Ok(SituationDetails {
        version: SITUATION_SHOW_SCHEMA_V1,
        situation_id: record.situation_id.clone(),
        category,
        original_text: record
            .original_text_redacted
            .clone()
            .unwrap_or_else(|| SITUATION_TEXT_REDACTED_PLACEHOLDER.to_owned()),
        created_at: record.created_at.clone(),
        context_hints,
        // Related-memory links are not part of the v1 stored record;
        // the empty list is the true stored state.
        related_memories: Vec::new(),
    })
}

/// Create a persisted situation record, or return the existing record
/// when the idempotence fingerprint already has one (bd-1tp6p.1
/// decision: repeated adoption returns the existing record with an
/// already-exists posture).
pub fn create_or_adopt_situation_record(
    connection: &crate::db::DbConnection,
    input: &AdoptSituationRecordInput,
) -> Result<SituationAdoption, crate::models::DomainError> {
    let lookup = |action: &str| -> Result<
        Option<crate::db::StoredSituationRecord>,
        crate::models::DomainError,
    > {
        connection
            .find_situation_record_by_fingerprint(
                &input.workspace_scope,
                &input.input_hash,
                &input.classifier_algorithm,
                SITUATION_RECORD_SCHEMA_VERSION,
            )
            .map_err(|error| situation_storage_error(action, error))
    };

    if let Some(existing) = lookup("look up")? {
        let details = stored_record_details(&existing)?;
        return Ok(SituationAdoption {
            situation_id: existing.situation_id,
            details,
            already_existed: true,
        });
    }

    let situation_id = deterministic_situation_record_id(
        &input.workspace_scope,
        &input.input_hash,
        &input.classifier_algorithm,
        SITUATION_RECORD_SCHEMA_VERSION,
    );
    let context_hints_json = serde_json::to_string(&input.context_hints)
        .map_err(|error| situation_storage_error("serialize context hints for", error))?;
    let record_input = crate::db::CreateSituationRecordInput {
        situation_id: situation_id.clone(),
        workspace_scope: input.workspace_scope.clone(),
        schema_version: SITUATION_RECORD_SCHEMA_VERSION.to_owned(),
        input_hash: input.input_hash.clone(),
        original_text_redacted: input.original_text_redacted.clone(),
        category: input.category.as_str().to_owned(),
        confidence: input.confidence.to_string(),
        confidence_score: input.confidence_score,
        signals_json: input.signals_json.clone(),
        alternative_categories_json: input.alternative_categories_json.clone(),
        routing_decisions_json: input.routing_decisions_json.clone(),
        context_hints_json,
        provenance_json: input.provenance_json.clone(),
        adopted_by: input.adopted_by.clone(),
        adoption_reason: input.adoption_reason.clone(),
        created_at: input.created_at.clone(),
        adopted_at: input.adopted_at.clone(),
        classifier_algorithm: input.classifier_algorithm.clone(),
        classifier_version: input.classifier_version.clone(),
        build_version: input.build_version.clone(),
    };

    match connection.insert_situation_record(&record_input) {
        Ok(()) => {}
        Err(insert_error) => {
            // A concurrent adopter may have won the unique fingerprint
            // index between lookup and insert; the existing record then
            // satisfies this request. Any other failure is a real
            // storage error and must surface.
            if let Some(existing) = lookup("re-check")? {
                let details = stored_record_details(&existing)?;
                return Ok(SituationAdoption {
                    situation_id: existing.situation_id,
                    details,
                    already_existed: true,
                });
            }
            return Err(situation_storage_error("insert", insert_error));
        }
    }

    let stored = connection
        .get_situation_record(&situation_id)
        .map_err(|error| situation_storage_error("read back", error))?
        .ok_or_else(|| crate::models::DomainError::Storage {
            message: format!(
                "Situation record `{situation_id}` was not readable immediately after insert."
            ),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let details = stored_record_details(&stored)?;
    Ok(SituationAdoption {
        situation_id,
        details,
        already_existed: false,
    })
}

/// Read one persisted situation record as its domain view.
pub fn get_situation_record_details(
    connection: &crate::db::DbConnection,
    situation_id: &str,
) -> Result<Option<SituationDetails>, crate::models::DomainError> {
    let Some(record) = connection
        .get_situation_record(situation_id)
        .map_err(|error| situation_storage_error("read", error))?
    else {
        return Ok(None);
    };
    stored_record_details(&record).map(Some)
}

/// Read one persisted situation record by its idempotence fingerprint.
pub fn find_situation_record_details_by_fingerprint(
    connection: &crate::db::DbConnection,
    workspace_scope: &str,
    input_hash: &str,
    classifier_algorithm: &str,
) -> Result<Option<SituationDetails>, crate::models::DomainError> {
    let Some(record) = connection
        .find_situation_record_by_fingerprint(
            workspace_scope,
            input_hash,
            classifier_algorithm,
            SITUATION_RECORD_SCHEMA_VERSION,
        )
        .map_err(|error| situation_storage_error("find", error))?
    else {
        return Ok(None);
    };
    stored_record_details(&record).map(Some)
}

/// Derive the v1 explanation from a persisted record (bd-1tp6p.1:
/// explanation v1 is derived, not stored; recommendations are sorted by
/// stable routing id then text and empty arrays stay empty arrays).
pub fn explain_situation_record(
    connection: &crate::db::DbConnection,
    situation_id: &str,
) -> Result<Option<SituationExplanation>, crate::models::DomainError> {
    let Some(record) = connection
        .get_situation_record(situation_id)
        .map_err(|error| situation_storage_error("read", error))?
    else {
        return Ok(None);
    };
    let category = record
        .category
        .parse::<SituationCategory>()
        .map_err(|_| situation_record_corrupt_error(&record.situation_id, "category"))?;
    let routing: Vec<serde_json::Value> = serde_json::from_str(&record.routing_decisions_json)
        .map_err(|_| {
            situation_record_corrupt_error(&record.situation_id, "routing_decisions_json")
        })?;
    let context_hints: Vec<String> = serde_json::from_str(&record.context_hints_json)
        .map_err(|_| situation_record_corrupt_error(&record.situation_id, "context_hints_json"))?;

    let mut keyed_recommendations: Vec<(String, String)> = routing
        .iter()
        .filter_map(|decision| {
            let routing_id = decision
                .get("routingId")
                .and_then(serde_json::Value::as_str)?;
            let surface = decision
                .get("surface")
                .and_then(serde_json::Value::as_str)?;
            let profile = decision
                .get("selectedProfile")
                .and_then(serde_json::Value::as_str)?;
            Some((
                routing_id.to_owned(),
                format!("Route {surface} through the `{profile}` profile."),
            ))
        })
        .collect();
    keyed_recommendations.sort();
    let recommendations: Vec<String> = keyed_recommendations
        .into_iter()
        .map(|(_, text)| text)
        .collect();

    let mut relevant_rules = context_hints;
    relevant_rules.sort();

    Ok(Some(SituationExplanation {
        version: SITUATION_EXPLAIN_SCHEMA_V1,
        situation_id: record.situation_id.clone(),
        category,
        explanation: format!(
            "Adopted situation classified as `{}` with {} confidence (score {:.2}) by `{}`.",
            category.as_str(),
            record.confidence,
            record.confidence_score,
            record.classifier_algorithm
        ),
        recommendations,
        relevant_rules,
        // Risk derivation needs rule lookups that v1 records do not
        // store; the empty list is the true derived state.
        potential_risks: Vec::new(),
    }))
}

// ============================================================================
// Audited adoption use case (bd-1tp6p.2.2)
// ============================================================================

/// Domain request for the audited adoption use case: turn raw task text
/// into a reviewed persisted situation record. `ee situation classify`
/// stays non-mutating; this is the single write path (bd-1tp6p.1
/// product decision).
#[derive(Clone, Debug)]
pub struct AdoptSituationRequest {
    pub task_text: String,
    pub workspace_scope: String,
    pub adopted_by: Option<String>,
    pub adoption_reason: Option<String>,
    pub evidence_ids: Vec<String>,
    /// Deterministic timestamp override for tests and replay; `None`
    /// stamps the current time.
    pub as_of: Option<String>,
}

fn classify_signals_json(result: &ClassifyResult) -> String {
    let signals: Vec<serde_json::Value> = result
        .signals
        .iter()
        .map(|signal| {
            serde_json::json!({
                "signalType": signal.signal_type,
                "pattern": signal.pattern,
                "weight": stable_score_json(signal.weight),
                "sourceKind": "static_keyword_catalog",
                "sourceId": SITUATION_HEURISTIC_SOURCE_V1,
                "evidenceIds": [],
            })
        })
        .collect();
    serde_json::Value::Array(signals).to_string()
}

fn classify_alternatives_json(result: &ClassifyResult) -> String {
    let alternatives: Vec<serde_json::Value> = result
        .alternative_categories
        .iter()
        .map(|(category, score)| {
            serde_json::json!({
                "category": category.as_str(),
                "score": stable_score_json(*score),
            })
        })
        .collect();
    serde_json::Value::Array(alternatives).to_string()
}

/// Run the deterministic classifier over the task text and persist (or
/// idempotently reuse) a situation record. Raw task text never reaches
/// storage: the stored body passes secret redaction first, and the
/// fingerprint uses the stable input hash that `ee situation classify`
/// already reports.
pub fn adopt_situation_from_text(
    connection: &crate::db::DbConnection,
    request: &AdoptSituationRequest,
) -> Result<SituationAdoption, crate::models::DomainError> {
    let classify = classify_task(&request.task_text);
    let input_hash = stable_hash_id("situation_input", &request.task_text);
    let redacted_text = crate::policy::redact_secret_like_content(&request.task_text).content;

    // Context hints derive from the matched signal patterns; sorted and
    // deduplicated so adoption is order-independent and deterministic.
    let mut context_hints: Vec<String> = classify
        .signals
        .iter()
        .map(|signal| signal.pattern.clone())
        .collect();
    context_hints.sort();
    context_hints.dedup();

    // Evidence ids are caller-supplied provenance; canonicalize ordering
    // so alternate input orderings produce identical stored records.
    let mut evidence_ids = request.evidence_ids.clone();
    evidence_ids.sort();
    evidence_ids.dedup();
    let provenance_json = serde_json::Value::Array(
        evidence_ids
            .into_iter()
            .map(serde_json::Value::String)
            .collect(),
    )
    .to_string();

    let timestamp = request
        .as_of
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    create_or_adopt_situation_record(
        connection,
        &AdoptSituationRecordInput {
            workspace_scope: request.workspace_scope.clone(),
            input_hash,
            original_text_redacted: Some(redacted_text),
            category: classify.category,
            confidence: classify.confidence,
            confidence_score: f64::from(classify.confidence_score),
            signals_json: classify_signals_json(&classify),
            alternative_categories_json: classify_alternatives_json(&classify),
            routing_decisions_json: serde_json::Value::Array(routing_decisions_json(
                &classify.routing_decisions,
            ))
            .to_string(),
            context_hints,
            provenance_json,
            adopted_by: request.adopted_by.clone(),
            adoption_reason: request.adoption_reason.clone(),
            created_at: timestamp.clone(),
            adopted_at: timestamp,
            classifier_algorithm: SITUATION_HEURISTIC_SOURCE_V1.to_owned(),
            classifier_version: "1".to_owned(),
            build_version: build_info().version.to_owned(),
        },
    )
}

/// Options for the `ee situation adopt` command surface (bd-1tp6p.2.3).
#[derive(Clone, Debug)]
pub struct AdoptSituationCommandOptions<'a> {
    pub workspace_path: &'a std::path::Path,
    pub database_path: Option<&'a std::path::Path>,
    pub task_text: &'a str,
    pub adopted_by: Option<&'a str>,
    pub adoption_reason: Option<&'a str>,
    pub evidence_ids: &'a [String],
    pub as_of: Option<&'a str>,
}

/// Machine- and human-facing report for an adoption command run. All
/// fields mirror the PERSISTED record (not the request), so the
/// already-exists posture reports what is actually stored.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationAdoptReport {
    pub version: &'static str,
    pub situation_id: String,
    pub already_existed: bool,
    pub workspace_scope: String,
    pub input_hash: String,
    pub category: SituationCategory,
    pub confidence: String,
    pub confidence_score: f64,
    pub context_hints: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub created_at: String,
    pub adopted_at: String,
    pub record_schema_version: String,
    pub classifier_algorithm: String,
    pub classifier_version: String,
    pub build_version: String,
}

impl SituationAdoptReport {
    #[must_use]
    pub fn posture(&self) -> &'static str {
        if self.already_existed {
            "already_exists"
        } else {
            "adopted"
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "situation adopt",
            "version": self.version,
            "situationId": self.situation_id,
            "posture": self.posture(),
            "workspaceScope": self.workspace_scope,
            "inputHash": self.input_hash,
            "category": self.category.as_str(),
            "confidence": self.confidence,
            // Same 3-decimal stabilization as classify's stable_score_json:
            // the stored f64 carries f32-cast noise that must not leak
            // into the wire contract.
            "confidenceScore": (self.confidence_score * 1_000.0).round() / 1_000.0,
            "contextHints": self.context_hints,
            "evidenceIds": self.evidence_ids,
            "createdAt": self.created_at,
            "adoptedAt": self.adopted_at,
            "recordSchemaVersion": self.record_schema_version,
            "classifierAlgorithm": self.classifier_algorithm,
            "classifierVersion": self.classifier_version,
            "buildVersion": self.build_version,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Situation {} ({})\n", self.situation_id, self.posture());
        output.push_str(&format!(
            "Category: {} ({} confidence, score {:.2})\n",
            self.category.as_str(),
            self.confidence,
            self.confidence_score
        ));
        output.push_str(&format!("Adopted: {}\n", self.adopted_at));
        if !self.context_hints.is_empty() {
            output.push_str(&format!(
                "Context hints: {}\n",
                self.context_hints.join(", ")
            ));
        }
        if !self.evidence_ids.is_empty() {
            output.push_str(&format!("Evidence: {}\n", self.evidence_ids.join(", ")));
        }
        output
    }
}

fn adopt_resolve_workspace_path(
    path: &std::path::Path,
) -> Result<std::path::PathBuf, crate::models::DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(path)
    };
    absolute
        .canonicalize()
        .map_err(|error| crate::models::DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        })
}

fn adopt_stable_workspace_id(path: &std::path::Path) -> String {
    crate::core::workspace::stable_workspace_id(path)
}

/// Run the full `ee situation adopt` command flow: resolve the
/// workspace, open and migrate the database, run the audited adoption
/// use case, and report the persisted record.
pub fn adopt_situation_command(
    options: &AdoptSituationCommandOptions<'_>,
) -> Result<SituationAdoptReport, crate::models::DomainError> {
    let workspace_path = adopt_resolve_workspace_path(options.workspace_path)?;
    let database_path = options
        .database_path
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));

    let connection = crate::db::DbConnection::open_file(&database_path).map_err(|error| {
        crate::models::DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee init --workspace .".to_owned()),
        }
    })?;
    connection
        .migrate()
        .map_err(|error| crate::models::DomainError::Storage {
            message: format!("Failed to migrate database: {error}"),
            repair: Some("ee migrate run --workspace .".to_owned()),
        })?;

    let requested = adopt_stable_workspace_id(&workspace_path);
    let workspace_scope = crate::core::workspace::bound_workspace_id_or_hash(
        &connection,
        &requested,
        &[options.workspace_path, workspace_path.as_path()],
    )?;
    let request = AdoptSituationRequest {
        task_text: options.task_text.to_owned(),
        workspace_scope,
        adopted_by: options.adopted_by.map(str::to_owned),
        adoption_reason: options.adoption_reason.map(str::to_owned),
        evidence_ids: options.evidence_ids.to_vec(),
        as_of: options.as_of.map(str::to_owned),
    };
    let adoption = adopt_situation_from_text(&connection, &request)?;

    let stored = connection
        .get_situation_record(&adoption.situation_id)
        .map_err(|error| situation_storage_error("read back", error))?
        .ok_or_else(|| crate::models::DomainError::Storage {
            message: format!(
                "Situation record `{}` was not readable after adoption.",
                adoption.situation_id
            ),
            repair: Some("ee doctor --json".to_owned()),
        })?;

    let context_hints: Vec<String> = serde_json::from_str(&stored.context_hints_json)
        .map_err(|_| situation_record_corrupt_error(&stored.situation_id, "context_hints_json"))?;
    let evidence_ids: Vec<String> = serde_json::from_str(&stored.provenance_json)
        .map_err(|_| situation_record_corrupt_error(&stored.situation_id, "provenance_json"))?;
    let category = stored
        .category
        .parse::<SituationCategory>()
        .map_err(|_| situation_record_corrupt_error(&stored.situation_id, "category"))?;

    Ok(SituationAdoptReport {
        version: crate::models::SITUATION_ADOPT_SCHEMA_V1,
        situation_id: stored.situation_id,
        already_existed: adoption.already_existed,
        workspace_scope: stored.workspace_scope,
        input_hash: stored.input_hash,
        category,
        confidence: stored.confidence,
        confidence_score: stored.confidence_score,
        context_hints,
        evidence_ids,
        created_at: stored.created_at,
        adopted_at: stored.adopted_at,
        record_schema_version: stored.schema_version,
        classifier_algorithm: stored.classifier_algorithm,
        classifier_version: stored.classifier_version,
        build_version: stored.build_version,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn adopt_input(input_hash: &str) -> AdoptSituationRecordInput {
        AdoptSituationRecordInput {
            workspace_scope: "wsp_01234567890123456789012345".to_owned(),
            input_hash: input_hash.to_owned(),
            original_text_redacted: Some("fix the failing release workflow".to_owned()),
            category: SituationCategory::BugFix,
            confidence: ConfidenceLevel::High,
            confidence_score: 0.92,
            signals_json: "[]".to_owned(),
            alternative_categories_json: "[]".to_owned(),
            routing_decisions_json: "[]".to_owned(),
            context_hints: vec!["release".to_owned(), "ci".to_owned()],
            provenance_json: "[]".to_owned(),
            adopted_by: Some("test-agent".to_owned()),
            adoption_reason: Some("unit test".to_owned()),
            created_at: "2026-06-11T00:00:00Z".to_owned(),
            adopted_at: "2026-06-11T00:00:00Z".to_owned(),
            classifier_algorithm: "keyword_v1".to_owned(),
            classifier_version: "1".to_owned(),
            build_version: "0.0.0-test".to_owned(),
        }
    }

    fn open_situation_test_db() -> Result<crate::db::DbConnection, String> {
        let connection = crate::db::DbConnection::open_memory().map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;
        Ok(connection)
    }

    fn check(condition: bool, message: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.to_owned())
        }
    }

    #[test]
    fn adopt_situation_command_binds_to_path_keyed_workspace() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path();
        std::fs::create_dir(workspace.join(".ee")).map_err(|error| error.to_string())?;
        let database = workspace.join(".ee").join("ee.db");
        let connection =
            crate::db::DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let canonical = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let legacy_id = "wsp_00000000000000000000legacy";
        let hashed = adopt_stable_workspace_id(&canonical);
        check(
            hashed != legacy_id,
            "fixture id must differ from the current path hash",
        )?;
        connection
            .insert_workspace(
                legacy_id,
                &crate::db::CreateWorkspaceInput {
                    path: canonical.to_string_lossy().into_owned(),
                    name: Some("situation bind".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = adopt_situation_command(&AdoptSituationCommandOptions {
            workspace_path: workspace,
            database_path: None,
            task_text: "fix the failing release workflow",
            adopted_by: Some("test-agent"),
            adoption_reason: Some("bind test"),
            evidence_ids: &[],
            as_of: None,
        })
        .map_err(|error| error.message())?;
        check(
            report.workspace_scope == legacy_id,
            "situation adopt must bind workspace_scope to the stored workspace id",
        )
    }

    #[test]
    fn adopt_situation_record_is_idempotent() -> TestResult {
        let connection = open_situation_test_db()?;
        let input = adopt_input("blake3:adopt-a");

        let first =
            create_or_adopt_situation_record(&connection, &input).map_err(|e| e.to_string())?;
        check(!first.already_existed, "first adoption must create")?;
        check(
            first.situation_id.starts_with("sit_"),
            "situation id must use the sit_ prefix",
        )?;

        let second =
            create_or_adopt_situation_record(&connection, &input).map_err(|e| e.to_string())?;
        check(second.already_existed, "second adoption must be idempotent")?;
        check(
            second.situation_id == first.situation_id,
            "idempotent adoption must return the same deterministic id",
        )?;
        check(
            second.details == first.details,
            "idempotent adoption must return the same stored details",
        )?;

        let found = find_situation_record_details_by_fingerprint(
            &connection,
            &input.workspace_scope,
            &input.input_hash,
            &input.classifier_algorithm,
        )
        .map_err(|e| e.to_string())?;
        check(
            found.as_ref().map(|details| details.situation_id.as_str())
                == Some(first.situation_id.as_str()),
            "fingerprint lookup must find the adopted record",
        )
    }

    #[test]
    fn adopt_situation_record_returns_existing_on_fingerprint_conflict() -> TestResult {
        let connection = open_situation_test_db()?;
        let input = adopt_input("blake3:adopt-b");

        // Simulate a previously adopted record that used a different id
        // for the same fingerprint (for example an older id scheme):
        // adoption must return it instead of fabricating a second row.
        let existing_id = "sit_legacy0000000000000000001";
        connection
            .insert_situation_record(&crate::db::CreateSituationRecordInput {
                situation_id: existing_id.to_owned(),
                workspace_scope: input.workspace_scope.clone(),
                schema_version: SITUATION_RECORD_SCHEMA_VERSION.to_owned(),
                input_hash: input.input_hash.clone(),
                original_text_redacted: None,
                category: "bug_fix".to_owned(),
                confidence: "high".to_owned(),
                confidence_score: 0.9,
                signals_json: "[]".to_owned(),
                alternative_categories_json: "[]".to_owned(),
                routing_decisions_json: "[]".to_owned(),
                context_hints_json: "[]".to_owned(),
                provenance_json: "[]".to_owned(),
                adopted_by: None,
                adoption_reason: None,
                created_at: "2026-06-10T00:00:00Z".to_owned(),
                adopted_at: "2026-06-10T00:00:00Z".to_owned(),
                classifier_algorithm: input.classifier_algorithm.clone(),
                classifier_version: "1".to_owned(),
                build_version: "0.0.0-test".to_owned(),
            })
            .map_err(|e| e.to_string())?;

        let adoption =
            create_or_adopt_situation_record(&connection, &input).map_err(|e| e.to_string())?;
        check(
            adoption.already_existed,
            "fingerprint conflict must resolve to the existing record",
        )?;
        check(
            adoption.situation_id == existing_id,
            "conflict resolution must return the existing record id",
        )?;
        check(
            adoption.details.original_text == "[redacted]",
            "absent redacted text must render the explicit redaction placeholder",
        )
    }

    fn adopt_request(task_text: &str) -> AdoptSituationRequest {
        AdoptSituationRequest {
            task_text: task_text.to_owned(),
            workspace_scope: "wsp_01234567890123456789012345".to_owned(),
            adopted_by: Some("test-agent".to_owned()),
            adoption_reason: Some("unit test".to_owned()),
            evidence_ids: Vec::new(),
            as_of: Some("2026-06-11T00:00:00Z".to_owned()),
        }
    }

    #[test]
    fn adopt_from_text_persists_classification_and_redacts_secrets() -> TestResult {
        let connection = open_situation_test_db()?;
        let mut request = adopt_request("fix the failing release with api_key=sk-test-secret-1234");
        request.evidence_ids = vec!["cass-session://abc#L1".to_owned()];

        let adoption =
            adopt_situation_from_text(&connection, &request).map_err(|e| e.to_string())?;
        check(!adoption.already_existed, "first adoption must create")?;
        check(
            adoption.details.category == SituationCategory::BugFix,
            "classifier category must persist",
        )?;
        check(
            adoption.details.created_at == "2026-06-11T00:00:00Z",
            "deterministic as_of timestamp must be honored",
        )?;

        let stored = connection
            .get_situation_record(&adoption.situation_id)
            .map_err(|e| e.to_string())?
            .ok_or("adopted record must be stored")?;
        check(
            !stored
                .original_text_redacted
                .as_deref()
                .unwrap_or("")
                .contains("sk-test-secret-1234"),
            "secret-like task text must be redacted before storage",
        )?;
        check(
            stored.input_hash == stable_hash_id("situation_input", &request.task_text),
            "stored input hash must match the classify inputHash convention",
        )?;
        check(
            stored.provenance_json == "[\"cass-session://abc#L1\"]",
            "evidence ids must persist as canonical provenance",
        )
    }

    #[test]
    fn adopt_from_text_is_idempotent_and_deterministic() -> TestResult {
        let connection = open_situation_test_db()?;
        let request = adopt_request("fix the flaky retry bug in the importer");

        let first = adopt_situation_from_text(&connection, &request).map_err(|e| e.to_string())?;
        let second = adopt_situation_from_text(&connection, &request).map_err(|e| e.to_string())?;
        check(second.already_existed, "repeat adoption must be idempotent")?;
        check(
            second.situation_id == first.situation_id,
            "repeat adoption must return the same deterministic id",
        )?;

        let expected_id = deterministic_situation_record_id(
            &request.workspace_scope,
            &stable_hash_id("situation_input", &request.task_text),
            SITUATION_HEURISTIC_SOURCE_V1,
            SITUATION_RECORD_SCHEMA_VERSION,
        );
        check(
            first.situation_id == expected_id,
            "adopted id must derive from the idempotence fingerprint",
        )
    }

    #[test]
    fn adopt_from_text_canonicalizes_evidence_ordering() -> TestResult {
        let text = "investigate the slow startup path";

        let connection_a = open_situation_test_db()?;
        let mut request_a = adopt_request(text);
        request_a.evidence_ids = vec!["ev:b".to_owned(), "ev:a".to_owned(), "ev:a".to_owned()];
        let adoption_a =
            adopt_situation_from_text(&connection_a, &request_a).map_err(|e| e.to_string())?;
        let stored_a = connection_a
            .get_situation_record(&adoption_a.situation_id)
            .map_err(|e| e.to_string())?
            .ok_or("record a must be stored")?;

        let connection_b = open_situation_test_db()?;
        let mut request_b = adopt_request(text);
        request_b.evidence_ids = vec!["ev:a".to_owned(), "ev:b".to_owned()];
        let adoption_b =
            adopt_situation_from_text(&connection_b, &request_b).map_err(|e| e.to_string())?;
        let stored_b = connection_b
            .get_situation_record(&adoption_b.situation_id)
            .map_err(|e| e.to_string())?
            .ok_or("record b must be stored")?;

        check(
            stored_a.provenance_json == stored_b.provenance_json,
            "alternate evidence orderings must store identical provenance",
        )?;
        check(
            stored_a.provenance_json == "[\"ev:a\",\"ev:b\"]",
            "provenance must be sorted and deduplicated",
        )
    }

    #[test]
    fn adopt_from_text_with_empty_evidence_stays_empty() -> TestResult {
        let connection = open_situation_test_db()?;
        let request = adopt_request("document the new mesh onboarding flow");

        let adoption =
            adopt_situation_from_text(&connection, &request).map_err(|e| e.to_string())?;
        let stored = connection
            .get_situation_record(&adoption.situation_id)
            .map_err(|e| e.to_string())?
            .ok_or("record must be stored")?;
        check(
            stored.provenance_json == "[]",
            "empty evidence must persist as an empty array, never null",
        )
    }

    #[test]
    fn explain_situation_record_orders_recommendations_deterministically() -> TestResult {
        let connection = open_situation_test_db()?;
        let mut input = adopt_input("blake3:adopt-c");
        input.routing_decisions_json = serde_json::json!([
            {"routingId": "route_b", "surface": "pack", "selectedProfile": "thorough"},
            {"routingId": "route_a", "surface": "search", "selectedProfile": "balanced"},
        ])
        .to_string();
        input.context_hints = vec!["zeta".to_owned(), "alpha".to_owned()];

        let adoption =
            create_or_adopt_situation_record(&connection, &input).map_err(|e| e.to_string())?;
        let explanation = explain_situation_record(&connection, &adoption.situation_id)
            .map_err(|e| e.to_string())?
            .ok_or("adopted record must be explainable")?;

        check(
            explanation.recommendations
                == vec![
                    "Route search through the `balanced` profile.".to_owned(),
                    "Route pack through the `thorough` profile.".to_owned(),
                ],
            "recommendations must be sorted by stable routing id",
        )?;
        check(
            explanation.relevant_rules == vec!["alpha".to_owned(), "zeta".to_owned()],
            "relevant rules must be sorted deterministically",
        )?;
        check(
            explanation.potential_risks.is_empty(),
            "empty risk arrays must stay empty arrays",
        )?;

        let missing = explain_situation_record(&connection, "sit_missing000000000000000001")
            .map_err(|e| e.to_string())?;
        check(
            missing.is_none(),
            "a missing situation id must explain as None, not an error",
        )
    }
    const SITUATION_FIXTURE_METRICS_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/situation/fixture_metrics.json.golden");
    const LOW_CONFIDENCE_BROADENING_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/situation/low_confidence_broadening.json.golden");
    const HIGH_RISK_ALTERNATIVE_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/situation/high_risk_alternative.json.golden");

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn normalize_package_version(
        value: &mut serde_json::Value,
        pointer: &str,
        ctx: &str,
    ) -> TestResult {
        let version = value
            .pointer_mut(pointer)
            .ok_or_else(|| format!("{ctx}: missing package version at {pointer}"))?;
        ensure(
            version.as_str().is_some(),
            true,
            &format!("{ctx}: package version is a string"),
        )?;
        *version = serde_json::Value::String("[CARGO_PKG_VERSION]".to_owned());
        Ok(())
    }

    fn classification_envelope(result: &ClassifyResult) -> serde_json::Value {
        serde_json::json!({
            "schema": SITUATION_CLASSIFY_SCHEMA_V1,
            "success": true,
            "data": result.data_json(),
        })
    }

    #[test]
    fn situation_category_strings_are_stable() -> TestResult {
        ensure(SituationCategory::BugFix.as_str(), "bug_fix", "bug_fix")?;
        ensure(SituationCategory::Feature.as_str(), "feature", "feature")?;
        ensure(SituationCategory::Refactor.as_str(), "refactor", "refactor")?;
        ensure(
            SituationCategory::Investigation.as_str(),
            "investigation",
            "investigation",
        )?;
        ensure(
            SituationCategory::Documentation.as_str(),
            "documentation",
            "documentation",
        )?;
        ensure(SituationCategory::Testing.as_str(), "testing", "testing")?;
        ensure(
            SituationCategory::Configuration.as_str(),
            "configuration",
            "configuration",
        )?;
        ensure(
            SituationCategory::Deployment.as_str(),
            "deployment",
            "deployment",
        )?;
        ensure(SituationCategory::Review.as_str(), "review", "review")?;
        ensure(SituationCategory::Unknown.as_str(), "unknown", "unknown")
    }

    #[test]
    fn situation_category_parse_roundtrip() -> TestResult {
        for cat in SituationCategory::ALL {
            let parsed: SituationCategory = cat.as_str().parse().map_err(|e| format!("{e}"))?;
            ensure(parsed, *cat, "roundtrip")?;
        }
        Ok(())
    }

    #[test]
    fn classify_task_detects_bug_fix() -> TestResult {
        let result = classify_task("fix the broken login button");
        ensure(
            result.category,
            SituationCategory::BugFix,
            "bug fix category",
        )?;
        ensure(result.confidence_score > 0.0, true, "has confidence")
    }

    #[test]
    fn classify_task_detects_feature() -> TestResult {
        let result = classify_task("add new user profile page");
        ensure(
            result.category,
            SituationCategory::Feature,
            "feature category",
        )
    }

    #[test]
    fn classify_task_detects_refactor() -> TestResult {
        let result = classify_task("refactor the auth module to simplify flow");
        ensure(
            result.category,
            SituationCategory::Refactor,
            "refactor category",
        )
    }

    #[test]
    fn classify_task_returns_unknown_for_empty() -> TestResult {
        let result = classify_task("");
        ensure(
            result.category,
            SituationCategory::Unknown,
            "unknown for empty",
        )?;
        ensure(
            result.confidence,
            ConfidenceLevel::Low,
            "empty is low confidence",
        )?;
        ensure(result.confidence_score, 0.0, "empty score")?;
        ensure(
            result.routing_decisions.is_empty(),
            true,
            "empty input does not create routes",
        )
    }

    #[test]
    fn classify_result_json_has_required_fields() -> TestResult {
        let result = classify_task("fix bug in login");
        let json = result.data_json();

        ensure(json.get("command").is_some(), true, "has command")?;
        ensure(
            json.get("classificationMode")
                .and_then(serde_json::Value::as_str),
            Some("heuristic_tagging"),
            "classification mode",
        )?;
        ensure(
            json.get("heuristic").and_then(serde_json::Value::as_bool),
            Some(true),
            "heuristic flag",
        )?;
        ensure(
            json.get("decisioningAllowed")
                .and_then(serde_json::Value::as_bool),
            Some(false),
            "decisioning disabled",
        )?;
        ensure(json.get("category").is_some(), true, "has category")?;
        ensure(json.get("confidence").is_some(), true, "has confidence")?;
        ensure(json.get("signals").is_some(), true, "has signals")?;
        ensure(
            json.get("routingDecisions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|routes| !routes.is_empty()),
            true,
            "has heuristic routing decisions",
        )
    }

    #[test]
    fn classify_result_has_no_unavailable_degradation() -> TestResult {
        let json = classify_task("fix bug in login").data_json();
        let degraded = json
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "classify degraded must be an array".to_string())?;

        let retired_code = ["situation", "decisioning_unavailable"].join("_");
        ensure(degraded.is_empty(), true, "classify degraded is empty")?;
        ensure(
            json.to_string().contains(&retired_code),
            false,
            "retired unavailable code absent from classify JSON",
        )
    }

    #[test]
    fn stored_situation_show_and_explain_read_persisted_records() -> TestResult {
        let connection = open_situation_test_db()?;
        let input = adopt_input("blake3:show-a");
        let classifier_algorithm = input.classifier_algorithm.clone();
        let adoption =
            create_or_adopt_situation_record(&connection, &input).map_err(|e| e.to_string())?;

        let details = show_situation(&connection, &adoption.situation_id)
            .map_err(|e| e.to_string())?
            .ok_or("adopted situation must be showable")?;
        ensure(
            details.situation_id == adoption.situation_id,
            true,
            "show returns persisted situation id",
        )?;
        ensure(
            details.category,
            SituationCategory::BugFix,
            "show returns persisted category",
        )?;

        let explanation = explain_situation(&connection, &adoption.situation_id)
            .map_err(|e| e.to_string())?
            .ok_or("adopted situation must be explainable")?;
        ensure(
            explanation.situation_id == adoption.situation_id,
            true,
            "explain returns persisted situation id",
        )?;
        ensure(
            explanation.explanation.contains(&classifier_algorithm),
            true,
            "explain names classifier provenance",
        )?;

        let missing = show_situation(&connection, "sit_missing000000000000000001")
            .map_err(|e| e.to_string())?;
        ensure(missing.is_none(), true, "missing show returns None")
    }

    #[test]
    fn classify_task_labels_heuristics_with_fixture_routes() -> TestResult {
        let result = classify_task("fix failing release workflow");

        ensure(result.category, SituationCategory::BugFix, "bug-fix tag")?;
        ensure(
            result.confidence,
            ConfidenceLevel::Low,
            "heuristic confidence",
        )?;
        ensure(result.confidence_score, 0.49, "confidence capped")?;
        ensure(
            result.routing_decisions.iter().any(|decision| {
                decision.surface == SituationRoutingSurface::FixtureFamily
                    && decision
                        .fixture_ids
                        .contains(&"fixture.situation.bug_fix".to_string())
                    && decision
                        .fixture_ids
                        .contains(&"fixture.preflight.standard".to_string())
            }),
            true,
            "heuristic tags route fixture and preflight hints",
        )
    }

    #[test]
    fn ambiguous_heuristic_tags_broaden_fixture_routes() -> TestResult {
        let result = classify_task("docs fix");
        ensure(
            result.category,
            SituationCategory::Documentation,
            "top category",
        )?;
        ensure(result.confidence, ConfidenceLevel::Low, "low confidence")?;

        ensure(
            result
                .alternative_categories
                .iter()
                .any(|(category, score)| {
                    *category == SituationCategory::BugFix && (*score - 0.3).abs() < f32::EPSILON
                }),
            true,
            "bug-fix alternative preserved as heuristic tag",
        )?;
        ensure(
            result.routing_decisions.iter().any(|decision| {
                decision.surface == SituationRoutingSurface::FixtureFamily
                    && decision
                        .fixture_ids
                        .contains(&"fixture.situation.documentation".to_string())
                    && decision
                        .fixture_ids
                        .contains(&"fixture.situation.bug_fix".to_string())
            }),
            true,
            "low-confidence alternative broadens fixture route",
        )
    }

    #[test]
    fn high_risk_alternative_emits_tripwire_route() -> TestResult {
        let result = classify_task("fix failing release workflow");
        ensure(
            result.category,
            SituationCategory::BugFix,
            "top category stays bug fix",
        )?;
        ensure(
            result
                .alternative_categories
                .iter()
                .any(|(category, _)| *category == SituationCategory::Release),
            true,
            "release retained as alternative",
        )?;

        ensure(
            result.routing_decisions.iter().any(|decision| {
                decision.surface == SituationRoutingSurface::TripwireCandidate
                    && decision
                        .tripwire_candidate_ids
                        .contains(&"tripwire.release_readiness".to_string())
            }),
            true,
            "high-risk alternative produces a tripwire route",
        )
    }

    #[test]
    fn low_confidence_broadening_json_matches_golden() -> TestResult {
        let mut actual = classification_envelope(&classify_task("docs fix"));
        ensure(
            actual
                .pointer("/data/version")
                .and_then(|value| value.as_str()),
            Some(env!("CARGO_PKG_VERSION")),
            "low confidence broadening runtime package version",
        )?;
        let mut expected: serde_json::Value =
            serde_json::from_str(LOW_CONFIDENCE_BROADENING_GOLDEN).map_err(|e| e.to_string())?;
        normalize_package_version(&mut actual, "/data/version", "low confidence actual")?;
        normalize_package_version(&mut expected, "/data/version", "low confidence golden")?;

        ensure(actual, expected, "low confidence broadening golden")
    }

    #[test]
    fn high_risk_alternative_json_matches_golden() -> TestResult {
        let mut actual = classification_envelope(&classify_task("fix failing release workflow"));
        ensure(
            actual
                .pointer("/data/version")
                .and_then(|value| value.as_str()),
            Some(env!("CARGO_PKG_VERSION")),
            "high risk alternative runtime package version",
        )?;
        let mut expected: serde_json::Value =
            serde_json::from_str(HIGH_RISK_ALTERNATIVE_GOLDEN).map_err(|e| e.to_string())?;
        normalize_package_version(&mut actual, "/data/version", "high risk actual")?;
        normalize_package_version(&mut expected, "/data/version", "high risk golden")?;

        ensure(actual, expected, "high risk alternative golden")
    }

    #[test]
    fn compare_situations_declines_low_confidence_heuristic_link() -> TestResult {
        let report = compare_situations(
            &SituationCompareOptions::new("fix failing release workflow", "fix broken login crash")
                .source_situation_id("sit.release_bug")
                .target_situation_id("sit.login_bug")
                .with_evidence("feat.shared.fix"),
        );

        ensure(report.schema, SITUATION_COMPARE_SCHEMA_V1, "compare schema")?;
        ensure(report.dry_run, true, "compare is dry-run")?;
        ensure(report.source.category, SituationCategory::BugFix, "source")?;
        ensure(report.target.category, SituationCategory::BugFix, "target")?;
        ensure(report.relation, SituationLinkRelation::Similar, "relation")?;
        ensure(report.confidence, ConfidenceLevel::Low, "confidence")?;
        ensure(report.recommended, false, "recommended")?;
        ensure(
            report.overlap.signal_patterns,
            vec!["fix".to_string()],
            "shared signal",
        )?;
        ensure(
            report
                .overlap
                .routing_targets
                .contains(&"fixture_family:fixture.situation.bug_fix".to_string()),
            true,
            "shared routing target",
        )?;
        ensure(
            report
                .reasons
                .iter()
                .any(|reason| reason.contains("heuristic tags are not sufficient evidence")),
            true,
            "heuristic warning",
        )
    }

    #[test]
    fn situation_link_dry_run_plans_high_confidence_shared_link() -> TestResult {
        let report = plan_situation_link_dry_run(
            &SituationCompareOptions::new("fix broken login crash", "fix broken checkout crash")
                .source_situation_id("sit.login_bug")
                .target_situation_id("sit.checkout_bug")
                .with_evidence("feat.shared.bug_fix")
                .created_at("2026-05-01T00:00:00Z"),
        );

        ensure(report.compare.recommended, true, "recommended")?;
        ensure(
            report.compare.confidence,
            ConfidenceLevel::Medium,
            "link confidence",
        )?;
        ensure(
            report.compare.confidence_score >= LINK_RECOMMENDATION_MIN_SCORE,
            true,
            "score reaches recommendation threshold",
        )?;
        ensure(report.planned_link.is_some(), true, "planned link")?;
        ensure(
            report
                .curation_candidate
                .reason
                .contains("propose similar situation link"),
            true,
            "curation reason proposes reviewed link",
        )
    }

    #[test]
    fn situation_link_dry_run_declines_heuristic_only_link() -> TestResult {
        let report = plan_situation_link_dry_run(
            &SituationCompareOptions::new("fix failing release workflow", "fix broken login crash")
                .source_situation_id("sit.release_bug")
                .target_situation_id("sit.login_bug")
                .with_evidence("feat.shared.fix")
                .created_at("2026-05-01T00:00:00Z"),
        );

        ensure(
            report.schema,
            SITUATION_LINK_DRY_RUN_SCHEMA_V1,
            "link dry-run schema",
        )?;
        ensure(report.dry_run, true, "dry-run")?;
        ensure(report.would_write, false, "does not write")?;
        ensure(
            report.curation_candidate.action,
            "propose_situation_link",
            "curation action",
        )?;
        ensure(
            report.curation_candidate.requires_review,
            true,
            "manual review",
        )?;
        ensure(report.planned_link.is_none(), true, "no planned link")?;
        let json = report.data_json();
        ensure(
            json.get("plannedLink"),
            Some(&serde_json::Value::Null),
            "planned link is null",
        )
    }

    #[test]
    fn situation_link_dry_run_declines_weak_unknown_link() -> TestResult {
        let report = plan_situation_link_dry_run(&SituationCompareOptions::new(
            "triage ambiguous work",
            "polish onboarding language",
        ));

        ensure(report.compare.recommended, false, "not recommended")?;
        ensure(report.planned_link.is_none(), true, "no planned link")?;
        ensure(
            report
                .curation_candidate
                .reason
                .contains("without stronger shared evidence"),
            true,
            "curation reason",
        )?;
        ensure(
            report.data_json().get("plannedLink"),
            Some(&serde_json::Value::Null),
            "json null planned link",
        )
    }

    #[test]
    fn confidence_level_thresholds_are_ordered() -> TestResult {
        ensure(
            ConfidenceLevel::High.threshold() > ConfidenceLevel::Medium.threshold(),
            true,
            "high > medium",
        )?;
        ensure(
            ConfidenceLevel::Medium.threshold() > ConfidenceLevel::Low.threshold(),
            true,
            "medium > low",
        )
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure(
            SITUATION_CLASSIFY_SCHEMA_V1,
            "ee.situation.classify.v1",
            "classify schema",
        )?;
        ensure(
            SITUATION_SHOW_SCHEMA_V1,
            "ee.situation.show.v1",
            "show schema",
        )?;
        ensure(
            SITUATION_EXPLAIN_SCHEMA_V1,
            "ee.situation.explain.v1",
            "explain schema",
        )?;
        ensure(
            SITUATION_COMPARE_SCHEMA_V1,
            "ee.situation.compare.v1",
            "compare schema",
        )?;
        ensure(
            SITUATION_LINK_DRY_RUN_SCHEMA_V1,
            "ee.situation.link_dry_run.v1",
            "link dry-run schema",
        )
    }

    #[test]
    fn situation_fixture_metrics_cover_precision_routing_and_alternatives() -> TestResult {
        let evaluation = evaluate_built_in_situation_fixtures();

        ensure(evaluation.case_count, 9, "fixture case count")?;
        ensure(
            evaluation.classification_precision,
            1.0,
            "classification precision",
        )?;
        ensure(evaluation.routing_usefulness, 1.0, "routing usefulness")?;
        ensure(
            evaluation.alternative_recall,
            Some(1.0),
            "alternative recall",
        )?;
        ensure(evaluation.families.len(), 4, "fixture family count")?;
        ensure(
            evaluation
                .families
                .iter()
                .any(|family| family.family == "classification_precision"),
            true,
            "classification family present",
        )?;
        ensure(
            evaluation
                .families
                .iter()
                .any(|family| family.family == "routing_usefulness"),
            true,
            "routing family present",
        )?;
        ensure(
            evaluation
                .families
                .iter()
                .any(|family| family.family == "alternative_recall"),
            true,
            "alternative family present",
        )
    }

    #[test]
    fn situation_fixture_metrics_json_matches_golden() -> TestResult {
        let mut actual = evaluate_built_in_situation_fixtures().data_json();
        ensure(
            actual.pointer("/version").and_then(|value| value.as_str()),
            Some(env!("CARGO_PKG_VERSION")),
            "situation fixture metrics runtime package version",
        )?;
        let mut expected: serde_json::Value =
            serde_json::from_str(SITUATION_FIXTURE_METRICS_GOLDEN).map_err(|e| e.to_string())?;
        normalize_package_version(&mut actual, "/version", "fixture metrics actual")?;
        normalize_package_version(&mut expected, "/version", "fixture metrics golden")?;

        ensure(actual, expected, "situation fixture metrics golden")
    }
}
