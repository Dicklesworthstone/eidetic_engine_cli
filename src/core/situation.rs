//! Situation analysis for task classification and explanation (EE-421).
//!
//! Provides commands for:
//! - Classifying task text into situation categories
//! - Showing situation details
//! - Explaining situation context and recommendations

use super::build_info;
use crate::models::ContextProfileName;
pub use crate::models::{
    ROUTING_DECISION_SCHEMA_V1, RoutingDecision, SITUATION_CLASSIFY_SCHEMA_V1,
    SITUATION_EXPLAIN_SCHEMA_V1, SITUATION_LINK_SCHEMA_V1, SITUATION_SHOW_SCHEMA_V1,
    SituationCategory, SituationConfidence as ConfidenceLevel, SituationLink,
    SituationLinkRelation, SituationReplayPolicy, SituationRoutingSurface,
};

pub const SITUATION_FIXTURE_METRICS_SCHEMA_V1: &str = "ee.situation.fixture_metrics.v1";
pub const SITUATION_COMPARE_SCHEMA_V1: &str = "ee.situation.compare.v1";
pub const SITUATION_LINK_DRY_RUN_SCHEMA_V1: &str = "ee.situation.link_dry_run.v1";
const HIGH_RISK_ALTERNATIVE_MIN_SCORE: f32 = 0.3;
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
            "category": self.category.as_str(),
            "categoryDescription": self.category.description(),
            "confidence": self.confidence.as_str(),
            "confidenceScore": stable_score_json(self.confidence_score),
            "signals": signals,
            "alternativeCategories": alternatives,
            "routingDecisions": routing_decisions_json(&self.routing_decisions),
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
            expected_alternative_categories: &[SituationCategory::Review],
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
            expected_alternative_categories: &[SituationCategory::Deployment],
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

    // Deployment signals
    let mut deploy_signals = Vec::new();
    let mut deploy_score: f32 = 0.0;
    for pattern in [
        "deploy",
        "release",
        "publish",
        "ship",
        "push",
        "production",
        "staging",
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

    // Sort by score descending
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let (category, confidence_score, signals) = scores
        .first()
        .filter(|(_, score, _)| *score > 0.0)
        .cloned()
        .unwrap_or((SituationCategory::Unknown, 0.0, Vec::new()));

    let confidence = if confidence_score >= 0.8 {
        ConfidenceLevel::High
    } else if confidence_score >= 0.5 {
        ConfidenceLevel::Medium
    } else {
        ConfidenceLevel::Low
    };

    let alternative_categories: Vec<(SituationCategory, f32)> = scores
        .iter()
        .skip(1)
        .filter(|(_, score, _)| *score > 0.0)
        .take(3)
        .map(|(cat, score, _)| (*cat, *score))
        .collect();
    let routing_decisions = route_situation_with_alternatives(
        category,
        confidence,
        confidence_score,
        &alternative_categories,
    );

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

/// Build deterministic route decisions for downstream surfaces.
#[must_use]
pub fn route_situation(
    category: SituationCategory,
    confidence: ConfidenceLevel,
    confidence_score: f32,
) -> Vec<RoutingDecision> {
    route_situation_with_alternatives(category, confidence, confidence_score, &[])
}

/// Build deterministic route decisions, preserving relevant alternatives.
#[must_use]
pub fn route_situation_with_alternatives(
    category: SituationCategory,
    confidence: ConfidenceLevel,
    confidence_score: f32,
    alternative_categories: &[(SituationCategory, f32)],
) -> Vec<RoutingDecision> {
    let situation_id = transient_situation_id(category);
    let broadening_categories =
        low_confidence_broadening_categories(confidence, alternative_categories);
    let high_risk_alternatives = high_risk_alternative_categories(alternative_categories);
    let mut decisions = Vec::with_capacity(7);

    let context_profile = context_profile_for(category, confidence);
    let mut context_route = base_route(
        category,
        &situation_id,
        SituationRoutingSurface::ContextProfile,
        confidence,
        confidence_score,
    )
    .selected_profile(context_profile)
    .retrieval_profile(context_profile)
    .with_reason(context_reason(category, confidence, context_profile));
    if !broadening_categories.is_empty() {
        context_route =
            context_route.with_reason(low_confidence_broadening_reason(&broadening_categories));
    }
    decisions.push(context_route);

    let preflight_profile = preflight_profile_for(category, confidence);
    decisions.push(
        base_route(
            category,
            &situation_id,
            SituationRoutingSurface::PreflightProfile,
            confidence,
            confidence_score,
        )
        .preflight_profile(preflight_profile)
        .with_reason(preflight_reason(category, preflight_profile)),
    );

    let mut procedure_route = base_route(
        category,
        &situation_id,
        SituationRoutingSurface::ProcedureCandidate,
        confidence,
        confidence_score,
    );
    procedure_route = add_procedure_candidates(procedure_route, category);
    for alternative in &broadening_categories {
        procedure_route = add_procedure_candidates(procedure_route, *alternative);
    }
    decisions.push(procedure_route.with_reason(procedure_reason(category)));

    let mut fixture_route = base_route(
        category,
        &situation_id,
        SituationRoutingSurface::FixtureFamily,
        confidence,
        confidence_score,
    );
    for fixture in fixture_families_for(category, preflight_profile) {
        fixture_route = add_fixture(fixture_route, fixture);
    }
    for alternative in &broadening_categories {
        fixture_route = add_fixture(fixture_route, category_fixture_for(*alternative));
    }
    decisions.push(fixture_route.with_reason(fixture_reason(category)));

    if !high_risk_alternatives.is_empty() {
        let mut tripwire_route = base_route(
            category,
            &situation_id,
            SituationRoutingSurface::TripwireCandidate,
            confidence,
            confidence_score,
        );
        for alternative in &high_risk_alternatives {
            tripwire_route = add_tripwire_candidates(tripwire_route, *alternative);
        }
        decisions.push(tripwire_route.with_reason(high_risk_alternative_reason(
            category,
            &high_risk_alternatives,
        )));
    }

    decisions.push(
        base_route(
            category,
            &situation_id,
            SituationRoutingSurface::CounterfactualReplay,
            confidence,
            confidence_score,
        )
        .replay_policy(replay_policy_for(category, confidence))
        .with_reason(replay_reason(category)),
    );

    if category == SituationCategory::Unknown || confidence == ConfidenceLevel::Low {
        decisions.push(
            base_route(
                category,
                &situation_id,
                SituationRoutingSurface::ManualReview,
                ConfidenceLevel::Medium,
                0.5,
            )
            .with_reason("classification confidence is too low for fully automatic routing"),
        );
    }

    decisions
}

fn base_route(
    category: SituationCategory,
    situation_id: &str,
    surface: SituationRoutingSurface,
    confidence: ConfidenceLevel,
    confidence_score: f32,
) -> RoutingDecision {
    RoutingDecision::new(
        format!("route_{}_{}", category.as_str(), surface.as_str()),
        situation_id,
        surface,
        "1970-01-01T00:00:00Z",
    )
    .with_confidence(confidence, confidence_score)
}

fn transient_situation_id(category: SituationCategory) -> String {
    format!("transient_{}", category.as_str())
}

fn context_profile_for(category: SituationCategory, confidence: ConfidenceLevel) -> &'static str {
    if confidence == ConfidenceLevel::Low {
        return ContextProfileName::Thorough.as_str();
    }

    match category {
        SituationCategory::BugFix
        | SituationCategory::Deployment
        | SituationCategory::Investigation => "thorough",
        SituationCategory::Documentation => "compact",
        SituationCategory::Unknown => "compact",
        SituationCategory::Feature
        | SituationCategory::Refactor
        | SituationCategory::Testing
        | SituationCategory::Configuration
        | SituationCategory::Review => "balanced",
    }
}

fn preflight_profile_for(category: SituationCategory, confidence: ConfidenceLevel) -> &'static str {
    if confidence == ConfidenceLevel::Low {
        return "summary";
    }

    match category {
        SituationCategory::Deployment | SituationCategory::Configuration => "full",
        SituationCategory::BugFix
        | SituationCategory::Feature
        | SituationCategory::Refactor
        | SituationCategory::Review => "standard",
        SituationCategory::Investigation | SituationCategory::Testing => "summary",
        SituationCategory::Documentation | SituationCategory::Unknown => "minimal",
    }
}

fn procedure_candidates_for(category: SituationCategory) -> &'static [&'static str] {
    match category {
        SituationCategory::BugFix => &[
            "procedure.debug.reproduce_failure",
            "procedure.verify.regression_fix",
        ],
        SituationCategory::Feature => &[
            "procedure.feature.implementation",
            "procedure.verify.acceptance_tests",
        ],
        SituationCategory::Refactor => &[
            "procedure.refactor.behavior_preserving",
            "procedure.verify.regression_suite",
        ],
        SituationCategory::Investigation => &[
            "procedure.investigate.evidence_collection",
            "procedure.search.related_context",
        ],
        SituationCategory::Documentation => &["procedure.docs.contract_update"],
        SituationCategory::Testing => &[
            "procedure.test.contract_coverage",
            "procedure.test.golden_update",
        ],
        SituationCategory::Configuration => &[
            "procedure.config.validate_effective_settings",
            "procedure.verify.degraded_modes",
        ],
        SituationCategory::Deployment => &[
            "procedure.release.verification_checklist",
            "procedure.release.rollback_plan",
        ],
        SituationCategory::Review => &[
            "procedure.review.risk_scan",
            "procedure.verify.changed_files",
        ],
        SituationCategory::Unknown => &["procedure.manual.triage"],
    }
}

fn fixture_families_for(
    category: SituationCategory,
    preflight_profile: &'static str,
) -> [&'static str; 2] {
    let category_fixture = category_fixture_for(category);
    let preflight_fixture = match preflight_profile {
        "full" => "fixture.preflight.full",
        "standard" => "fixture.preflight.standard",
        "summary" => "fixture.preflight.summary",
        _ => "fixture.preflight.minimal",
    };
    [category_fixture, preflight_fixture]
}

fn category_fixture_for(category: SituationCategory) -> &'static str {
    match category {
        SituationCategory::BugFix => "fixture.situation.bug_fix",
        SituationCategory::Feature => "fixture.situation.feature",
        SituationCategory::Refactor => "fixture.situation.refactor",
        SituationCategory::Investigation => "fixture.situation.investigation",
        SituationCategory::Documentation => "fixture.situation.documentation",
        SituationCategory::Testing => "fixture.situation.testing",
        SituationCategory::Configuration => "fixture.situation.configuration",
        SituationCategory::Deployment => "fixture.situation.deployment",
        SituationCategory::Review => "fixture.situation.review",
        SituationCategory::Unknown => "fixture.situation.unknown",
    }
}

fn replay_policy_for(
    category: SituationCategory,
    confidence: ConfidenceLevel,
) -> SituationReplayPolicy {
    if confidence == ConfidenceLevel::Low {
        return SituationReplayPolicy::NotEligible;
    }

    match category {
        SituationCategory::BugFix
        | SituationCategory::Testing
        | SituationCategory::Investigation => SituationReplayPolicy::Allowed,
        SituationCategory::Refactor | SituationCategory::Deployment | SituationCategory::Review => {
            SituationReplayPolicy::DryRunOnly
        }
        SituationCategory::Feature
        | SituationCategory::Documentation
        | SituationCategory::Configuration
        | SituationCategory::Unknown => SituationReplayPolicy::NotEligible,
    }
}

fn context_reason(
    category: SituationCategory,
    confidence: ConfidenceLevel,
    profile: &str,
) -> String {
    if confidence == ConfidenceLevel::Low {
        return format!(
            "{} situations use the thorough context profile to preserve alternatives when classification confidence is low",
            category.as_str()
        );
    }

    format!(
        "{} situations route to the {profile} context profile",
        category.as_str()
    )
}

fn preflight_reason(category: SituationCategory, profile: &str) -> String {
    format!(
        "{} situations route to the {profile} preflight field profile",
        category.as_str()
    )
}

fn procedure_reason(category: SituationCategory) -> String {
    format!(
        "{} situations select procedure candidates before free-form execution",
        category.as_str()
    )
}

fn fixture_reason(category: SituationCategory) -> String {
    format!(
        "{} situations select classification and preflight fixture families",
        category.as_str()
    )
}

fn replay_reason(category: SituationCategory) -> String {
    format!(
        "{} situations declare counterfactual replay eligibility without mutating memory",
        category.as_str()
    )
}

fn low_confidence_broadening_categories(
    confidence: ConfidenceLevel,
    alternative_categories: &[(SituationCategory, f32)],
) -> Vec<SituationCategory> {
    if confidence != ConfidenceLevel::Low {
        return Vec::new();
    }

    alternative_categories
        .iter()
        .map(|(category, _)| *category)
        .collect()
}

fn high_risk_alternative_categories(
    alternative_categories: &[(SituationCategory, f32)],
) -> Vec<SituationCategory> {
    alternative_categories
        .iter()
        .filter(|(category, score)| {
            *score >= HIGH_RISK_ALTERNATIVE_MIN_SCORE && is_high_risk_situation(*category)
        })
        .map(|(category, _)| *category)
        .collect()
}

fn is_high_risk_situation(category: SituationCategory) -> bool {
    matches!(
        category,
        SituationCategory::Configuration | SituationCategory::Deployment
    )
}

fn low_confidence_broadening_reason(alternatives: &[SituationCategory]) -> String {
    format!(
        "low-confidence classification broadens routing to preserve alternatives: {}",
        category_list(alternatives)
    )
}

fn high_risk_alternative_reason(
    category: SituationCategory,
    alternatives: &[SituationCategory],
) -> String {
    format!(
        "{} remains the top classification, but high-risk alternative(s) {} add tripwire candidates",
        category.as_str(),
        category_list(alternatives)
    )
}

fn category_list(categories: &[SituationCategory]) -> String {
    categories
        .iter()
        .map(|category| category.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn tripwire_candidates_for(category: SituationCategory) -> &'static [&'static str] {
    match category {
        SituationCategory::Configuration => &["tripwire.situation.configuration_alternative"],
        SituationCategory::Deployment => &["tripwire.situation.deployment_alternative"],
        _ => &[],
    }
}

fn add_procedure_candidates(
    mut route: RoutingDecision,
    category: SituationCategory,
) -> RoutingDecision {
    for candidate in procedure_candidates_for(category) {
        if !route
            .procedure_candidate_ids
            .iter()
            .any(|existing| existing == candidate)
        {
            route = route.with_procedure_candidate(*candidate);
        }
    }
    route
}

fn add_fixture(mut route: RoutingDecision, fixture: &str) -> RoutingDecision {
    if !route.fixture_ids.iter().any(|existing| existing == fixture) {
        route = route.with_fixture(fixture);
    }
    route
}

fn add_tripwire_candidates(
    mut route: RoutingDecision,
    category: SituationCategory,
) -> RoutingDecision {
    for candidate in tripwire_candidates_for(category) {
        if !route
            .tripwire_candidate_ids
            .iter()
            .any(|existing| existing == candidate)
        {
            route = route.with_tripwire_candidate(*candidate);
        }
    }
    route
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
        score -= 0.10;
    }
    score.clamp(0.0, 1.0)
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

/// Show details for a situation (stub - would look up from storage).
#[must_use]
pub fn show_situation(situation_id: &str) -> Option<SituationDetails> {
    let version = build_info().version;

    // Stub implementation - in real version would look up from database
    Some(SituationDetails {
        version,
        situation_id: situation_id.to_string(),
        category: SituationCategory::Unknown,
        original_text: "[stored situation text]".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        context_hints: vec![
            "Check related memories for similar tasks".to_string(),
            "Review procedural rules for this category".to_string(),
        ],
        related_memories: vec![],
    })
}

/// Explain a situation (stub - would analyze from storage and rules).
#[must_use]
pub fn explain_situation(situation_id: &str) -> Option<SituationExplanation> {
    let version = build_info().version;

    // Stub implementation - in real version would analyze from database
    Some(SituationExplanation {
        version,
        situation_id: situation_id.to_string(),
        category: SituationCategory::Unknown,
        explanation: "This situation has been classified but no detailed analysis is available yet. \
            The explain command will provide richer context once memory and rule retrieval is wired.".to_string(),
        recommendations: vec![
            "Use `ee context` to retrieve relevant memories".to_string(),
            "Check `ee search` for similar past situations".to_string(),
        ],
        relevant_rules: vec![],
        potential_risks: vec![],
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;
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

    fn route_for(
        result: &ClassifyResult,
        surface: SituationRoutingSurface,
    ) -> Result<&RoutingDecision, String> {
        result
            .routing_decisions
            .iter()
            .find(|decision| decision.surface == surface)
            .ok_or_else(|| format!("missing route for {}", surface.as_str()))
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
            result.routing_decisions.len(),
            6,
            "unknown includes manual-review route",
        )?;
        let replay = route_for(&result, SituationRoutingSurface::CounterfactualReplay)?;
        ensure(
            replay.replay_policy,
            SituationReplayPolicy::NotEligible,
            "low confidence replay",
        )
    }

    #[test]
    fn classify_result_json_has_required_fields() -> TestResult {
        let result = classify_task("fix bug in login");
        let json = result.data_json();

        ensure(json.get("command").is_some(), true, "has command")?;
        ensure(json.get("category").is_some(), true, "has category")?;
        ensure(json.get("confidence").is_some(), true, "has confidence")?;
        ensure(json.get("signals").is_some(), true, "has signals")?;
        ensure(
            json.get("routingDecisions").is_some(),
            true,
            "has routing decisions",
        )
    }

    #[test]
    fn classify_task_routes_downstream_surfaces() -> TestResult {
        let result = classify_task("fix failing release workflow");

        let context = route_for(&result, SituationRoutingSurface::ContextProfile)?;
        ensure(
            context.selected_profile.as_deref(),
            Some("thorough"),
            "bug-fix context profile",
        )?;
        ensure(
            context.retrieval_profile.as_deref(),
            Some("thorough"),
            "bug-fix retrieval profile",
        )?;

        let preflight = route_for(&result, SituationRoutingSurface::PreflightProfile)?;
        ensure(
            preflight.preflight_profile.as_deref(),
            Some("standard"),
            "bug-fix preflight profile",
        )?;

        let procedure = route_for(&result, SituationRoutingSurface::ProcedureCandidate)?;
        ensure(
            procedure
                .procedure_candidate_ids
                .first()
                .map(String::as_str),
            Some("procedure.debug.reproduce_failure"),
            "bug-fix procedure candidate",
        )?;

        let fixtures = route_for(&result, SituationRoutingSurface::FixtureFamily)?;
        ensure(
            fixtures.fixture_ids.clone(),
            vec![
                "fixture.situation.bug_fix".to_string(),
                "fixture.preflight.standard".to_string(),
            ],
            "bug-fix fixture family",
        )?;

        let replay = route_for(&result, SituationRoutingSurface::CounterfactualReplay)?;
        ensure(
            replay.replay_policy,
            SituationReplayPolicy::Allowed,
            "bug-fix replay policy",
        )?;
        ensure(
            result
                .routing_decisions
                .iter()
                .any(|decision| decision.surface == SituationRoutingSurface::ManualReview),
            false,
            "medium confidence avoids manual review",
        )
    }

    #[test]
    fn low_confidence_classification_broadens_routing() -> TestResult {
        let result = classify_task("docs fix");
        ensure(
            result.category,
            SituationCategory::Documentation,
            "top category",
        )?;
        ensure(result.confidence, ConfidenceLevel::Low, "low confidence")?;

        let context = route_for(&result, SituationRoutingSurface::ContextProfile)?;
        ensure(
            context.retrieval_profile.as_deref(),
            Some(ContextProfileName::Thorough.as_str()),
            "low-confidence retrieval profile",
        )?;
        ensure(
            ContextProfileName::parse(context.retrieval_profile.as_deref().unwrap_or_default())
                .is_some(),
            true,
            "routing profile is accepted by ee context",
        )?;
        ensure(
            context
                .reasons
                .iter()
                .any(|reason| reason.contains("preserve alternatives")),
            true,
            "broadening reason",
        )?;

        let procedure = route_for(&result, SituationRoutingSurface::ProcedureCandidate)?;
        ensure(
            procedure
                .procedure_candidate_ids
                .iter()
                .any(|candidate| candidate == "procedure.debug.reproduce_failure"),
            true,
            "alternative procedure preserved",
        )?;

        let fixtures = route_for(&result, SituationRoutingSurface::FixtureFamily)?;
        ensure(
            fixtures
                .fixture_ids
                .iter()
                .any(|fixture| fixture == "fixture.situation.bug_fix"),
            true,
            "alternative fixture preserved",
        )?;
        ensure(
            result
                .routing_decisions
                .iter()
                .any(|decision| decision.surface == SituationRoutingSurface::ManualReview),
            true,
            "low confidence still asks for review",
        )
    }

    #[test]
    fn high_risk_alternative_adds_tripwire_without_changing_top_category() -> TestResult {
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
                .any(|(category, _)| *category == SituationCategory::Deployment),
            true,
            "deployment retained as alternative",
        )?;

        let tripwire = route_for(&result, SituationRoutingSurface::TripwireCandidate)?;
        ensure(
            tripwire.tripwire_candidate_ids.clone(),
            vec!["tripwire.situation.deployment_alternative".to_string()],
            "high-risk alternative tripwire",
        )?;
        ensure(
            tripwire
                .reasons
                .iter()
                .any(|reason| reason.contains("remains the top classification")),
            true,
            "tripwire reason",
        )
    }

    #[test]
    fn low_confidence_broadening_json_matches_golden() -> TestResult {
        let actual = classification_envelope(&classify_task("docs fix"));
        let expected: serde_json::Value =
            serde_json::from_str(LOW_CONFIDENCE_BROADENING_GOLDEN).map_err(|e| e.to_string())?;

        ensure(actual, expected, "low confidence broadening golden")
    }

    #[test]
    fn high_risk_alternative_json_matches_golden() -> TestResult {
        let actual = classification_envelope(&classify_task("fix failing release workflow"));
        let expected: serde_json::Value =
            serde_json::from_str(HIGH_RISK_ALTERNATIVE_GOLDEN).map_err(|e| e.to_string())?;

        ensure(actual, expected, "high risk alternative golden")
    }

    #[test]
    fn compare_situations_recommends_shared_bug_fix_link() -> TestResult {
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
        ensure(report.confidence, ConfidenceLevel::Medium, "confidence")?;
        ensure(report.recommended, true, "recommended")?;
        ensure(
            report.overlap.signal_patterns,
            vec!["fix".to_string()],
            "shared signal",
        )?;
        ensure(
            report
                .overlap
                .routing_targets
                .iter()
                .any(|target| target == "context_profile:thorough"),
            true,
            "shared context route",
        )
    }

    #[test]
    fn situation_link_dry_run_creates_curation_backed_plan_without_mutation() -> TestResult {
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
        let link = report
            .planned_link
            .as_ref()
            .ok_or_else(|| "expected planned link".to_string())?;
        ensure(link.schema, SITUATION_LINK_SCHEMA_V1, "link schema")?;
        ensure(
            link.source_situation_id.as_str(),
            "sit.release_bug",
            "source id",
        )?;
        ensure(
            link.target_situation_id.as_str(),
            "sit.login_bug",
            "target id",
        )?;
        ensure(link.created_at.as_str(), "2026-05-01T00:00:00Z", "time")?;
        ensure(
            link.evidence_ids.clone(),
            vec!["feat.shared.fix".to_string()],
            "evidence ids",
        )?;
        let json = report.data_json();
        ensure(
            json.get("plannedLink")
                .and_then(|value| value.get("relation"))
                .and_then(serde_json::Value::as_str),
            Some("similar"),
            "planned relation",
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
        let actual = evaluate_built_in_situation_fixtures().data_json();
        let expected: serde_json::Value =
            serde_json::from_str(SITUATION_FIXTURE_METRICS_GOLDEN).map_err(|e| e.to_string())?;

        ensure(actual, expected, "situation fixture metrics golden")
    }
}
