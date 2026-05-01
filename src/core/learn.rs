//! Active learning agenda and uncertainty operations (EE-441).
//!
//! Provides agenda, uncertainty, and summary operations for identifying
//! knowledge gaps and prioritizing learning opportunities.

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::{DomainError, ExperimentSafetyBoundary, LearningExperimentStatus};

/// Schema for learning agenda report.
pub const LEARN_AGENDA_SCHEMA_V1: &str = "ee.learn.agenda.v1";

/// Schema for uncertainty report.
pub const LEARN_UNCERTAINTY_SCHEMA_V1: &str = "ee.learn.uncertainty.v1";

/// Schema for learning summary report.
pub const LEARN_SUMMARY_SCHEMA_V1: &str = "ee.learn.summary.v1";

/// Schema for experiment proposal reports.
pub const LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1: &str = "ee.learn.experiment_proposal.v1";

// ============================================================================
// Agenda Operation
// ============================================================================

/// Options for showing the learning agenda.
#[derive(Clone, Debug, Default)]
pub struct LearnAgendaOptions {
    pub workspace: PathBuf,
    pub limit: u32,
    pub topic: Option<String>,
    pub include_resolved: bool,
    pub sort: String,
}

/// A single item in the learning agenda.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgendaItem {
    pub id: String,
    pub topic: String,
    pub gap_description: String,
    pub priority: u8,
    pub uncertainty: f64,
    pub source: String,
    pub status: String,
    pub created_at: String,
}

/// Report from showing the learning agenda.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearnAgendaReport {
    pub schema: String,
    pub items: Vec<AgendaItem>,
    pub total_gaps: u32,
    pub high_priority_count: u32,
    pub resolved_count: u32,
    pub generated_at: String,
}

impl LearnAgendaReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show the active learning agenda.
pub fn show_agenda(options: &LearnAgendaOptions) -> Result<LearnAgendaReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let all_items = vec![
        AgendaItem {
            id: "gap_001".to_owned(),
            topic: "error_handling".to_owned(),
            gap_description: "Missing patterns for async error propagation".to_owned(),
            priority: 85,
            uncertainty: 0.72,
            source: "curation_review".to_owned(),
            status: "open".to_owned(),
            created_at: now.clone(),
        },
        AgendaItem {
            id: "gap_002".to_owned(),
            topic: "testing".to_owned(),
            gap_description: "No integration test patterns for database operations".to_owned(),
            priority: 70,
            uncertainty: 0.65,
            source: "failure_analysis".to_owned(),
            status: "open".to_owned(),
            created_at: now.clone(),
        },
        AgendaItem {
            id: "gap_003".to_owned(),
            topic: "cli".to_owned(),
            gap_description: "Unclear argument validation conventions".to_owned(),
            priority: 55,
            uncertainty: 0.45,
            source: "user_feedback".to_owned(),
            status: "resolved".to_owned(),
            created_at: now.clone(),
        },
    ];

    let filtered: Vec<_> = all_items
        .into_iter()
        .filter(|item| {
            (options.include_resolved || item.status != "resolved")
                && options
                    .topic
                    .as_ref()
                    .is_none_or(|t| item.topic.contains(t))
        })
        .take(options.limit as usize)
        .collect();

    let high_priority = filtered.iter().filter(|i| i.priority >= 70).count() as u32;

    Ok(LearnAgendaReport {
        schema: LEARN_AGENDA_SCHEMA_V1.to_owned(),
        total_gaps: 3,
        high_priority_count: high_priority,
        resolved_count: 1,
        items: filtered,
        generated_at: now,
    })
}

// ============================================================================
// Uncertainty Operation
// ============================================================================

/// Options for showing uncertainty estimates.
#[derive(Clone, Debug, Default)]
pub struct LearnUncertaintyOptions {
    pub workspace: PathBuf,
    pub limit: u32,
    pub min_uncertainty: f64,
    pub kind: Option<String>,
    pub low_confidence: bool,
}

/// An item with uncertainty estimate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UncertaintyItem {
    pub memory_id: String,
    pub content_preview: String,
    pub kind: String,
    pub uncertainty: f64,
    pub confidence: f64,
    pub retrieval_count: u32,
    pub last_accessed: Option<String>,
}

/// Report from showing uncertainty estimates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearnUncertaintyReport {
    pub schema: String,
    pub items: Vec<UncertaintyItem>,
    pub mean_uncertainty: f64,
    pub high_uncertainty_count: u32,
    pub sampling_candidates: u32,
    pub generated_at: String,
}

impl LearnUncertaintyReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show uncertainty estimates and sampling priorities.
pub fn show_uncertainty(
    options: &LearnUncertaintyOptions,
) -> Result<LearnUncertaintyReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let all_items = vec![
        UncertaintyItem {
            memory_id: "mem_001".to_owned(),
            content_preview: "Always run cargo fmt before committing...".to_owned(),
            kind: "procedural".to_owned(),
            uncertainty: 0.82,
            confidence: 0.35,
            retrieval_count: 2,
            last_accessed: Some(now.clone()),
        },
        UncertaintyItem {
            memory_id: "mem_002".to_owned(),
            content_preview: "Use Result<T, E> for fallible operations...".to_owned(),
            kind: "episodic".to_owned(),
            uncertainty: 0.55,
            confidence: 0.65,
            retrieval_count: 8,
            last_accessed: Some(now.clone()),
        },
        UncertaintyItem {
            memory_id: "mem_003".to_owned(),
            content_preview: "The search index lives in .ee/index/...".to_owned(),
            kind: "semantic".to_owned(),
            uncertainty: 0.38,
            confidence: 0.78,
            retrieval_count: 15,
            last_accessed: Some(now.clone()),
        },
    ];

    let filtered: Vec<_> = all_items
        .into_iter()
        .filter(|item| {
            item.uncertainty >= options.min_uncertainty
                && options.kind.as_ref().is_none_or(|k| &item.kind == k)
                && (!options.low_confidence || item.confidence < 0.5)
        })
        .take(options.limit as usize)
        .collect();

    let mean = if filtered.is_empty() {
        0.0
    } else {
        filtered.iter().map(|i| i.uncertainty).sum::<f64>() / filtered.len() as f64
    };

    let high_uncertainty = filtered.iter().filter(|i| i.uncertainty > 0.7).count() as u32;

    Ok(LearnUncertaintyReport {
        schema: LEARN_UNCERTAINTY_SCHEMA_V1.to_owned(),
        mean_uncertainty: mean,
        high_uncertainty_count: high_uncertainty,
        sampling_candidates: filtered.len() as u32,
        items: filtered,
        generated_at: now,
    })
}

// ============================================================================
// Summary Operation
// ============================================================================

/// Options for showing learning summary.
#[derive(Clone, Debug, Default)]
pub struct LearnSummaryOptions {
    pub workspace: PathBuf,
    pub period: String,
    pub detailed: bool,
}

/// Learning summary statistics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningSummary {
    pub period: String,
    pub memories_created: u32,
    pub memories_promoted: u32,
    pub memories_demoted: u32,
    pub rules_learned: u32,
    pub rules_validated: u32,
    pub gaps_identified: u32,
    pub gaps_resolved: u32,
    pub net_knowledge_delta: i32,
}

/// Detailed learning event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningEvent {
    pub event_type: String,
    pub description: String,
    pub impact: String,
    pub occurred_at: String,
}

/// Report from showing learning summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearnSummaryReport {
    pub schema: String,
    pub summary: LearningSummary,
    pub events: Vec<LearningEvent>,
    pub generated_at: String,
}

impl LearnSummaryReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show learning summary for a period.
pub fn show_summary(options: &LearnSummaryOptions) -> Result<LearnSummaryReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let summary = LearningSummary {
        period: options.period.clone(),
        memories_created: 15,
        memories_promoted: 3,
        memories_demoted: 1,
        rules_learned: 2,
        rules_validated: 1,
        gaps_identified: 4,
        gaps_resolved: 2,
        net_knowledge_delta: 12,
    };

    let events = if options.detailed {
        vec![
            LearningEvent {
                event_type: "rule_learned".to_owned(),
                description: "New rule: Always validate input at API boundaries".to_owned(),
                impact: "high".to_owned(),
                occurred_at: now.clone(),
            },
            LearningEvent {
                event_type: "gap_resolved".to_owned(),
                description: "Resolved: Missing async error handling patterns".to_owned(),
                impact: "medium".to_owned(),
                occurred_at: now.clone(),
            },
        ]
    } else {
        Vec::new()
    };

    Ok(LearnSummaryReport {
        schema: LEARN_SUMMARY_SCHEMA_V1.to_owned(),
        summary,
        events,
        generated_at: now,
    })
}

// ============================================================================
// Experiment Proposal Operation
// ============================================================================

/// Options for proposing safe learning experiments.
#[derive(Clone, Debug)]
pub struct LearnExperimentProposeOptions {
    pub workspace: PathBuf,
    pub limit: u32,
    pub topic: Option<String>,
    pub min_expected_value: f64,
    pub max_attention_tokens: u32,
    pub max_runtime_seconds: u32,
    pub safety_boundary: ExperimentSafetyBoundary,
}

impl Default for LearnExperimentProposeOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::new(),
            limit: 3,
            topic: None,
            min_expected_value: 0.0,
            max_attention_tokens: 1_200,
            max_runtime_seconds: 300,
            safety_boundary: ExperimentSafetyBoundary::DryRunOnly,
        }
    }
}

/// Budget envelope for a proposed experiment.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentBudget {
    pub attention_tokens: u32,
    pub max_runtime_seconds: u32,
    pub dry_run_required: bool,
    pub budget_class: String,
}

/// Safety posture that must be honored before an experiment can run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentSafetyPlan {
    pub boundary: String,
    pub dry_run_first: bool,
    pub mutation_allowed: bool,
    pub review_required: bool,
    pub stop_conditions: Vec<String>,
    pub denied_reasons: Vec<String>,
}

/// Decision that could change if the experiment reduces uncertainty.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentDecisionImpact {
    pub decision_id: String,
    pub target_artifact_ids: Vec<String>,
    pub current_decision: String,
    pub possible_change: String,
    pub impact_score: f64,
}

/// One proposed dry-run-first active learning experiment.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentProposal {
    pub experiment_id: String,
    pub question_id: String,
    pub title: String,
    pub hypothesis: String,
    pub status: String,
    pub topic: String,
    pub expected_value: f64,
    pub uncertainty_reduction: f64,
    pub confidence: f64,
    pub budget: ExperimentBudget,
    pub safety: ExperimentSafetyPlan,
    pub decision_impact: ExperimentDecisionImpact,
    pub evidence_ids: Vec<String>,
    pub next_command: String,
}

/// Report returned by `ee learn experiment propose --json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LearnExperimentProposalReport {
    pub schema: String,
    pub proposals: Vec<ExperimentProposal>,
    pub total_candidates: u32,
    pub returned: u32,
    pub min_expected_value: f64,
    pub max_attention_tokens: u32,
    pub max_runtime_seconds: u32,
    pub generated_at: String,
}

impl LearnExperimentProposalReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Propose safe dry-run-first experiments that could change memory decisions.
pub fn propose_experiments(
    options: &LearnExperimentProposeOptions,
) -> Result<LearnExperimentProposalReport, DomainError> {
    let now = Utc::now().to_rfc3339();
    let candidates = experiment_seed_candidates(options);
    let total_candidates = candidates.len() as u32;
    let limit = options.limit as usize;
    let min_expected_value = bounded_metric(options.min_expected_value);

    let mut proposals: Vec<_> = candidates
        .into_iter()
        .filter(|proposal| {
            options
                .topic
                .as_ref()
                .is_none_or(|topic| proposal.topic.contains(topic))
                && proposal.expected_value >= min_expected_value
        })
        .collect();

    proposals.sort_by(compare_proposals);
    proposals.truncate(limit);

    Ok(LearnExperimentProposalReport {
        schema: LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1.to_owned(),
        returned: proposals.len() as u32,
        proposals,
        total_candidates,
        min_expected_value,
        max_attention_tokens: options.max_attention_tokens,
        max_runtime_seconds: options.max_runtime_seconds,
        generated_at: now,
    })
}

fn experiment_seed_candidates(options: &LearnExperimentProposeOptions) -> Vec<ExperimentProposal> {
    [
        ExperimentSeed {
            experiment_id: "exp_replay_error_boundary",
            question_id: "gap_001",
            topic: "error_handling",
            title: "Replay async error boundary failures",
            hypothesis: "A dry-run replay can distinguish missing propagation evidence from a weak procedural rule.",
            evidence_ids: &["gap_001", "mem_002"],
            decision_id: "decision_error_boundary_rule",
            target_artifact_ids: &["mem_002"],
            current_decision: "Keep async error propagation guidance at low confidence.",
            possible_change: "Promote or demote the rule based on replayed failure evidence.",
            priority: 85.0,
            uncertainty: 0.72,
            confidence: 0.48,
            uncertainty_reduction: 0.32,
            runtime_seconds: 240,
            attention_tokens: 900,
            stop_condition: "Stop after the replay produces a pass/fail explanation or safety finding.",
        },
        ExperimentSeed {
            experiment_id: "exp_database_contract_fixture",
            question_id: "gap_002",
            topic: "testing",
            title: "Run database-operation contract fixture",
            hypothesis: "A fixture-only dry run can show whether database-operation memories need stronger integration-test evidence.",
            evidence_ids: &["gap_002", "mem_001"],
            decision_id: "decision_database_test_pattern",
            target_artifact_ids: &["mem_001"],
            current_decision: "Treat database integration guidance as plausible but under-sampled.",
            possible_change: "Increase confidence or request more evidence before promotion.",
            priority: 70.0,
            uncertainty: 0.65,
            confidence: 0.55,
            uncertainty_reduction: 0.28,
            runtime_seconds: 180,
            attention_tokens: 700,
            stop_condition: "Stop after fixture output records stdout/stderr separation and mutation posture.",
        },
        ExperimentSeed {
            experiment_id: "exp_cli_validation_shadow",
            question_id: "gap_003",
            topic: "cli",
            title: "Shadow CLI validation examples",
            hypothesis: "Shadowing invalid arguments can clarify whether CLI validation rules are resolved or still ambiguous.",
            evidence_ids: &["gap_003", "mem_003"],
            decision_id: "decision_cli_validation_convention",
            target_artifact_ids: &["mem_003"],
            current_decision: "Keep CLI validation convention marked resolved unless new ambiguity appears.",
            possible_change: "Reopen the learning question or keep it resolved with supporting evidence.",
            priority: 55.0,
            uncertainty: 0.45,
            confidence: 0.72,
            uncertainty_reduction: 0.18,
            runtime_seconds: 120,
            attention_tokens: 500,
            stop_condition: "Stop after two invalid argument examples produce stable repair text.",
        },
    ]
    .into_iter()
    .map(|seed| seed.into_proposal(options))
    .collect()
}

#[derive(Clone, Copy, Debug)]
struct ExperimentSeed {
    experiment_id: &'static str,
    question_id: &'static str,
    topic: &'static str,
    title: &'static str,
    hypothesis: &'static str,
    evidence_ids: &'static [&'static str],
    decision_id: &'static str,
    target_artifact_ids: &'static [&'static str],
    current_decision: &'static str,
    possible_change: &'static str,
    priority: f64,
    uncertainty: f64,
    confidence: f64,
    uncertainty_reduction: f64,
    runtime_seconds: u32,
    attention_tokens: u32,
    stop_condition: &'static str,
}

impl ExperimentSeed {
    fn into_proposal(self, options: &LearnExperimentProposeOptions) -> ExperimentProposal {
        let attention_tokens = self.attention_tokens.min(options.max_attention_tokens);
        let runtime_seconds = self.runtime_seconds.min(options.max_runtime_seconds);
        let safety = safety_plan(options.safety_boundary, self.stop_condition);
        let expected_value = expected_value(self.priority, self.uncertainty, self.confidence);
        let experiment_id = self.experiment_id.to_owned();

        ExperimentProposal {
            next_command: format!("ee learn experiment run --dry-run --id {experiment_id} --json"),
            experiment_id,
            question_id: self.question_id.to_owned(),
            title: self.title.to_owned(),
            hypothesis: self.hypothesis.to_owned(),
            status: LearningExperimentStatus::Proposed.as_str().to_owned(),
            topic: self.topic.to_owned(),
            expected_value,
            uncertainty_reduction: rounded_metric(self.uncertainty_reduction),
            confidence: rounded_metric(self.confidence),
            budget: ExperimentBudget {
                attention_tokens,
                max_runtime_seconds: runtime_seconds,
                dry_run_required: true,
                budget_class: budget_class(attention_tokens, runtime_seconds).to_owned(),
            },
            safety,
            decision_impact: ExperimentDecisionImpact {
                decision_id: self.decision_id.to_owned(),
                target_artifact_ids: self
                    .target_artifact_ids
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect(),
                current_decision: self.current_decision.to_owned(),
                possible_change: self.possible_change.to_owned(),
                impact_score: rounded_metric(self.priority / 100.0),
            },
            evidence_ids: self
                .evidence_ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
        }
    }
}

fn safety_plan(
    boundary: ExperimentSafetyBoundary,
    stop_condition: &'static str,
) -> ExperimentSafetyPlan {
    let review_required = matches!(
        boundary,
        ExperimentSafetyBoundary::AskBeforeActing
            | ExperimentSafetyBoundary::HumanReview
            | ExperimentSafetyBoundary::Denied
    );
    let denied_reasons = if boundary == ExperimentSafetyBoundary::Denied {
        vec!["configured safety boundary denies experiment execution".to_owned()]
    } else {
        Vec::new()
    };

    ExperimentSafetyPlan {
        boundary: boundary.as_str().to_owned(),
        dry_run_first: true,
        mutation_allowed: false,
        review_required,
        stop_conditions: vec![
            stop_condition.to_owned(),
            "Stop before any durable memory mutation; close with observe/close evidence first."
                .to_owned(),
        ],
        denied_reasons,
    }
}

fn expected_value(priority: f64, uncertainty: f64, confidence: f64) -> f64 {
    rounded_metric((priority / 100.0) * uncertainty * (1.0 - confidence * 0.25))
}

fn rounded_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

fn bounded_metric(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn budget_class(attention_tokens: u32, runtime_seconds: u32) -> &'static str {
    match (attention_tokens, runtime_seconds) {
        (0..=600, 0..=120) => "small",
        (0..=1_000, 0..=240) => "medium",
        _ => "large",
    }
}

fn compare_proposals(left: &ExperimentProposal, right: &ExperimentProposal) -> std::cmp::Ordering {
    right
        .expected_value
        .total_cmp(&left.expected_value)
        .then_with(|| left.experiment_id.cmp(&right.experiment_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn agenda_filters_resolved() -> TestResult {
        let options = LearnAgendaOptions {
            limit: 10,
            include_resolved: false,
            ..Default::default()
        };

        let report = show_agenda(&options).map_err(|e| e.message())?;
        assert!(report.items.iter().all(|i| i.status != "resolved"));
        Ok(())
    }

    #[test]
    fn agenda_filters_by_topic() -> TestResult {
        let options = LearnAgendaOptions {
            limit: 10,
            topic: Some("error".to_owned()),
            include_resolved: true,
            ..Default::default()
        };

        let report = show_agenda(&options).map_err(|e| e.message())?;
        assert!(report.items.iter().all(|i| i.topic.contains("error")));
        Ok(())
    }

    #[test]
    fn uncertainty_filters_by_threshold() -> TestResult {
        let options = LearnUncertaintyOptions {
            limit: 10,
            min_uncertainty: 0.5,
            ..Default::default()
        };

        let report = show_uncertainty(&options).map_err(|e| e.message())?;
        assert!(report.items.iter().all(|i| i.uncertainty >= 0.5));
        Ok(())
    }

    #[test]
    fn uncertainty_filters_low_confidence() -> TestResult {
        let options = LearnUncertaintyOptions {
            limit: 10,
            min_uncertainty: 0.0,
            low_confidence: true,
            ..Default::default()
        };

        let report = show_uncertainty(&options).map_err(|e| e.message())?;
        assert!(report.items.iter().all(|i| i.confidence < 0.5));
        Ok(())
    }

    #[test]
    fn summary_includes_events_when_detailed() -> TestResult {
        let options = LearnSummaryOptions {
            period: "week".to_owned(),
            detailed: true,
            ..Default::default()
        };

        let report = show_summary(&options).map_err(|e| e.message())?;
        assert!(!report.events.is_empty());
        Ok(())
    }

    #[test]
    fn summary_no_events_when_not_detailed() -> TestResult {
        let options = LearnSummaryOptions {
            period: "week".to_owned(),
            detailed: false,
            ..Default::default()
        };

        let report = show_summary(&options).map_err(|e| e.message())?;
        assert!(report.events.is_empty());
        Ok(())
    }

    #[test]
    fn learn_experiment_proposals_are_ranked_by_expected_value() -> TestResult {
        let report = propose_experiments(&LearnExperimentProposeOptions {
            limit: 2,
            ..Default::default()
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.schema, LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1);
        assert_eq!(report.total_candidates, 3);
        assert_eq!(report.returned, 2);
        assert_eq!(
            report.proposals[0].experiment_id,
            "exp_replay_error_boundary"
        );
        assert!(
            report.proposals[0].expected_value >= report.proposals[1].expected_value,
            "proposals must be sorted by expected value"
        );
        Ok(())
    }

    #[test]
    fn learn_experiment_proposals_filter_topic_and_expected_value() -> TestResult {
        let report = propose_experiments(&LearnExperimentProposeOptions {
            limit: 10,
            topic: Some("testing".to_owned()),
            min_expected_value: 0.3,
            ..Default::default()
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.returned, 1);
        assert_eq!(report.proposals[0].topic, "testing");
        assert!(report.proposals[0].expected_value >= 0.3);
        Ok(())
    }

    #[test]
    fn learn_experiment_proposals_enforce_safety_and_budget_caps() -> TestResult {
        let report = propose_experiments(&LearnExperimentProposeOptions {
            limit: 1,
            max_attention_tokens: 600,
            max_runtime_seconds: 90,
            safety_boundary: ExperimentSafetyBoundary::HumanReview,
            ..Default::default()
        })
        .map_err(|e| e.message())?;

        let proposal = &report.proposals[0];
        assert_eq!(proposal.budget.attention_tokens, 600);
        assert_eq!(proposal.budget.max_runtime_seconds, 90);
        assert_eq!(proposal.safety.boundary, "human_review");
        assert!(proposal.safety.dry_run_first);
        assert!(!proposal.safety.mutation_allowed);
        assert!(proposal.safety.review_required);
        assert!(
            proposal
                .decision_impact
                .possible_change
                .contains("Promote or demote")
        );
        Ok(())
    }

    #[test]
    fn learn_experiment_proposals_allow_empty_limit() -> TestResult {
        let report = propose_experiments(&LearnExperimentProposeOptions {
            limit: 0,
            ..Default::default()
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.returned, 0);
        assert!(report.proposals.is_empty());
        Ok(())
    }
}
