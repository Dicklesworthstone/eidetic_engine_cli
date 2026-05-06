//! Active learning agenda and uncertainty operations (EE-441).
//!
//! Provides agenda, uncertainty, and summary operations for identifying
//! knowledge gaps and prioritizing learning opportunities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::outcome::{OutcomeRecordOptions, OutcomeRecordReport, record_outcome};
use crate::db::{
    CreateCurationCandidateInput, CreateLearningObservationInput, CreateWorkspaceInput,
    DbConnection, StoredCurationCandidate, StoredFeedbackEvent, StoredLearningObservation,
    StoredMemory,
};
use crate::models::{
    DomainError, ExperimentOutcome, ExperimentOutcomeStatus, ExperimentSafetyBoundary,
    LearningObservation, LearningObservationSignal, LearningTargetKind, WorkspaceId,
};

/// Schema for learning agenda report.
pub const LEARN_AGENDA_SCHEMA_V1: &str = "ee.learn.agenda.v1";

/// Schema for uncertainty report.
pub const LEARN_UNCERTAINTY_SCHEMA_V1: &str = "ee.learn.uncertainty.v1";

/// Schema for learning summary report.
pub const LEARN_SUMMARY_SCHEMA_V1: &str = "ee.learn.summary.v1";

/// Schema for experiment proposal reports.
pub const LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1: &str = "ee.learn.experiment_proposal.v1";

/// Schema for learning experiment dry-run reports.
pub const LEARN_EXPERIMENT_RUN_SCHEMA_V1: &str = "ee.learn.experiment_run.v1";

/// Schema for learning observation reports.
pub const LEARN_OBSERVE_SCHEMA_V1: &str = "ee.learn.observe.v1";

/// Schema for learning experiment closure reports.
pub const LEARN_CLOSE_SCHEMA_V1: &str = "ee.learn.close.v1";

/// Schema for downstream learning feedback projections.
pub const LEARN_DOWNSTREAM_EFFECTS_SCHEMA_V1: &str = "ee.learn.downstream_effects.v1";

const EXPERIMENT_REGISTRY_UNAVAILABLE_MESSAGE: &str = "Experiment execution requires persisted experiment definitions from an evaluation registry. Hard-coded experiment templates have been removed to preserve deterministic explainable retrieval.";
const EXPERIMENT_REGISTRY_UNAVAILABLE_REPAIR: &str =
    "Provide explicit input datasets or use skill workflows for experiment orchestration.";

fn experiment_registry_unavailable() -> DomainError {
    DomainError::UnsatisfiedDegradedMode {
        message: EXPERIMENT_REGISTRY_UNAVAILABLE_MESSAGE.to_string(),
        repair: Some(EXPERIMENT_REGISTRY_UNAVAILABLE_REPAIR.to_string()),
    }
}

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
    pub sample_ids: Vec<String>,
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
    let clusters = load_learning_clusters(&options.workspace, options.topic.as_deref())?;
    let mut items = clusters
        .iter()
        .filter(|cluster| options.include_resolved || cluster.status() != "resolved")
        .map(|cluster| cluster.agenda_item())
        .collect::<Vec<_>>();

    match options.sort.as_str() {
        "uncertainty" => items.sort_by(|left, right| {
            right
                .uncertainty
                .total_cmp(&left.uncertainty)
                .then_with(|| right.priority.cmp(&left.priority))
                .then_with(|| left.topic.cmp(&right.topic))
                .then_with(|| left.id.cmp(&right.id))
        }),
        "recency" => items.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.priority.cmp(&left.priority))
                .then_with(|| left.topic.cmp(&right.topic))
                .then_with(|| left.id.cmp(&right.id))
        }),
        _ => items.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| right.uncertainty.total_cmp(&left.uncertainty))
                .then_with(|| left.topic.cmp(&right.topic))
                .then_with(|| left.id.cmp(&right.id))
        }),
    }

    let total_gaps = items.len() as u32;
    let high_priority_count = items.iter().filter(|item| item.priority >= 70).count() as u32;
    let resolved_count = items
        .iter()
        .filter(|item| item.status == "resolved")
        .count() as u32;
    items.truncate(options.limit as usize);

    Ok(LearnAgendaReport {
        schema: LEARN_AGENDA_SCHEMA_V1.to_string(),
        items,
        total_gaps,
        high_priority_count,
        resolved_count,
        generated_at: stable_learning_generated_at(),
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
    let clusters = load_learning_clusters(&options.workspace, options.kind.as_deref())?;
    let mut items = clusters
        .iter()
        .map(|cluster| cluster.uncertainty_item())
        .filter(|item| item.uncertainty >= options.min_uncertainty)
        .filter(|item| !options.low_confidence || item.confidence < 0.5)
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .uncertainty
            .total_cmp(&left.uncertainty)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    let mean_uncertainty = if items.is_empty() {
        0.0
    } else {
        rounded_metric(items.iter().map(|item| item.uncertainty).sum::<f64>() / items.len() as f64)
    };
    let high_uncertainty_count = items.iter().filter(|item| item.uncertainty >= 0.7).count() as u32;
    let sampling_candidates = items.len() as u32;
    items.truncate(options.limit as usize);

    Ok(LearnUncertaintyReport {
        schema: LEARN_UNCERTAINTY_SCHEMA_V1.to_string(),
        items,
        mean_uncertainty,
        high_uncertainty_count,
        sampling_candidates,
        generated_at: stable_learning_generated_at(),
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
    pub since: Option<String>,
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
    pub observations_recorded: u32,
    pub candidates_proposed: u32,
    pub applied_rules: u32,
    pub harmful_feedback_count: u32,
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
    let snapshot = load_learning_snapshot(&options.workspace)?;
    let since = options.since.as_deref();
    let events = snapshot
        .feedback_events
        .iter()
        .filter(|event| since.is_none_or(|since| event.created_at.as_str() >= since))
        .cloned()
        .collect::<Vec<_>>();
    let ledger_observation_count = snapshot
        .learning_observations
        .iter()
        .filter(|observation| since.is_none_or(|since| observation.observed_at.as_str() >= since))
        .count() as u32;
    let clusters = build_learning_clusters(&snapshot, None, &events);
    let harmful_feedback_count = events
        .iter()
        .filter(|event| is_negative_signal(&event.signal))
        .count() as u32;
    let memories_created = snapshot
        .memories
        .values()
        .filter(|memory| since.is_none_or(|since| memory.created_at.as_str() >= since))
        .count() as u32;
    let candidates = snapshot
        .curation_candidates
        .iter()
        .filter(|candidate| since.is_none_or(|since| candidate.created_at.as_str() >= since))
        .collect::<Vec<_>>();
    let candidates_proposed = candidates.len() as u32;
    let applied_rules = candidates
        .iter()
        .filter(|candidate| {
            candidate.candidate_type == "rule"
                && (candidate.status == "applied" || candidate.applied_at.is_some())
        })
        .count() as u32;
    let rules_validated = candidates
        .iter()
        .filter(|candidate| candidate.candidate_type == "rule" && candidate.status == "approved")
        .count() as u32;
    let rules_learned = candidates
        .iter()
        .filter(|candidate| candidate.candidate_type == "rule")
        .count() as u32;
    let gaps_identified = clusters.len() as u32;
    let gaps_resolved = clusters
        .iter()
        .filter(|cluster| cluster.status() == "resolved")
        .count() as u32;
    let memories_promoted = events
        .iter()
        .filter(|event| {
            matches!(
                event.signal.as_str(),
                "positive" | "helpful" | "confirmation"
            )
        })
        .count() as u32;
    let memories_demoted = harmful_feedback_count;
    let net_knowledge_delta = i32::try_from(memories_promoted + rules_learned + rules_validated)
        .unwrap_or(i32::MAX)
        - i32::try_from(memories_demoted).unwrap_or(i32::MAX);

    let mut learning_events = Vec::new();
    if options.detailed {
        for event in events.iter().rev().take(10) {
            learning_events.push(LearningEvent {
                event_type: event.signal.clone(),
                description: event
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("Observed {} feedback.", event.signal)),
                impact: feedback_impact(&event.signal).to_string(),
                occurred_at: event.created_at.clone(),
            });
        }
        learning_events.sort_by(|left, right| {
            right
                .occurred_at
                .cmp(&left.occurred_at)
                .then_with(|| left.event_type.cmp(&right.event_type))
        });
    }

    Ok(LearnSummaryReport {
        schema: LEARN_SUMMARY_SCHEMA_V1.to_string(),
        summary: LearningSummary {
            period: options
                .since
                .clone()
                .unwrap_or_else(|| options.period.clone()),
            memories_created,
            memories_promoted,
            memories_demoted,
            rules_learned,
            rules_validated,
            gaps_identified,
            gaps_resolved,
            observations_recorded: ledger_observation_count.max(events.len() as u32),
            candidates_proposed,
            applied_rules,
            harmful_feedback_count,
            net_knowledge_delta,
        },
        events: learning_events,
        generated_at: stable_learning_generated_at(),
    })
}

// ============================================================================
// Experiment Observation And Closure Operations
// ============================================================================

/// Options for attaching evidence observed during a learning experiment.
#[derive(Clone, Debug)]
pub struct LearnObserveOptions {
    pub workspace: PathBuf,
    pub database_path: Option<PathBuf>,
    pub workspace_id: Option<String>,
    pub experiment_id: String,
    pub observation_id: Option<String>,
    pub observed_at: Option<String>,
    pub observer: Option<String>,
    pub signal: LearningObservationSignal,
    pub measurement_name: String,
    pub measurement_value: Option<f64>,
    pub evidence_ids: Vec<String>,
    pub note: Option<String>,
    pub redaction_status: Option<String>,
    pub session_id: Option<String>,
    pub event_id: Option<String>,
    pub actor: Option<String>,
    pub dry_run: bool,
}

/// Options for closing a learning experiment with an auditable outcome.
#[derive(Clone, Debug)]
pub struct LearnCloseOptions {
    pub workspace: PathBuf,
    pub database_path: Option<PathBuf>,
    pub workspace_id: Option<String>,
    pub experiment_id: String,
    pub outcome_id: Option<String>,
    pub closed_at: Option<String>,
    pub status: ExperimentOutcomeStatus,
    pub decision_impact: String,
    pub confidence_delta: f64,
    pub priority_delta: i32,
    pub promoted_artifact_ids: Vec<String>,
    pub demoted_artifact_ids: Vec<String>,
    pub safety_notes: Vec<String>,
    pub audit_ids: Vec<String>,
    pub session_id: Option<String>,
    pub event_id: Option<String>,
    pub actor: Option<String>,
    pub dry_run: bool,
}

/// Report returned by `ee learn observe`.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnObserveReport {
    pub schema: String,
    pub status: String,
    pub dry_run: bool,
    pub observation: LearningObservation,
    pub feedback: Option<OutcomeRecordReport>,
    pub generated_at: String,
}

impl LearnObserveReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "success": true,
            "status": self.status,
            "dryRun": self.dry_run,
            "observation": self.observation.data_json(),
            "feedback": self.feedback.as_ref().map(OutcomeRecordReport::data_json),
            "generatedAt": self.generated_at,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        if self.dry_run {
            output.push_str("DRY RUN: Would attach learning observation\n\n");
        } else {
            output.push_str("Attached learning observation\n\n");
        }
        output.push_str(&format!(
            "  Experiment: {}\n",
            self.observation.experiment_id
        ));
        output.push_str(&format!(
            "  Observation: {}\n",
            self.observation.observation_id
        ));
        output.push_str(&format!("  Signal: {}\n", self.observation.signal.as_str()));
        output.push_str(&format!(
            "  Evidence IDs: {}\n",
            self.observation.evidence_ids.len()
        ));
        if let Some(feedback) = &self.feedback {
            if let Some(event_id) = &feedback.event_id {
                output.push_str(&format!("  Feedback event: {event_id}\n"));
            }
        }
        output
    }

    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "LEARN_OBSERVE|{}|{}|{}|evidence={}",
            self.status,
            self.observation.experiment_id,
            self.observation.signal.as_str(),
            self.observation.evidence_ids.len()
        )
    }
}

/// Deterministic score movement projected from a closed experiment outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnOutcomeEconomyScoreEffect {
    pub affected_artifact_ids: Vec<String>,
    pub promoted_count: usize,
    pub demoted_count: usize,
    pub utility_delta: f64,
    pub confidence_delta: f64,
    pub priority_delta: i32,
    pub priority_multiplier: f64,
    pub scoring_note: String,
}

impl LearnOutcomeEconomyScoreEffect {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "affectedArtifactIds": self.affected_artifact_ids,
            "promotedCount": self.promoted_count,
            "demotedCount": self.demoted_count,
            "utilityDelta": self.utility_delta,
            "confidenceDelta": self.confidence_delta,
            "priorityDelta": self.priority_delta,
            "priorityMultiplier": self.priority_multiplier,
            "scoringNote": self.scoring_note,
        })
    }
}

/// Procedure drift projection generated from an experiment close event.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnOutcomeProcedureDriftEffect {
    pub procedure_artifact_ids: Vec<String>,
    pub drift_signal: String,
    pub drift_score_delta: f64,
    pub requires_revalidation: bool,
    pub action: String,
}

impl LearnOutcomeProcedureDriftEffect {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "procedureArtifactIds": self.procedure_artifact_ids,
            "driftSignal": self.drift_signal,
            "driftScoreDelta": self.drift_score_delta,
            "requiresRevalidation": self.requires_revalidation,
            "action": self.action,
        })
    }
}

/// Tripwire false-alarm cost projection generated from an experiment close event.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnOutcomeTripwireFalseAlarmEffect {
    pub tripwire_artifact_ids: Vec<String>,
    pub false_alarm_cost_delta: u32,
    pub confidence_delta: f64,
    pub action: String,
    pub scoring_note: String,
}

impl LearnOutcomeTripwireFalseAlarmEffect {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "tripwireArtifactIds": self.tripwire_artifact_ids,
            "falseAlarmCostDelta": self.false_alarm_cost_delta,
            "confidenceDelta": self.confidence_delta,
            "action": self.action,
            "scoringNote": self.scoring_note,
        })
    }
}

/// Situation confidence projection generated from an experiment close event.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnOutcomeSituationConfidenceEffect {
    pub situation_artifact_ids: Vec<String>,
    pub confidence_delta: f64,
    pub confidence_direction: String,
    pub action: String,
}

impl LearnOutcomeSituationConfidenceEffect {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "situationArtifactIds": self.situation_artifact_ids,
            "confidenceDelta": self.confidence_delta,
            "confidenceDirection": self.confidence_direction,
            "action": self.action,
        })
    }
}

/// Audit metadata for downstream feedback projections.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnOutcomeDownstreamAudit {
    pub durable_feedback_recorded: bool,
    pub source_type: String,
    pub source_id: String,
    pub feedback_event_id: Option<String>,
    pub audit_id: Option<String>,
    pub silent_mutation: bool,
}

impl LearnOutcomeDownstreamAudit {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "durableFeedbackRecorded": self.durable_feedback_recorded,
            "sourceType": self.source_type,
            "sourceId": self.source_id,
            "feedbackEventId": self.feedback_event_id,
            "auditId": self.audit_id,
            "silentMutation": self.silent_mutation,
        })
    }
}

/// Downstream feedback projection produced by `ee learn close`.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnOutcomeDownstreamEffects {
    pub schema: &'static str,
    pub mutation_mode: String,
    pub economy_score: LearnOutcomeEconomyScoreEffect,
    pub procedure_drift: LearnOutcomeProcedureDriftEffect,
    pub tripwire_false_alarm: LearnOutcomeTripwireFalseAlarmEffect,
    pub situation_confidence: LearnOutcomeSituationConfidenceEffect,
    pub audit: LearnOutcomeDownstreamAudit,
}

impl LearnOutcomeDownstreamEffects {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "mutationMode": self.mutation_mode,
            "economyScore": self.economy_score.data_json(),
            "procedureDrift": self.procedure_drift.data_json(),
            "tripwireFalseAlarm": self.tripwire_false_alarm.data_json(),
            "situationConfidence": self.situation_confidence.data_json(),
            "audit": self.audit.data_json(),
        })
    }
}

/// Report returned by `ee learn close`.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnCloseReport {
    pub schema: String,
    pub status: String,
    pub dry_run: bool,
    pub outcome: ExperimentOutcome,
    pub feedback: Option<OutcomeRecordReport>,
    pub downstream_effects: LearnOutcomeDownstreamEffects,
    pub generated_at: String,
}

impl LearnCloseReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "success": true,
            "status": self.status,
            "dryRun": self.dry_run,
            "outcome": self.outcome.data_json(),
            "feedback": self.feedback.as_ref().map(OutcomeRecordReport::data_json),
            "downstreamEffects": self.downstream_effects.data_json(),
            "generatedAt": self.generated_at,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        if self.dry_run {
            output.push_str("DRY RUN: Would close learning experiment\n\n");
        } else {
            output.push_str("Closed learning experiment\n\n");
        }
        output.push_str(&format!("  Experiment: {}\n", self.outcome.experiment_id));
        output.push_str(&format!("  Outcome: {}\n", self.outcome.outcome_id));
        output.push_str(&format!("  Status: {}\n", self.outcome.status.as_str()));
        output.push_str(&format!(
            "  Promoted: {} | Demoted: {}\n",
            self.outcome.promoted_artifact_ids.len(),
            self.outcome.demoted_artifact_ids.len()
        ));
        output.push_str(&format!(
            "  Downstream feedback: {}\n",
            self.downstream_effects.mutation_mode
        ));
        output.push_str(&format!(
            "  Tripwire false-alarm delta: {}\n",
            self.downstream_effects
                .tripwire_false_alarm
                .false_alarm_cost_delta
        ));
        if let Some(feedback) = &self.feedback {
            if let Some(event_id) = &feedback.event_id {
                output.push_str(&format!("  Feedback event: {event_id}\n"));
            }
        }
        output
    }

    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "LEARN_CLOSE|{}|{}|{}|promoted={}|demoted={}|effect={}",
            self.status,
            self.outcome.experiment_id,
            self.outcome.status.as_str(),
            self.outcome.promoted_artifact_ids.len(),
            self.outcome.demoted_artifact_ids.len(),
            self.downstream_effects.mutation_mode
        )
    }
}

/// Attach evidence observed during a learning experiment.
///
/// Dry runs validate and render the observation without opening storage. Real
/// writes persist an audited feedback event against the experiment candidate.
pub fn observe_experiment(
    options: &LearnObserveOptions,
) -> Result<LearnObserveReport, DomainError> {
    let generated_at = Utc::now().to_rfc3339();
    let experiment_id = require_text(
        "experiment id",
        &options.experiment_id,
        "ee learn observe <experiment-id> --measurement-name verification --json",
    )?;
    let observation_id = options.observation_id.as_deref().map_or_else(
        || Ok(generate_learning_record_id("lobs")),
        |value| {
            require_text(
                "observation id",
                value,
                "ee learn observe --observation-id lobs_...",
            )
        },
    )?;
    let observed_at = options.observed_at.as_deref().map_or_else(
        || Ok(generated_at.clone()),
        |value| {
            require_text(
                "observed at",
                value,
                "ee learn observe --observed-at 2026-01-01T00:00:00Z",
            )
        },
    )?;
    let observer = options.observer.as_deref().map_or_else(
        || Ok("agent".to_string()),
        |value| require_text("observer", value, "ee learn observe --observer MistySalmon"),
    )?;
    let measurement_name = require_text(
        "measurement name",
        &options.measurement_name,
        "ee learn observe --measurement-name verification_status",
    )?;
    let evidence_ids = normalize_text_list(
        "evidence id",
        &options.evidence_ids,
        "ee learn observe --evidence-id ev_001",
    )?;
    let note = normalize_optional_text(
        "note",
        options.note.as_deref(),
        "ee learn observe --note 'dry-run passed'",
    )?;
    let redaction_status = options.redaction_status.as_deref().map_or_else(
        || Ok("not_required".to_string()),
        |value| {
            require_text(
                "redaction status",
                value,
                "ee learn observe --redaction-status redacted",
            )
        },
    )?;
    let measurement_value = validate_optional_metric(options.measurement_value)?;

    let mut observation = LearningObservation::new(
        observation_id,
        experiment_id.clone(),
        observed_at,
        observer,
        measurement_name,
    )
    .with_signal(options.signal)
    .with_redaction_status(redaction_status);
    if let Some(value) = measurement_value {
        observation = observation.with_measurement_value(value);
    }
    for evidence_id in evidence_ids {
        observation = observation.with_evidence(evidence_id);
    }
    if let Some(note) = note {
        observation = observation.with_note(note);
    }

    if options.dry_run {
        return Ok(LearnObserveReport {
            schema: LEARN_OBSERVE_SCHEMA_V1.to_string(),
            status: "dry_run".to_string(),
            dry_run: true,
            observation,
            feedback: None,
            generated_at,
        });
    }

    let database_path =
        learning_database_path(options.database_path.as_deref(), &options.workspace);
    let workspace_id = ensure_learning_workspace(
        &database_path,
        &options.workspace,
        options.workspace_id.as_deref(),
    )?;
    let evidence_json = observation.data_json().to_string();
    let feedback = record_outcome(&OutcomeRecordOptions {
        database_path: &database_path,
        target_type: "candidate".to_string(),
        target_id: experiment_id,
        workspace_id: Some(workspace_id.clone()),
        signal: observation_signal_to_feedback(options.signal).to_string(),
        weight: None,
        source_type: "automated_check".to_string(),
        source_id: Some(observation.observation_id.clone()),
        reason: observation.note.clone(),
        evidence_json: Some(evidence_json),
        session_id: options.session_id.clone(),
        event_id: options.event_id.clone(),
        actor: options.actor.clone(),
        dry_run: false,
        harmful_per_source_per_hour: crate::core::outcome::DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
        harmful_burst_window_seconds: crate::core::outcome::DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
    })?;
    persist_learning_observation(
        &database_path,
        &workspace_id,
        &LearningObservationLedgerInput {
            observation_kind: "experiment_observe",
            source_type: "feedback_event",
            source_id: feedback
                .event_id
                .clone()
                .or_else(|| feedback.source_id.clone()),
            target_type: "candidate",
            target_id: &observation.experiment_id,
            topic: Some(normalize_topic(&observation.experiment_id)),
            signal: observation_signal_to_feedback(options.signal),
            evidence_json: Some(observation.data_json().to_string()),
            observed_at: &observation.observed_at,
        },
    )?;
    let status = if feedback.status.as_str() == "already_recorded" {
        "already_recorded"
    } else {
        "observed"
    };

    Ok(LearnObserveReport {
        schema: LEARN_OBSERVE_SCHEMA_V1.to_string(),
        status: status.to_string(),
        dry_run: false,
        observation,
        feedback: Some(feedback),
        generated_at,
    })
}

/// Close a learning experiment with a confirmed, rejected, inconclusive, or
/// unsafe outcome.
pub fn close_experiment(options: &LearnCloseOptions) -> Result<LearnCloseReport, DomainError> {
    let generated_at = Utc::now().to_rfc3339();
    let experiment_id = require_text(
        "experiment id",
        &options.experiment_id,
        "ee learn close <experiment-id> --status confirmed --decision-impact '...'",
    )?;
    let outcome_id = options.outcome_id.as_deref().map_or_else(
        || Ok(generate_learning_record_id("lout")),
        |value| require_text("outcome id", value, "ee learn close --outcome-id lout_..."),
    )?;
    let closed_at = options.closed_at.as_deref().map_or_else(
        || Ok(generated_at.clone()),
        |value| {
            require_text(
                "closed at",
                value,
                "ee learn close --closed-at 2026-01-01T00:00:00Z",
            )
        },
    )?;
    let decision_impact = require_text(
        "decision impact",
        &options.decision_impact,
        "ee learn close --decision-impact 'confirmed promotion evidence'",
    )?;
    let promoted_artifact_ids = normalize_text_list(
        "promoted artifact id",
        &options.promoted_artifact_ids,
        "ee learn close --promote-artifact mem_001",
    )?;
    let demoted_artifact_ids = normalize_text_list(
        "demoted artifact id",
        &options.demoted_artifact_ids,
        "ee learn close --demote-artifact mem_002",
    )?;
    let safety_notes = normalize_text_list(
        "safety note",
        &options.safety_notes,
        "ee learn close --safety-note 'no unsafe mutation'",
    )?;
    let audit_ids = normalize_text_list(
        "audit id",
        &options.audit_ids,
        "ee learn close --audit-id audit_001",
    )?;
    let confidence_delta = validate_metric_delta(options.confidence_delta, "confidence delta")?;

    let mut outcome = ExperimentOutcome::new(
        outcome_id,
        experiment_id.clone(),
        closed_at,
        decision_impact,
    )
    .with_status(options.status)
    .with_confidence_delta(confidence_delta)
    .with_priority_delta(options.priority_delta);
    for artifact_id in promoted_artifact_ids {
        outcome = outcome.with_promoted_artifact(artifact_id);
    }
    for artifact_id in demoted_artifact_ids {
        outcome = outcome.with_demoted_artifact(artifact_id);
    }
    for note in safety_notes {
        outcome = outcome.with_safety_note(note);
    }
    for audit_id in audit_ids {
        outcome = outcome.with_audit_id(audit_id);
    }

    if options.dry_run {
        let downstream_effects = downstream_effects_for_outcome(&outcome, true, None);
        return Ok(LearnCloseReport {
            schema: LEARN_CLOSE_SCHEMA_V1.to_string(),
            status: "dry_run".to_string(),
            dry_run: true,
            outcome,
            feedback: None,
            downstream_effects,
            generated_at,
        });
    }

    let database_path =
        learning_database_path(options.database_path.as_deref(), &options.workspace);
    let workspace_id = ensure_learning_workspace(
        &database_path,
        &options.workspace,
        options.workspace_id.as_deref(),
    )?;
    let evidence_json = outcome.data_json().to_string();
    let feedback = record_outcome(&OutcomeRecordOptions {
        database_path: &database_path,
        target_type: "candidate".to_string(),
        target_id: experiment_id,
        workspace_id: Some(workspace_id.clone()),
        signal: outcome_status_to_feedback(options.status).to_string(),
        weight: None,
        source_type: "outcome_observed".to_string(),
        source_id: Some(outcome.outcome_id.clone()),
        reason: Some(outcome.decision_impact.clone()),
        evidence_json: Some(evidence_json),
        session_id: options.session_id.clone(),
        event_id: options.event_id.clone(),
        actor: options.actor.clone(),
        dry_run: false,
        harmful_per_source_per_hour: crate::core::outcome::DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
        harmful_burst_window_seconds: crate::core::outcome::DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
    })?;
    persist_learning_observation(
        &database_path,
        &workspace_id,
        &LearningObservationLedgerInput {
            observation_kind: "experiment_close",
            source_type: "feedback_event",
            source_id: feedback
                .event_id
                .clone()
                .or_else(|| feedback.source_id.clone()),
            target_type: "candidate",
            target_id: &outcome.experiment_id,
            topic: Some(normalize_topic(&outcome.experiment_id)),
            signal: outcome_status_to_feedback(options.status),
            evidence_json: Some(outcome.data_json().to_string()),
            observed_at: &outcome.closed_at,
        },
    )?;
    let status = if feedback.status.as_str() == "already_recorded" {
        "already_recorded"
    } else {
        "closed"
    };
    let downstream_effects = downstream_effects_for_outcome(&outcome, false, Some(&feedback));

    Ok(LearnCloseReport {
        schema: LEARN_CLOSE_SCHEMA_V1.to_string(),
        status: status.to_string(),
        dry_run: false,
        outcome,
        feedback: Some(feedback),
        downstream_effects,
        generated_at,
    })
}

fn learning_database_path(database_path: Option<&Path>, workspace: &Path) -> PathBuf {
    database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace.join(".ee").join("ee.db"))
}

fn ensure_learning_workspace(
    database_path: &Path,
    workspace_path: &Path,
    workspace_id: Option<&str>,
) -> Result<String, DomainError> {
    if let Some(workspace_id) = workspace_id {
        return require_text(
            "workspace id",
            workspace_id,
            "ee learn observe --workspace-id wsp_...",
        );
    }
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_string()),
        });
    }
    let connection =
        DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    let normalized_workspace = normalize_workspace_path(workspace_path);
    let workspace_path_text = normalized_workspace.to_string_lossy().into_owned();
    if let Some(workspace) = connection
        .get_workspace_by_path(&workspace_path_text)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
    {
        connection.close().map_err(|error| DomainError::Storage {
            message: format!("Failed to close database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
        return Ok(workspace.id);
    }

    let workspace_id = stable_workspace_id(&workspace_path_text);
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace_path_text,
                name: normalized_workspace
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to register workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    connection.close().map_err(|error| DomainError::Storage {
        message: format!("Failed to close database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;
    Ok(workspace_id)
}

fn normalize_workspace_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

fn stable_workspace_id(path: &str) -> String {
    let hash = blake3::hash(format!("workspace:{path}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn generate_learning_record_id(prefix: &str) -> String {
    format!("{}_{}", prefix, uuid::Uuid::now_v7().simple())
}

fn observation_signal_to_feedback(signal: LearningObservationSignal) -> &'static str {
    match signal {
        LearningObservationSignal::Positive => "positive",
        LearningObservationSignal::Negative => "negative",
        LearningObservationSignal::Neutral => "neutral",
        LearningObservationSignal::Safety => "harmful",
    }
}

fn outcome_status_to_feedback(status: ExperimentOutcomeStatus) -> &'static str {
    match status {
        ExperimentOutcomeStatus::Confirmed => "confirmation",
        ExperimentOutcomeStatus::Rejected => "contradiction",
        ExperimentOutcomeStatus::Inconclusive => "neutral",
        ExperimentOutcomeStatus::Unsafe => "harmful",
    }
}

fn downstream_effects_for_outcome(
    outcome: &ExperimentOutcome,
    dry_run: bool,
    feedback: Option<&OutcomeRecordReport>,
) -> LearnOutcomeDownstreamEffects {
    let mutation_mode = if dry_run {
        "dry_run_projection"
    } else if feedback.is_some_and(|report| report.status.as_str() == "already_recorded") {
        "already_recorded_no_new_mutation"
    } else {
        "audited_feedback_recorded"
    };
    let source_id = feedback
        .and_then(|report| report.source_id.clone())
        .unwrap_or_else(|| outcome.outcome_id.clone());

    LearnOutcomeDownstreamEffects {
        schema: LEARN_DOWNSTREAM_EFFECTS_SCHEMA_V1,
        mutation_mode: mutation_mode.to_string(),
        economy_score: economy_score_effect_for_outcome(outcome),
        procedure_drift: procedure_drift_effect_for_outcome(outcome),
        tripwire_false_alarm: tripwire_false_alarm_effect_for_outcome(outcome),
        situation_confidence: situation_confidence_effect_for_outcome(outcome),
        audit: LearnOutcomeDownstreamAudit {
            durable_feedback_recorded: !dry_run && feedback.is_some(),
            source_type: "outcome_observed".to_string(),
            source_id,
            feedback_event_id: feedback.and_then(|report| report.event_id.clone()),
            audit_id: feedback.and_then(|report| report.audit_id.clone()),
            silent_mutation: false,
        },
    }
}

fn economy_score_effect_for_outcome(outcome: &ExperimentOutcome) -> LearnOutcomeEconomyScoreEffect {
    let affected_artifact_ids = affected_artifact_ids(outcome);
    let promoted_count = outcome.promoted_artifact_ids.len();
    let demoted_count = outcome.demoted_artifact_ids.len();
    let status_base = match outcome.status {
        ExperimentOutcomeStatus::Confirmed => 0.10,
        ExperimentOutcomeStatus::Rejected => -0.10,
        ExperimentOutcomeStatus::Inconclusive => 0.0,
        ExperimentOutcomeStatus::Unsafe => -0.25,
    };
    let artifact_delta = promoted_count as f64 * 0.02 - demoted_count as f64 * 0.02;
    let utility_delta = rounded_metric(
        (status_base + outcome.confidence_delta * 0.5 + artifact_delta).clamp(-1.0, 1.0),
    );
    let priority_multiplier = match outcome.status {
        ExperimentOutcomeStatus::Confirmed => 1.05,
        ExperimentOutcomeStatus::Rejected => 0.80,
        ExperimentOutcomeStatus::Inconclusive => 1.0,
        ExperimentOutcomeStatus::Unsafe => 0.50,
    };
    let scoring_note = match outcome.status {
        ExperimentOutcomeStatus::Confirmed => {
            "Confirmed experiment outcome raises utility for promoted evidence."
        }
        ExperimentOutcomeStatus::Rejected => {
            "Rejected experiment outcome lowers utility and pushes demoted evidence toward review."
        }
        ExperimentOutcomeStatus::Inconclusive => {
            "Inconclusive experiment outcome is retained without economy score movement."
        }
        ExperimentOutcomeStatus::Unsafe => {
            "Unsafe experiment outcome sharply lowers utility pending manual review."
        }
    };

    LearnOutcomeEconomyScoreEffect {
        affected_artifact_ids,
        promoted_count,
        demoted_count,
        utility_delta,
        confidence_delta: rounded_metric(outcome.confidence_delta),
        priority_delta: outcome.priority_delta,
        priority_multiplier,
        scoring_note: scoring_note.to_string(),
    }
}

fn procedure_drift_effect_for_outcome(
    outcome: &ExperimentOutcome,
) -> LearnOutcomeProcedureDriftEffect {
    let procedure_artifact_ids = artifact_ids_for_kind(outcome, LearningTargetKind::Procedure);
    let drift_signal = match outcome.status {
        ExperimentOutcomeStatus::Confirmed => "validated_by_experiment",
        ExperimentOutcomeStatus::Rejected => "contradicted_by_experiment",
        ExperimentOutcomeStatus::Inconclusive => "needs_more_evidence",
        ExperimentOutcomeStatus::Unsafe => "unsafe_drift",
    };
    let drift_score_delta = match outcome.status {
        ExperimentOutcomeStatus::Confirmed => -0.10,
        ExperimentOutcomeStatus::Rejected => 0.25,
        ExperimentOutcomeStatus::Inconclusive => 0.05,
        ExperimentOutcomeStatus::Unsafe => 0.50,
    };
    let requires_revalidation = matches!(
        outcome.status,
        ExperimentOutcomeStatus::Rejected
            | ExperimentOutcomeStatus::Inconclusive
            | ExperimentOutcomeStatus::Unsafe
    ) && !procedure_artifact_ids.is_empty();
    let action = if procedure_artifact_ids.is_empty() {
        "no_procedure_artifacts"
    } else {
        match outcome.status {
            ExperimentOutcomeStatus::Confirmed => "promote_procedure_evidence",
            ExperimentOutcomeStatus::Rejected => "revalidate_or_demote_procedure",
            ExperimentOutcomeStatus::Inconclusive => "keep_procedure_pending",
            ExperimentOutcomeStatus::Unsafe => "halt_procedure_promotion",
        }
    };

    LearnOutcomeProcedureDriftEffect {
        procedure_artifact_ids,
        drift_signal: drift_signal.to_string(),
        drift_score_delta: rounded_metric(drift_score_delta),
        requires_revalidation,
        action: action.to_string(),
    }
}

fn tripwire_false_alarm_effect_for_outcome(
    outcome: &ExperimentOutcome,
) -> LearnOutcomeTripwireFalseAlarmEffect {
    let tripwire_artifact_ids = artifact_ids_for_kind(outcome, LearningTargetKind::Tripwire);
    let demoted_tripwire_count =
        artifact_ids_for_kind_from(&outcome.demoted_artifact_ids, LearningTargetKind::Tripwire)
            .len();
    let promoted_tripwire_count =
        artifact_ids_for_kind_from(&outcome.promoted_artifact_ids, LearningTargetKind::Tripwire)
            .len();
    let false_alarm_cost_delta = if matches!(
        outcome.status,
        ExperimentOutcomeStatus::Rejected | ExperimentOutcomeStatus::Unsafe
    ) {
        demoted_tripwire_count as u32
    } else {
        0
    };
    let confidence_delta = if false_alarm_cost_delta > 0 {
        -0.12 * f64::from(false_alarm_cost_delta)
    } else if outcome.status == ExperimentOutcomeStatus::Confirmed && promoted_tripwire_count > 0 {
        0.08 * promoted_tripwire_count as f64
    } else {
        0.0
    };
    let (action, scoring_note) = if false_alarm_cost_delta > 0 {
        (
            "increase_false_alarm_cost",
            "Closed outcome contradicted a demoted tripwire; increase false-alarm cost without deleting evidence.",
        )
    } else if outcome.status == ExperimentOutcomeStatus::Confirmed && promoted_tripwire_count > 0 {
        (
            "confirm_tripwire",
            "Closed outcome confirmed promoted tripwire evidence.",
        )
    } else {
        (
            "retain_tripwire_audit",
            "Closed outcome does not move tripwire false-alarm cost.",
        )
    };

    LearnOutcomeTripwireFalseAlarmEffect {
        tripwire_artifact_ids,
        false_alarm_cost_delta,
        confidence_delta: rounded_metric(confidence_delta),
        action: action.to_string(),
        scoring_note: scoring_note.to_string(),
    }
}

fn situation_confidence_effect_for_outcome(
    outcome: &ExperimentOutcome,
) -> LearnOutcomeSituationConfidenceEffect {
    let situation_artifact_ids = artifact_ids_for_kind(outcome, LearningTargetKind::Situation);
    let confidence_delta = rounded_metric(outcome.confidence_delta);
    let confidence_direction = if confidence_delta > 0.0 {
        "increase"
    } else if confidence_delta < 0.0 {
        "decrease"
    } else {
        "unchanged"
    };
    let action = if situation_artifact_ids.is_empty() {
        "no_situation_artifacts"
    } else {
        match confidence_direction {
            "increase" => "increase_situation_confidence",
            "decrease" => "decrease_situation_confidence",
            _ => "retain_situation_confidence",
        }
    };

    LearnOutcomeSituationConfidenceEffect {
        situation_artifact_ids,
        confidence_delta,
        confidence_direction: confidence_direction.to_string(),
        action: action.to_string(),
    }
}

fn affected_artifact_ids(outcome: &ExperimentOutcome) -> Vec<String> {
    let mut ids = outcome.promoted_artifact_ids.clone();
    ids.extend(outcome.demoted_artifact_ids.iter().cloned());
    ids.sort();
    ids.dedup();
    ids
}

fn artifact_ids_for_kind(outcome: &ExperimentOutcome, kind: LearningTargetKind) -> Vec<String> {
    artifact_ids_for_kind_from(&affected_artifact_ids(outcome), kind)
}

fn artifact_ids_for_kind_from(ids: &[String], kind: LearningTargetKind) -> Vec<String> {
    let mut matching = ids
        .iter()
        .filter(|id| infer_learning_target_kind(id) == kind)
        .cloned()
        .collect::<Vec<_>>();
    matching.sort();
    matching.dedup();
    matching
}

fn infer_learning_target_kind(artifact_id: &str) -> LearningTargetKind {
    let artifact = artifact_id.to_ascii_lowercase();
    if artifact.starts_with("proc_")
        || artifact.starts_with("procedure_")
        || artifact.contains("procedure")
    {
        LearningTargetKind::Procedure
    } else if artifact.starts_with("tw_")
        || artifact.starts_with("tripwire_")
        || artifact.contains("tripwire")
    {
        LearningTargetKind::Tripwire
    } else if artifact.starts_with("sit_")
        || artifact.starts_with("situation_")
        || artifact.contains("situation")
    {
        LearningTargetKind::Situation
    } else if artifact.starts_with("econ_")
        || artifact.starts_with("economy_")
        || artifact.contains("economy")
        || artifact.contains("budget")
    {
        LearningTargetKind::Economy
    } else if artifact.starts_with("decision_") || artifact.contains("decision") {
        LearningTargetKind::Decision
    } else {
        LearningTargetKind::Memory
    }
}

fn require_text(field: &str, raw: &str, repair: &str) -> Result<String, DomainError> {
    let value = raw.trim();
    if value.is_empty() {
        Err(DomainError::Usage {
            message: format!("{field} must not be empty"),
            repair: Some(repair.to_string()),
        })
    } else {
        Ok(value.to_string())
    }
}

fn normalize_optional_text(
    field: &str,
    raw: Option<&str>,
    repair: &str,
) -> Result<Option<String>, DomainError> {
    raw.map(|value| require_text(field, value, repair))
        .transpose()
}

fn normalize_text_list(
    field: &str,
    raw: &[String],
    repair: &str,
) -> Result<Vec<String>, DomainError> {
    let mut values = raw
        .iter()
        .map(|value| require_text(field, value, repair))
        .collect::<Result<Vec<_>, _>>()?;
    values.sort();
    values.dedup();
    Ok(values)
}

fn validate_optional_metric(value: Option<f64>) -> Result<Option<f64>, DomainError> {
    value
        .map(|metric| validate_metric(metric, "measurement value"))
        .transpose()
}

fn validate_metric(value: f64, field: &str) -> Result<f64, DomainError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(DomainError::Usage {
            message: format!("{field} must be finite"),
            repair: Some("Use a finite numeric value.".to_string()),
        })
    }
}

fn validate_metric_delta(value: f64, field: &str) -> Result<f64, DomainError> {
    let value = validate_metric(value, field)?;
    if (-1.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(DomainError::Usage {
            message: format!("{field} must be between -1.0 and 1.0"),
            repair: Some("Use --confidence-delta 0.0".to_string()),
        })
    }
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

/// Options for running a safe learning experiment rehearsal.
#[derive(Clone, Debug)]
pub struct LearnExperimentRunOptions {
    pub workspace: PathBuf,
    pub experiment_id: String,
    pub max_attention_tokens: u32,
    pub max_runtime_seconds: u32,
    pub dry_run: bool,
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

impl Default for LearnExperimentRunOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::new(),
            experiment_id: String::new(),
            max_attention_tokens: 1_200,
            max_runtime_seconds: 300,
            dry_run: true,
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

/// Budget envelope for a learning experiment run rehearsal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentRunBudget {
    pub requested_attention_tokens: u32,
    pub requested_runtime_seconds: u32,
    pub planned_attention_tokens: u32,
    pub planned_runtime_seconds: u32,
    pub shadow_budget_delta_tokens: i32,
    pub budget_class: String,
}

/// One deterministic step in a learning experiment rehearsal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentRunStep {
    pub order: u32,
    pub name: String,
    pub action: String,
    pub expected_signal: String,
    pub writes_storage: bool,
}

/// Observation that a dry-run experiment would attach through `ee learn observe`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentRunObservationPreview {
    pub signal: String,
    pub measurement_name: String,
    pub measurement_value: Option<f64>,
    pub evidence_ids: Vec<String>,
    pub note: String,
}

/// Outcome that a dry-run experiment would close through `ee learn close`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentRunOutcomePreview {
    pub status: String,
    pub decision_impact: String,
    pub confidence_delta: f64,
    pub priority_delta: i32,
    pub promoted_artifact_ids: Vec<String>,
    pub demoted_artifact_ids: Vec<String>,
    pub safety_notes: Vec<String>,
}

/// Report returned by `ee learn experiment run --dry-run --json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LearnExperimentRunReport {
    pub schema: String,
    pub status: String,
    pub dry_run: bool,
    pub experiment_id: String,
    pub experiment_kind: String,
    pub title: String,
    pub hypothesis: String,
    pub budget: ExperimentRunBudget,
    pub safety: ExperimentSafetyPlan,
    pub steps: Vec<ExperimentRunStep>,
    pub observations: Vec<ExperimentRunObservationPreview>,
    pub outcome_preview: ExperimentRunOutcomePreview,
    pub next_actions: Vec<String>,
    pub generated_at: String,
}

impl LearnExperimentRunReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "success": true,
            "status": self.status,
            "dryRun": self.dry_run,
            "experimentId": self.experiment_id,
            "experimentKind": self.experiment_kind,
            "title": self.title,
            "hypothesis": self.hypothesis,
            "budget": self.budget,
            "safety": self.safety,
            "steps": self.steps,
            "observations": self.observations,
            "outcomePreview": self.outcome_preview,
            "nextActions": self.next_actions,
            "generatedAt": self.generated_at,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        output.push_str("Learning Experiment Run [DRY RUN]\n\n");
        output.push_str(&format!("Experiment: {}\n", self.experiment_id));
        output.push_str(&format!("Kind: {}\n", self.experiment_kind));
        output.push_str(&format!("Status: {}\n", self.status));
        output.push_str(&format!(
            "Budget: {} tokens, {}s runtime ({})\n",
            self.budget.planned_attention_tokens,
            self.budget.planned_runtime_seconds,
            self.budget.budget_class
        ));
        output.push_str("\nSteps:\n");
        for step in &self.steps {
            output.push_str(&format!(
                "  {}. {} -> {}\n",
                step.order, step.name, step.expected_signal
            ));
        }
        output.push_str("\nNext:\n");
        for action in &self.next_actions {
            output.push_str(&format!("  {action}\n"));
        }
        output
    }

    #[must_use]
    pub fn toon_summary(&self) -> String {
        format!(
            "LEARN_EXPERIMENT_RUN|{}|{}|steps={}|observations={}|dry_run={}",
            self.experiment_id,
            self.experiment_kind,
            self.steps.len(),
            self.observations.len(),
            self.dry_run
        )
    }
}

/// Propose safe dry-run-first experiments that could change memory decisions.
pub fn propose_experiments(
    options: &LearnExperimentProposeOptions,
) -> Result<LearnExperimentProposalReport, DomainError> {
    let snapshot = load_learning_snapshot(&options.workspace)?;
    let clusters = load_learning_clusters(&options.workspace, options.topic.as_deref())?;
    let database_path = learning_database_path(None, &options.workspace);
    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let mut durable_proposals = Vec::new();
    for cluster in clusters
        .iter()
        .filter(|cluster| cluster.is_non_trivial())
        .filter(|cluster| cluster.expected_value() >= options.min_expected_value)
    {
        let Some(target_memory_id) = cluster.target_memory_id() else {
            continue;
        };
        let candidate_id = cluster.curation_candidate_id();
        let proposed_content = cluster.proposed_rule_content();
        let source_ids = cluster.sample_ids_vec();
        let already_exists = connection
            .get_curation_candidate(&snapshot.workspace_id, &candidate_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to check learning curation candidate: {error}"),
                repair: Some("ee curate candidates --json".to_string()),
            })?
            .is_some();
        if !already_exists {
            connection
                .insert_curation_candidate(
                    &candidate_id,
                    &CreateCurationCandidateInput {
                        workspace_id: snapshot.workspace_id.clone(),
                        candidate_type: "rule".to_string(),
                        target_memory_id: target_memory_id.clone(),
                        proposed_content: Some(proposed_content),
                        proposed_confidence: Some(cluster.proposed_confidence()),
                        proposed_trust_class: Some("agent_validated".to_string()),
                        source_type: "feedback_event".to_string(),
                        source_id: Some(source_ids.join(",")),
                        reason: cluster.proposal_reason(),
                        confidence: cluster.proposed_confidence(),
                        status: Some("pending".to_string()),
                        created_at: Some(stable_learning_generated_at()),
                        ttl_expires_at: None,
                    },
                )
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to insert learning curation candidate: {error}"),
                    repair: Some("ee curate candidates --json".to_string()),
                })?;
        }
        durable_proposals.push(cluster.experiment_proposal(
            options.max_attention_tokens,
            options.max_runtime_seconds,
            options.safety_boundary,
        ));
    }
    connection.close().map_err(|error| DomainError::Storage {
        message: format!("Failed to close database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;

    durable_proposals.sort_by(|left, right| {
        right
            .expected_value
            .total_cmp(&left.expected_value)
            .then_with(|| left.topic.cmp(&right.topic))
            .then_with(|| left.experiment_id.cmp(&right.experiment_id))
    });
    let total_candidates = durable_proposals.len() as u32;
    durable_proposals.truncate(options.limit as usize);
    let returned = durable_proposals.len() as u32;

    Ok(LearnExperimentProposalReport {
        schema: LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1.to_string(),
        proposals: durable_proposals,
        total_candidates,
        returned,
        min_expected_value: rounded_metric(options.min_expected_value),
        max_attention_tokens: options.max_attention_tokens,
        max_runtime_seconds: options.max_runtime_seconds,
        generated_at: stable_learning_generated_at(),
    })
}

/// Run a deterministic dry-run-only active learning experiment rehearsal.
///
/// Abstains with `experiment_registry_unavailable` because experiment definitions
/// must come from persisted evaluation registries, not hard-coded templates.
pub fn run_experiment(
    _options: &LearnExperimentRunOptions,
) -> Result<LearnExperimentRunReport, DomainError> {
    Err(experiment_registry_unavailable())
}

fn rounded_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

fn stable_learning_generated_at() -> String {
    "1970-01-01T00:00:00Z".to_string()
}

#[derive(Clone, Debug)]
struct LearningSnapshot {
    workspace_id: String,
    memories: BTreeMap<String, StoredMemory>,
    memory_tags: BTreeMap<String, Vec<String>>,
    feedback_events: Vec<StoredFeedbackEvent>,
    learning_observations: Vec<StoredLearningObservation>,
    curation_candidates: Vec<StoredCurationCandidate>,
}

struct LearningObservationLedgerInput<'a> {
    observation_kind: &'a str,
    source_type: &'a str,
    source_id: Option<String>,
    target_type: &'a str,
    target_id: &'a str,
    topic: Option<String>,
    signal: &'a str,
    evidence_json: Option<String>,
    observed_at: &'a str,
}

#[derive(Clone, Debug)]
struct LearningCluster {
    topic: String,
    positive_count: u32,
    negative_count: u32,
    neutral_count: u32,
    decay_count: u32,
    positive_weight: f64,
    negative_weight: f64,
    neutral_weight: f64,
    decay_weight: f64,
    memory_ids: BTreeSet<String>,
    sample_ids: BTreeSet<String>,
    source_types: BTreeSet<String>,
    content_previews: BTreeSet<String>,
    last_seen_at: String,
}

impl LearningCluster {
    fn new(topic: String) -> Self {
        Self {
            topic,
            positive_count: 0,
            negative_count: 0,
            neutral_count: 0,
            decay_count: 0,
            positive_weight: 0.0,
            negative_weight: 0.0,
            neutral_weight: 0.0,
            decay_weight: 0.0,
            memory_ids: BTreeSet::new(),
            sample_ids: BTreeSet::new(),
            source_types: BTreeSet::new(),
            content_previews: BTreeSet::new(),
            last_seen_at: stable_learning_generated_at(),
        }
    }

    fn record(
        &mut self,
        event: &StoredFeedbackEvent,
        evidence_ids: BTreeSet<String>,
        memory_ids: BTreeSet<String>,
        previews: BTreeSet<String>,
    ) {
        match signal_bucket(&event.signal) {
            SignalBucket::Positive => {
                self.positive_count = self.positive_count.saturating_add(1);
                self.positive_weight += f64::from(event.weight);
            }
            SignalBucket::Negative => {
                self.negative_count = self.negative_count.saturating_add(1);
                self.negative_weight += f64::from(event.weight);
            }
            SignalBucket::Decay => {
                self.decay_count = self.decay_count.saturating_add(1);
                self.decay_weight += f64::from(event.weight);
            }
            SignalBucket::Neutral => {
                self.neutral_count = self.neutral_count.saturating_add(1);
                self.neutral_weight += f64::from(event.weight);
            }
        }
        self.sample_ids.insert(event.id.clone());
        self.sample_ids.insert(event.target_id.clone());
        if let Some(source_id) = &event.source_id {
            self.sample_ids.insert(source_id.clone());
        }
        self.sample_ids.extend(evidence_ids);
        self.memory_ids.extend(memory_ids);
        self.source_types.insert(event.source_type.clone());
        self.content_previews.extend(previews);
        if event.created_at > self.last_seen_at {
            self.last_seen_at = event.created_at.clone();
        }
    }

    fn total_count(&self) -> u32 {
        self.positive_count + self.negative_count + self.neutral_count + self.decay_count
    }

    fn confidence(&self) -> f64 {
        let total =
            self.positive_weight + self.negative_weight + self.neutral_weight + self.decay_weight;
        if total <= f64::EPSILON {
            return 0.0;
        }
        rounded_metric(
            ((self.positive_weight + self.neutral_weight * 0.5)
                / (total + self.decay_weight * 0.5))
                .clamp(0.0, 1.0),
        )
    }

    fn uncertainty(&self) -> f64 {
        let positive = self.positive_weight.max(0.0);
        let negative = self.negative_weight.max(0.0);
        let neutral = (self.neutral_weight + self.decay_weight).max(0.0);
        let total = positive + negative + neutral;
        let entropy = if total <= f64::EPSILON {
            0.0
        } else {
            let mut entropy = 0.0;
            for weight in [positive, negative, neutral] {
                if weight > f64::EPSILON {
                    let probability = weight / total;
                    entropy -= probability * probability.log2();
                }
            }
            entropy / 3.0_f64.log2()
        };
        let scarcity = if self.total_count() >= 4 {
            0.0
        } else {
            (4.0 - f64::from(self.total_count())) / 4.0
        };
        rounded_metric(entropy.max(scarcity).clamp(0.0, 1.0))
    }

    fn priority(&self) -> u8 {
        let evidence_bonus = f64::from(self.total_count().min(20)) * 1.5;
        let contradiction_bonus = if self.positive_count > 0 && self.negative_count > 0 {
            15.0
        } else {
            0.0
        };
        let priority = self.uncertainty() * 65.0 + evidence_bonus + contradiction_bonus;
        priority.round().clamp(1.0, 100.0) as u8
    }

    fn status(&self) -> &'static str {
        if self.total_count() >= 8
            && self.negative_count == 0
            && self.decay_count == 0
            && self.uncertainty() < 0.25
        {
            "resolved"
        } else {
            "open"
        }
    }

    fn agenda_item(&self) -> AgendaItem {
        AgendaItem {
            id: self.question_id(),
            topic: self.topic.clone(),
            gap_description: self.gap_description(),
            priority: self.priority(),
            uncertainty: self.uncertainty(),
            source: self.source(),
            sample_ids: self.sample_ids_vec(),
            status: self.status().to_string(),
            created_at: self.last_seen_at.clone(),
        }
    }

    fn uncertainty_item(&self) -> UncertaintyItem {
        UncertaintyItem {
            memory_id: self
                .target_memory_id()
                .unwrap_or_else(|| self.question_id()),
            content_preview: self.content_preview(),
            kind: self.topic.clone(),
            uncertainty: self.uncertainty(),
            confidence: self.confidence(),
            retrieval_count: self.total_count(),
            last_accessed: Some(self.last_seen_at.clone()),
        }
    }

    fn experiment_proposal(
        &self,
        max_attention_tokens: u32,
        max_runtime_seconds: u32,
        safety_boundary: ExperimentSafetyBoundary,
    ) -> ExperimentProposal {
        let evidence_ids = self.sample_ids_vec();
        let target_memory_id = self
            .target_memory_id()
            .unwrap_or_else(|| self.question_id());
        let experiment_id = self.experiment_id();
        ExperimentProposal {
            experiment_id: experiment_id.clone(),
            question_id: self.question_id(),
            title: format!("Validate {} learning cluster", self.topic),
            hypothesis: self.proposal_hypothesis(),
            status: "proposed".to_string(),
            topic: self.topic.clone(),
            expected_value: self.expected_value(),
            uncertainty_reduction: rounded_metric((self.uncertainty() * 0.45 + 0.15).min(1.0)),
            confidence: self.confidence(),
            budget: ExperimentBudget {
                attention_tokens: max_attention_tokens,
                max_runtime_seconds,
                dry_run_required: true,
                budget_class: budget_class(max_attention_tokens, max_runtime_seconds).to_string(),
            },
            safety: safety_plan(safety_boundary),
            decision_impact: ExperimentDecisionImpact {
                decision_id: format!(
                    "decision_{}",
                    stable_suffix("learn_decision", &self.topic, 20)
                ),
                target_artifact_ids: vec![target_memory_id],
                current_decision: self.current_decision(),
                possible_change: self.possible_change(),
                impact_score: rounded_metric((self.expected_value() + self.uncertainty()) / 2.0),
            },
            evidence_ids,
            next_command: format!("ee learn experiment run --dry-run --id {experiment_id} --json"),
        }
    }

    fn is_non_trivial(&self) -> bool {
        self.total_count() >= 2 && self.sample_ids.len() >= 2 && !self.memory_ids.is_empty()
    }

    fn question_id(&self) -> String {
        format!("gap_{}", stable_suffix("learn_question", &self.topic, 20))
    }

    fn experiment_id(&self) -> String {
        format!("exp_{}", stable_suffix("learn_experiment", &self.topic, 24))
    }

    fn curation_candidate_id(&self) -> String {
        format!(
            "curate_{}",
            stable_suffix("learn_candidate", &self.topic, 26)
        )
    }

    fn target_memory_id(&self) -> Option<String> {
        self.memory_ids.iter().next().cloned()
    }

    fn sample_ids_vec(&self) -> Vec<String> {
        self.sample_ids.iter().take(12).cloned().collect()
    }

    fn proposed_confidence(&self) -> f32 {
        rounded_metric((self.confidence() * 0.75 + self.expected_value() * 0.25).clamp(0.05, 0.95))
            as f32
    }

    fn expected_value(&self) -> f64 {
        let evidence_strength = (f64::from(self.total_count()).ln_1p() / 4.0).min(0.35);
        let contradiction_value = if self.positive_count > 0 && self.negative_count > 0 {
            0.15
        } else {
            0.0
        };
        rounded_metric(
            (self.uncertainty() * 0.35
                + self.confidence() * 0.20
                + evidence_strength
                + contradiction_value)
                .clamp(0.0, 1.0),
        )
    }

    fn proposed_rule_content(&self) -> String {
        if self.positive_count >= self.negative_count {
            format!(
                "For {}, prefer the pattern supported by {} positive outcome(s) and {} total observation(s): {}",
                self.topic,
                self.positive_count,
                self.total_count(),
                self.content_preview()
            )
        } else {
            format!(
                "For {}, avoid or revalidate the pattern contradicted by {} negative outcome(s): {}",
                self.topic,
                self.negative_count,
                self.content_preview()
            )
        }
    }

    fn proposal_reason(&self) -> String {
        format!(
            "Learning cluster `{}` has {} observation(s), uncertainty {:.3}, confidence {:.3}, and {} evidence pointer(s).",
            self.topic,
            self.total_count(),
            self.uncertainty(),
            self.confidence(),
            self.sample_ids.len()
        )
    }

    fn gap_description(&self) -> String {
        if self.positive_count > 0 && self.negative_count > 0 {
            format!(
                "{} has contradictory outcome evidence; replay or review before promoting a procedural rule.",
                self.topic
            )
        } else if self.total_count() < 3 {
            format!(
                "{} has only {} outcome observation(s); gather more evidence before promotion.",
                self.topic,
                self.total_count()
            )
        } else if self.negative_count > 0 || self.decay_count > 0 {
            format!(
                "{} has harmful, stale, or contradictory feedback that needs procedural review.",
                self.topic
            )
        } else {
            format!(
                "{} has repeated supportive outcomes and is ready for a candidate procedural rule.",
                self.topic
            )
        }
    }

    fn content_preview(&self) -> String {
        self.content_previews
            .iter()
            .next()
            .cloned()
            .unwrap_or_else(|| format!("Outcome observations for {}.", self.topic))
    }

    fn source(&self) -> String {
        if self.source_types.is_empty() {
            "feedback_event".to_string()
        } else {
            self.source_types
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        }
    }

    fn proposal_hypothesis(&self) -> String {
        if self.positive_count > 0 && self.negative_count > 0 {
            format!(
                "A dry-run comparison can separate valid {} guidance from contradicted cases.",
                self.topic
            )
        } else {
            format!(
                "The repeated {} outcomes can be consolidated into a durable procedural rule.",
                self.topic
            )
        }
    }

    fn current_decision(&self) -> String {
        format!(
            "Keep {} guidance at confidence {:.3} until the evidence cluster is reviewed.",
            self.topic,
            self.confidence()
        )
    }

    fn possible_change(&self) -> String {
        if self.negative_count > self.positive_count {
            "Demote or quarantine the candidate rule if replay confirms harmful outcomes."
                .to_string()
        } else {
            "Promote a candidate procedural rule through ee curate candidates.".to_string()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignalBucket {
    Positive,
    Negative,
    Neutral,
    Decay,
}

fn load_learning_clusters(
    workspace: &Path,
    topic_filter: Option<&str>,
) -> Result<Vec<LearningCluster>, DomainError> {
    let snapshot = load_learning_snapshot(workspace)?;
    let events = snapshot.feedback_events.clone();
    Ok(build_learning_clusters(&snapshot, topic_filter, &events))
}

fn load_learning_snapshot(workspace: &Path) -> Result<LearningSnapshot, DomainError> {
    let database_path = learning_database_path(None, workspace);
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_string()),
        });
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    let normalized_workspace = normalize_workspace_path(workspace);
    let workspace_path_text = normalized_workspace.to_string_lossy().into_owned();
    let workspace_id = connection
        .get_workspace_by_path(&workspace_path_text)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
        .map_or_else(
            || stable_workspace_id(&workspace_path_text),
            |workspace| workspace.id,
        );
    let memory_rows = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list learning memories: {error}"),
            repair: Some("ee remember --workspace . --json".to_string()),
        })?;
    let memories = memory_rows
        .into_iter()
        .map(|memory| (memory.id.clone(), memory))
        .collect::<BTreeMap<_, _>>();
    let memory_ids = memories.keys().map(String::as_str).collect::<Vec<_>>();
    let memory_tags = connection
        .get_memory_tags_batch(&memory_ids)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load memory tags for learning: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    let feedback_events = connection
        .list_feedback_events(&workspace_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list learning feedback events: {error}"),
            repair: Some("ee outcome list --json".to_string()),
        })?;
    let learning_observations = connection
        .list_learning_observations(&workspace_id, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list learning observations: {error}"),
            repair: Some("ee learn observe <experiment-id> --json".to_string()),
        })?;
    let curation_candidates = connection
        .list_curation_candidates(&workspace_id, None, None, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list curation candidates: {error}"),
            repair: Some("ee curate candidates --json".to_string()),
        })?;
    connection.close().map_err(|error| DomainError::Storage {
        message: format!("Failed to close database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;

    Ok(LearningSnapshot {
        workspace_id,
        memories,
        memory_tags,
        feedback_events,
        learning_observations,
        curation_candidates,
    })
}

fn persist_learning_observation(
    database_path: &Path,
    workspace_id: &str,
    input: &LearningObservationLedgerInput<'_>,
) -> Result<(), DomainError> {
    let connection =
        DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    let observation_id = stable_learning_observation_id(input);
    connection
        .insert_learning_observation(
            &observation_id,
            &CreateLearningObservationInput {
                workspace_id: workspace_id.to_string(),
                observation_kind: input.observation_kind.to_string(),
                source_type: input.source_type.to_string(),
                source_id: input.source_id.clone(),
                target_type: input.target_type.to_string(),
                target_id: input.target_id.to_string(),
                topic: input.topic.clone(),
                signal: input.signal.to_string(),
                evidence_json: input.evidence_json.clone(),
                observed_at: input.observed_at.to_string(),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to insert learning observation: {error}"),
            repair: Some("ee learn summary --json".to_string()),
        })?;
    connection.close().map_err(|error| DomainError::Storage {
        message: format!("Failed to close database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;
    Ok(())
}

fn stable_learning_observation_id(input: &LearningObservationLedgerInput<'_>) -> String {
    let payload = serde_json::json!({
        "kind": input.observation_kind,
        "sourceType": input.source_type,
        "sourceId": &input.source_id,
        "targetType": input.target_type,
        "targetId": input.target_id,
        "observedAt": input.observed_at,
    });
    format!(
        "lobs_{}",
        blake3::hash(payload.to_string().as_bytes())
            .to_hex()
            .chars()
            .take(32)
            .collect::<String>()
    )
}

fn build_learning_clusters(
    snapshot: &LearningSnapshot,
    topic_filter: Option<&str>,
    events: &[StoredFeedbackEvent],
) -> Vec<LearningCluster> {
    let normalized_filter = topic_filter.map(normalize_topic);
    let mut clusters = BTreeMap::new();
    for event in events {
        let evidence_ids = evidence_ids_for_event(event);
        let memory_ids = memory_ids_for_event(snapshot, event, &evidence_ids);
        let topic = topic_for_event(snapshot, event, &memory_ids);
        if normalized_filter
            .as_ref()
            .is_some_and(|filter| &topic != filter)
        {
            continue;
        }
        let previews = previews_for_memories(snapshot, &memory_ids, event);
        clusters
            .entry(topic.clone())
            .or_insert_with(|| LearningCluster::new(topic))
            .record(event, evidence_ids, memory_ids, previews);
    }

    clusters.into_values().collect()
}

fn evidence_ids_for_event(event: &StoredFeedbackEvent) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    ids.insert(event.target_id.clone());
    if let Some(source_id) = &event.source_id {
        ids.insert(source_id.clone());
    }
    if let Some(evidence_json) = &event.evidence_json {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence_json) {
            collect_evidence_ids(&value, &mut ids);
        }
    }
    ids
}

fn collect_evidence_ids(value: &serde_json::Value, ids: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_evidence_ids(value, ids);
            }
        }
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if matches!(
                    key.as_str(),
                    "evidenceIds"
                        | "promotedArtifactIds"
                        | "demotedArtifactIds"
                        | "targetArtifactIds"
                        | "auditIds"
                ) {
                    if let Some(values) = value.as_array() {
                        ids.extend(
                            values
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_string),
                        );
                    }
                }
                collect_evidence_ids(value, ids);
            }
        }
        _ => {}
    }
}

fn memory_ids_for_event(
    snapshot: &LearningSnapshot,
    event: &StoredFeedbackEvent,
    evidence_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut memory_ids = BTreeSet::new();
    if event.target_type == "memory" && snapshot.memories.contains_key(&event.target_id) {
        memory_ids.insert(event.target_id.clone());
    }
    memory_ids.extend(
        evidence_ids
            .iter()
            .filter(|id| snapshot.memories.contains_key(id.as_str()))
            .cloned(),
    );
    memory_ids
}

fn topic_for_event(
    snapshot: &LearningSnapshot,
    event: &StoredFeedbackEvent,
    memory_ids: &BTreeSet<String>,
) -> String {
    memory_ids
        .iter()
        .filter_map(|memory_id| {
            snapshot
                .memory_tags
                .get(memory_id)
                .and_then(|tags| tags.iter().find(|tag| !tag.trim().is_empty()))
                .cloned()
                .or_else(|| {
                    snapshot
                        .memories
                        .get(memory_id)
                        .map(|memory| memory.kind.clone())
                })
        })
        .map(|topic| normalize_topic(&topic))
        .find(|topic| topic != "general")
        .unwrap_or_else(|| normalize_topic(&event.target_type))
}

fn previews_for_memories(
    snapshot: &LearningSnapshot,
    memory_ids: &BTreeSet<String>,
    event: &StoredFeedbackEvent,
) -> BTreeSet<String> {
    let mut previews = memory_ids
        .iter()
        .filter_map(|memory_id| snapshot.memories.get(memory_id))
        .map(|memory| preview_text(&memory.content, 120))
        .collect::<BTreeSet<_>>();
    if previews.is_empty() {
        if let Some(reason) = &event.reason {
            previews.insert(preview_text(reason, 120));
        }
    }
    previews
}

fn preview_text(raw: &str, max_chars: usize) -> String {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        format!(
            "{}...",
            normalized
                .chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn normalize_topic(raw: &str) -> String {
    let mut topic = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    while topic.contains("__") {
        topic = topic.replace("__", "_");
    }
    let topic = topic.trim_matches('_').to_string();
    if topic.is_empty() {
        "general".to_string()
    } else {
        topic
    }
}

fn signal_bucket(signal: &str) -> SignalBucket {
    match signal {
        "positive" | "helpful" | "confirmation" => SignalBucket::Positive,
        "negative" | "harmful" | "contradiction" | "inaccurate" => SignalBucket::Negative,
        "stale" | "outdated" => SignalBucket::Decay,
        _ => SignalBucket::Neutral,
    }
}

fn is_negative_signal(signal: &str) -> bool {
    matches!(
        signal,
        "negative" | "harmful" | "contradiction" | "inaccurate" | "outdated" | "stale"
    )
}

fn feedback_impact(signal: &str) -> &'static str {
    match signal_bucket(signal) {
        SignalBucket::Positive => "promotes confidence",
        SignalBucket::Negative => "requires review",
        SignalBucket::Decay => "lowers freshness",
        SignalBucket::Neutral => "adds evidence",
    }
}

fn stable_suffix(namespace: &str, value: &str, len: usize) -> String {
    let hash = blake3::hash(format!("{namespace}:{value}").as_bytes());
    hash.to_hex().chars().take(len).collect()
}

fn budget_class(attention_tokens: u32, runtime_seconds: u32) -> &'static str {
    if attention_tokens <= 600 && runtime_seconds <= 120 {
        "small"
    } else if attention_tokens <= 1_500 && runtime_seconds <= 600 {
        "medium"
    } else {
        "large"
    }
}

fn safety_plan(boundary: ExperimentSafetyBoundary) -> ExperimentSafetyPlan {
    let boundary_name = boundary.as_str().to_string();
    let mutation_allowed = false;
    let review_required = matches!(
        boundary,
        ExperimentSafetyBoundary::AskBeforeActing | ExperimentSafetyBoundary::HumanReview
    );
    let denied_reasons = if boundary == ExperimentSafetyBoundary::Denied {
        vec!["Safety boundary denies experiment execution.".to_string()]
    } else {
        Vec::new()
    };

    ExperimentSafetyPlan {
        boundary: boundary_name,
        dry_run_first: true,
        mutation_allowed,
        review_required,
        stop_conditions: vec![
            "Stop after the replay produces a pass/fail explanation or safety finding.".to_string(),
            "Stop before any durable memory mutation; close with observe/close evidence first."
                .to_string(),
        ],
        denied_reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateFeedbackEventInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection,
    };

    type TestResult = Result<(), String>;

    fn seed_learning_database(prefix: &str) -> Result<(tempfile::TempDir, PathBuf), String> {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| error.to_string())?;
        let database = dir.path().join(".ee").join("ee.db");
        if let Some(parent) = database.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
        Ok((dir, database))
    }

    fn seed_learning_workspace(
        prefix: &str,
    ) -> Result<(tempfile::TempDir, PathBuf, String), String> {
        let (dir, database) = seed_learning_database(prefix)?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_path = dir.path().to_string_lossy().into_owned();
        let workspace_id = stable_workspace_id(&workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path,
                    name: Some(prefix.to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
        Ok((dir, database, workspace_id))
    }

    fn assert_experiment_registry_unavailable<T>(result: Result<T, DomainError>) -> TestResult {
        match result {
            Err(DomainError::UnsatisfiedDegradedMode { message, repair }) => {
                assert_eq!(message, EXPERIMENT_REGISTRY_UNAVAILABLE_MESSAGE);
                assert_eq!(
                    repair.as_deref(),
                    Some(EXPERIMENT_REGISTRY_UNAVAILABLE_REPAIR)
                );
                Ok(())
            }
            Err(error) => Err(format!(
                "expected unsatisfied degraded mode for experiment registry, got {}",
                error.code()
            )),
            Ok(_) => Err(
                "expected unsatisfied degraded mode for experiment registry, got success"
                    .to_string(),
            ),
        }
    }

    fn seed_memory(
        connection: &DbConnection,
        workspace_id: &str,
        id: &str,
        tag: &str,
        content: &str,
    ) -> TestResult {
        connection
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_string(),
                    level: "episodic".to_string(),
                    kind: "procedure".to_string(),
                    content: content.to_string(),
                    workflow_id: None,
                    confidence: 0.5,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some(format!("test://{id}")),
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                    tags: vec![tag.to_string()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn seed_feedback(
        connection: &DbConnection,
        workspace_id: &str,
        id: &str,
        memory_id: &str,
        signal: &str,
    ) -> TestResult {
        connection
            .insert_feedback_event(
                id,
                &CreateFeedbackEventInput {
                    workspace_id: workspace_id.to_string(),
                    target_type: "memory".to_string(),
                    target_id: memory_id.to_string(),
                    signal: signal.to_string(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_string(),
                    source_id: Some(format!("outcome_{id}")),
                    reason: Some(format!("{signal} outcome for {memory_id}")),
                    evidence_json: Some(
                        serde_json::json!({
                            "evidenceIds": [memory_id],
                            "status": signal,
                        })
                        .to_string(),
                    ),
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    #[test]
    fn agenda_empty_ledger_returns_empty_report() -> TestResult {
        let (dir, _database, _workspace_id) = seed_learning_workspace("ee-learn-empty")?;
        let report = show_agenda(&LearnAgendaOptions {
            workspace: dir.path().to_path_buf(),
            limit: 10,
            include_resolved: false,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, LEARN_AGENDA_SCHEMA_V1);
        assert!(report.items.is_empty());
        assert_eq!(report.total_gaps, 0);
        Ok(())
    }

    #[test]
    fn agenda_clusters_single_observation_with_sample_ids() -> TestResult {
        let (dir, database, workspace_id) = seed_learning_workspace("ee-learn-single")?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        seed_memory(
            &connection,
            &workspace_id,
            "mem_11234567890123456789012345",
            "testing",
            "Run the database contract fixture before promoting test guidance.",
        )?;
        seed_feedback(
            &connection,
            &workspace_id,
            "fb_11234567890123456789012345",
            "mem_11234567890123456789012345",
            "confirmation",
        )?;
        connection.close().map_err(|error| error.to_string())?;

        let report = show_agenda(&LearnAgendaOptions {
            workspace: dir.path().to_path_buf(),
            limit: 10,
            include_resolved: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.topic, "testing");
        assert!(
            item.sample_ids
                .contains(&"fb_11234567890123456789012345".to_string())
        );
        assert!(
            item.sample_ids
                .contains(&"mem_11234567890123456789012345".to_string())
        );
        assert!(item.uncertainty >= 0.7);
        Ok(())
    }

    #[test]
    fn uncertainty_detects_contradictory_observations() -> TestResult {
        let (dir, database, workspace_id) = seed_learning_workspace("ee-learn-contradict")?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        seed_memory(
            &connection,
            &workspace_id,
            "mem_21234567890123456789012345",
            "review",
            "Promote review-session candidates only after evidence aggregation.",
        )?;
        seed_feedback(
            &connection,
            &workspace_id,
            "fb_21234567890123456789012345",
            "mem_21234567890123456789012345",
            "confirmation",
        )?;
        seed_feedback(
            &connection,
            &workspace_id,
            "fb_22234567890123456789012345",
            "mem_21234567890123456789012345",
            "contradiction",
        )?;
        connection.close().map_err(|error| error.to_string())?;

        let report = show_uncertainty(&LearnUncertaintyOptions {
            workspace: dir.path().to_path_buf(),
            limit: 10,
            min_uncertainty: 0.0,
            kind: Some("review".to_string()),
            low_confidence: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.items.len(), 1);
        assert!(report.items[0].uncertainty >= 0.6);
        assert!(report.items[0].confidence < 0.6);
        Ok(())
    }

    #[test]
    fn summary_aggregates_learning_observations_and_candidates() -> TestResult {
        let (dir, database, workspace_id) = seed_learning_workspace("ee-learn-summary")?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        seed_memory(
            &connection,
            &workspace_id,
            "mem_31234567890123456789012345",
            "summary",
            "Summarize learning signals from feedback and curation rows.",
        )?;
        seed_feedback(
            &connection,
            &workspace_id,
            "fb_31234567890123456789012345",
            "mem_31234567890123456789012345",
            "helpful",
        )?;
        seed_feedback(
            &connection,
            &workspace_id,
            "fb_32234567890123456789012345",
            "mem_31234567890123456789012345",
            "harmful",
        )?;
        connection.close().map_err(|error| error.to_string())?;

        let report = show_summary(&LearnSummaryOptions {
            workspace: dir.path().to_path_buf(),
            period: "all".to_string(),
            since: None,
            detailed: true,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.summary.observations_recorded, 2);
        assert_eq!(report.summary.harmful_feedback_count, 1);
        assert_eq!(report.summary.gaps_identified, 1);
        assert_eq!(report.events.len(), 2);
        Ok(())
    }

    #[test]
    fn observe_experiment_dry_run_attaches_sorted_evidence_without_storage() -> TestResult {
        let report = observe_experiment(&LearnObserveOptions {
            workspace: PathBuf::from("/workspace"),
            database_path: None,
            workspace_id: None,
            experiment_id: "exp_database_contract_fixture".to_string(),
            observation_id: Some("lobs_test".to_string()),
            observed_at: Some("2026-01-01T00:00:00Z".to_string()),
            observer: Some("MistySalmon".to_string()),
            signal: LearningObservationSignal::Positive,
            measurement_name: "contract_status".to_string(),
            measurement_value: Some(1.0),
            evidence_ids: vec!["ev_b".to_string(), "ev_a".to_string(), "ev_a".to_string()],
            note: Some("Contract fixture passed.".to_string()),
            redaction_status: Some("redacted".to_string()),
            session_id: None,
            event_id: None,
            actor: Some("MistySalmon".to_string()),
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, LEARN_OBSERVE_SCHEMA_V1);
        assert_eq!(report.status, "dry_run");
        assert!(report.feedback.is_none());
        assert_eq!(
            report.observation.evidence_ids,
            vec!["ev_a".to_string(), "ev_b".to_string()]
        );
        let json = report.data_json();
        assert_eq!(json["observation"]["signal"], "positive");
        assert_eq!(json["observation"]["redactionStatus"], "redacted");
        Ok(())
    }

    #[test]
    fn observe_experiment_records_feedback_and_audit() -> TestResult {
        let (dir, database) = seed_learning_database("ee-learn-observe")?;
        let report = observe_experiment(&LearnObserveOptions {
            workspace: dir.path().to_path_buf(),
            database_path: Some(database.clone()),
            workspace_id: None,
            experiment_id: "exp_replay_error_boundary".to_string(),
            observation_id: Some("lobs_recorded".to_string()),
            observed_at: Some("2026-01-01T00:00:00Z".to_string()),
            observer: Some("MistySalmon".to_string()),
            signal: LearningObservationSignal::Safety,
            measurement_name: "safety_findings".to_string(),
            measurement_value: Some(1.0),
            evidence_ids: vec!["ev_safety".to_string()],
            note: Some("Dry-run found unsafe mutation risk.".to_string()),
            redaction_status: Some("redacted".to_string()),
            session_id: Some("sess_learn_observe".to_string()),
            event_id: Some("fb_22234567890123456789012345".to_string()),
            actor: Some("MistySalmon".to_string()),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.status, "observed");
        let feedback = report
            .feedback
            .as_ref()
            .ok_or_else(|| "feedback report must be present".to_string())?;
        assert_eq!(feedback.target_type, "candidate");
        assert_eq!(feedback.target_id, "exp_replay_error_boundary");
        assert_eq!(feedback.signal, "harmful");
        assert!(feedback.evidence_json_present);
        assert!(feedback.audit_id.is_some());

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let events = connection
            .list_feedback_events_for_target("candidate", "exp_replay_error_boundary")
            .map_err(|error| error.to_string())?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_id.as_deref(), Some("lobs_recorded"));
        let observations = connection
            .list_learning_observations(&feedback.workspace_id, None)
            .map_err(|error| error.to_string())?;
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].observation_kind, "experiment_observe");
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn close_experiment_dry_run_records_confirmed_outcome_shape() -> TestResult {
        let report = close_experiment(&LearnCloseOptions {
            workspace: PathBuf::from("/workspace"),
            database_path: None,
            workspace_id: None,
            experiment_id: "exp_database_contract_fixture".to_string(),
            outcome_id: Some("lout_confirmed".to_string()),
            closed_at: Some("2026-01-02T00:00:00Z".to_string()),
            status: ExperimentOutcomeStatus::Confirmed,
            decision_impact: "Promote database fixture guidance.".to_string(),
            confidence_delta: 0.25,
            priority_delta: -5,
            promoted_artifact_ids: vec!["mem_001".to_string()],
            demoted_artifact_ids: Vec::new(),
            safety_notes: vec!["No unsafe mutation observed.".to_string()],
            audit_ids: vec!["audit_manual".to_string()],
            session_id: None,
            event_id: None,
            actor: Some("MistySalmon".to_string()),
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, LEARN_CLOSE_SCHEMA_V1);
        assert_eq!(report.status, "dry_run");
        assert_eq!(report.outcome.status, ExperimentOutcomeStatus::Confirmed);
        assert_eq!(report.outcome.confidence_delta, 0.25);
        assert_eq!(
            report.outcome.promoted_artifact_ids,
            vec!["mem_001".to_string()]
        );
        assert_eq!(
            report.downstream_effects.mutation_mode,
            "dry_run_projection"
        );
        assert_eq!(
            report.downstream_effects.economy_score.confidence_delta,
            0.25
        );
        assert!(!report.downstream_effects.audit.durable_feedback_recorded);
        assert!(!report.downstream_effects.audit.silent_mutation);
        assert!(report.feedback.is_none());
        Ok(())
    }

    #[test]
    fn close_experiment_projects_downstream_effects_by_artifact_kind() -> TestResult {
        let report = close_experiment(&LearnCloseOptions {
            workspace: PathBuf::from("/workspace"),
            database_path: None,
            workspace_id: None,
            experiment_id: "exp_false_alarm_probe".to_string(),
            outcome_id: Some("lout_false_alarm".to_string()),
            closed_at: Some("2026-01-02T00:00:00Z".to_string()),
            status: ExperimentOutcomeStatus::Rejected,
            decision_impact: "Reject noisy tripwire and stale procedure evidence.".to_string(),
            confidence_delta: -0.3,
            priority_delta: 4,
            promoted_artifact_ids: vec!["mem_keep_001".to_string()],
            demoted_artifact_ids: vec![
                "tw_noisy_001".to_string(),
                "proc_release_001".to_string(),
                "sit_release_001".to_string(),
            ],
            safety_notes: Vec::new(),
            audit_ids: vec!["audit_false_alarm".to_string()],
            session_id: None,
            event_id: None,
            actor: Some("MistySalmon".to_string()),
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        let effects = report.downstream_effects;
        assert_eq!(effects.mutation_mode, "dry_run_projection");
        assert_eq!(
            effects.procedure_drift.procedure_artifact_ids,
            vec!["proc_release_001".to_string()]
        );
        assert_eq!(
            effects.procedure_drift.drift_signal,
            "contradicted_by_experiment"
        );
        assert!(effects.procedure_drift.requires_revalidation);
        assert_eq!(
            effects.tripwire_false_alarm.tripwire_artifact_ids,
            vec!["tw_noisy_001".to_string()]
        );
        assert_eq!(effects.tripwire_false_alarm.false_alarm_cost_delta, 1);
        assert_eq!(
            effects.situation_confidence.situation_artifact_ids,
            vec!["sit_release_001".to_string()]
        );
        assert_eq!(
            effects.situation_confidence.confidence_direction,
            "decrease"
        );
        assert!(!effects.audit.silent_mutation);
        Ok(())
    }

    #[test]
    fn close_experiment_records_rejected_outcome_feedback() -> TestResult {
        let (dir, database) = seed_learning_database("ee-learn-close")?;
        let report = close_experiment(&LearnCloseOptions {
            workspace: dir.path().to_path_buf(),
            database_path: Some(database.clone()),
            workspace_id: None,
            experiment_id: "exp_cli_validation_shadow".to_string(),
            outcome_id: Some("lout_rejected".to_string()),
            closed_at: Some("2026-01-02T00:00:00Z".to_string()),
            status: ExperimentOutcomeStatus::Rejected,
            decision_impact: "Reject promotion because shadow examples contradicted it."
                .to_string(),
            confidence_delta: -0.4,
            priority_delta: 10,
            promoted_artifact_ids: Vec::new(),
            demoted_artifact_ids: vec!["mem_003".to_string(), "tw_cli_noisy_001".to_string()],
            safety_notes: Vec::new(),
            audit_ids: Vec::new(),
            session_id: Some("sess_learn_close".to_string()),
            event_id: Some("fb_33234567890123456789012345".to_string()),
            actor: Some("MistySalmon".to_string()),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.status, "closed");
        let feedback = report
            .feedback
            .as_ref()
            .ok_or_else(|| "feedback report must be present".to_string())?;
        assert_eq!(feedback.signal, "contradiction");
        assert_eq!(feedback.source_type, "outcome_observed");
        assert_eq!(feedback.source_id.as_deref(), Some("lout_rejected"));
        assert_eq!(feedback.feedback.total_count, 1);
        assert!(report.data_json()["outcome"]["demotedArtifactIds"].is_array());
        assert_eq!(
            report
                .data_json()
                .pointer("/downstreamEffects/mutationMode"),
            Some(&serde_json::json!("audited_feedback_recorded"))
        );
        assert_eq!(
            report
                .data_json()
                .pointer("/downstreamEffects/tripwireFalseAlarm/falseAlarmCostDelta"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            report
                .data_json()
                .pointer("/downstreamEffects/audit/silentMutation"),
            Some(&serde_json::json!(false))
        );
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let observations = connection
            .list_learning_observations(&feedback.workspace_id, None)
            .map_err(|error| error.to_string())?;
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].observation_kind, "experiment_close");
        connection.close().map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn close_experiment_rejects_out_of_range_confidence_delta() -> TestResult {
        let result = close_experiment(&LearnCloseOptions {
            workspace: PathBuf::from("/workspace"),
            database_path: None,
            workspace_id: None,
            experiment_id: "exp_cli_validation_shadow".to_string(),
            outcome_id: Some("lout_bad".to_string()),
            closed_at: Some("2026-01-02T00:00:00Z".to_string()),
            status: ExperimentOutcomeStatus::Unsafe,
            decision_impact: "Unsafe result.".to_string(),
            confidence_delta: 1.5,
            priority_delta: 0,
            promoted_artifact_ids: Vec::new(),
            demoted_artifact_ids: Vec::new(),
            safety_notes: vec!["Unsafe mutation risk.".to_string()],
            audit_ids: Vec::new(),
            session_id: None,
            event_id: None,
            actor: None,
            dry_run: true,
        });

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn learn_experiment_proposals_persist_deterministic_rule_candidates() -> TestResult {
        let (dir, database, workspace_id) = seed_learning_workspace("ee-learn-propose")?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        for index in 0..50 {
            let memory_id = format!("mem_{index:026}");
            let feedback_id = format!("fb_{index:026}");
            seed_memory(
                &connection,
                &workspace_id,
                &memory_id,
                "large_cluster",
                "Use RCH with an isolated Cargo target directory before closing shared Rust beads.",
            )?;
            seed_feedback(
                &connection,
                &workspace_id,
                &feedback_id,
                &memory_id,
                "confirmation",
            )?;
        }
        connection.close().map_err(|error| error.to_string())?;

        let options = LearnExperimentProposeOptions {
            workspace: dir.path().to_path_buf(),
            limit: 5,
            topic: Some("large_cluster".to_string()),
            min_expected_value: 0.0,
            max_attention_tokens: 900,
            max_runtime_seconds: 180,
            safety_boundary: ExperimentSafetyBoundary::HumanReview,
        };
        let first = propose_experiments(&options).map_err(|error| error.message())?;
        let second = propose_experiments(&options).map_err(|error| error.message())?;

        assert_eq!(first.proposals.len(), 1);
        assert_eq!(
            first.proposals[0].experiment_id,
            second.proposals[0].experiment_id
        );
        assert!(first.proposals[0].evidence_ids.len() >= 2);
        assert_eq!(first.proposals[0].topic, "large_cluster");

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let candidates = connection
            .list_curation_candidates(&workspace_id, Some("rule"), Some("pending"), None)
            .map_err(|error| error.to_string())?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source_type, "feedback_event");
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn learn_experiment_run_abstains_until_backed_by_registry() -> TestResult {
        assert_experiment_registry_unavailable(run_experiment(&LearnExperimentRunOptions {
            experiment_id: "exp_database_contract_fixture".to_string(),
            max_attention_tokens: 600,
            max_runtime_seconds: 90,
            dry_run: true,
            ..Default::default()
        }))
    }

    #[test]
    fn learn_experiment_run_abstains_for_non_dry_run() -> TestResult {
        assert_experiment_registry_unavailable(run_experiment(&LearnExperimentRunOptions {
            experiment_id: "exp_database_contract_fixture".to_string(),
            dry_run: false,
            ..Default::default()
        }))
    }

    #[test]
    fn learn_experiment_run_abstains_for_all_experiment_ids() -> TestResult {
        let experiment_ids = [
            "exp_replay_error_boundary",
            "exp_database_contract_fixture",
            "exp_cli_validation_shadow",
            "exp_shadow_budget_probe",
            "exp_unknown_experiment",
        ];

        for experiment_id in experiment_ids {
            assert_experiment_registry_unavailable(run_experiment(&LearnExperimentRunOptions {
                experiment_id: experiment_id.to_string(),
                dry_run: true,
                ..Default::default()
            }))?;
        }
        Ok(())
    }
}
