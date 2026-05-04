//! Active learning agenda and uncertainty operations (EE-441).
//!
//! Provides agenda, uncertainty, and summary operations for identifying
//! knowledge gaps and prioritizing learning opportunities.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::outcome::{OutcomeRecordOptions, OutcomeRecordReport, record_outcome};
use crate::db::{CreateWorkspaceInput, DbConnection};
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

const LEARNING_RECORDS_UNAVAILABLE_MESSAGE: &str = "Learning agenda, uncertainty, summary, and experiment proposal records are unavailable until learn commands read persisted observation and evaluation ledgers instead of seed data.";
const LEARNING_RECORDS_UNAVAILABLE_REPAIR: &str =
    "ee learn observe <experiment-id> --dry-run --json";

fn learning_records_unavailable() -> DomainError {
    DomainError::UnsatisfiedDegradedMode {
        message: LEARNING_RECORDS_UNAVAILABLE_MESSAGE.to_string(),
        repair: Some(LEARNING_RECORDS_UNAVAILABLE_REPAIR.to_string()),
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
pub fn show_agenda(_options: &LearnAgendaOptions) -> Result<LearnAgendaReport, DomainError> {
    Err(learning_records_unavailable())
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
    _options: &LearnUncertaintyOptions,
) -> Result<LearnUncertaintyReport, DomainError> {
    Err(learning_records_unavailable())
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
pub fn show_summary(_options: &LearnSummaryOptions) -> Result<LearnSummaryReport, DomainError> {
    Err(learning_records_unavailable())
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
        workspace_id: Some(workspace_id),
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
        workspace_id: Some(workspace_id),
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
    _options: &LearnExperimentProposeOptions,
) -> Result<LearnExperimentProposalReport, DomainError> {
    Err(learning_records_unavailable())
}

/// Run a deterministic dry-run-only active learning experiment rehearsal.
pub fn run_experiment(
    options: &LearnExperimentRunOptions,
) -> Result<LearnExperimentRunReport, DomainError> {
    let generated_at = Utc::now().to_rfc3339();
    if !options.dry_run {
        return Err(DomainError::PolicyDenied {
            message: "Learning experiment execution requires --dry-run in this slice.".to_string(),
            repair: Some(
                "Use ee learn experiment run --id <experiment-id> --dry-run --json".to_string(),
            ),
        });
    }
    let experiment_id = require_text(
        "experiment id",
        &options.experiment_id,
        "ee learn experiment run --id exp_database_contract_fixture --dry-run --json",
    )?;
    let template =
        experiment_run_template(&experiment_id).ok_or_else(|| DomainError::NotFound {
            resource: "learning experiment".to_string(),
            id: experiment_id.clone(),
            repair: Some(
                "Run ee learn experiment propose --json to list known experiments.".to_string(),
            ),
        })?;
    let planned_attention_tokens = template
        .requested_attention_tokens
        .min(options.max_attention_tokens);
    let planned_runtime_seconds = template
        .requested_runtime_seconds
        .min(options.max_runtime_seconds);
    let safety = safety_plan(
        ExperimentSafetyBoundary::DryRunOnly,
        template.stop_condition,
    );

    Ok(LearnExperimentRunReport {
        schema: LEARN_EXPERIMENT_RUN_SCHEMA_V1.to_owned(),
        status: "dry_run".to_string(),
        dry_run: true,
        experiment_id,
        experiment_kind: template.experiment_kind.to_string(),
        title: template.title.to_string(),
        hypothesis: template.hypothesis.to_string(),
        budget: ExperimentRunBudget {
            requested_attention_tokens: template.requested_attention_tokens,
            requested_runtime_seconds: template.requested_runtime_seconds,
            planned_attention_tokens,
            planned_runtime_seconds,
            shadow_budget_delta_tokens: template.shadow_budget_delta_tokens,
            budget_class: budget_class(planned_attention_tokens, planned_runtime_seconds)
                .to_string(),
        },
        safety,
        steps: template.steps,
        observations: template.observations,
        outcome_preview: template.outcome_preview,
        next_actions: template.next_actions,
        generated_at,
    })
}

#[derive(Clone, Debug, PartialEq)]
struct ExperimentRunTemplate {
    experiment_kind: &'static str,
    title: &'static str,
    hypothesis: &'static str,
    stop_condition: &'static str,
    requested_attention_tokens: u32,
    requested_runtime_seconds: u32,
    shadow_budget_delta_tokens: i32,
    steps: Vec<ExperimentRunStep>,
    observations: Vec<ExperimentRunObservationPreview>,
    outcome_preview: ExperimentRunOutcomePreview,
    next_actions: Vec<String>,
}

fn experiment_run_template(experiment_id: &str) -> Option<ExperimentRunTemplate> {
    match experiment_id {
        "exp_replay_error_boundary" => Some(ExperimentRunTemplate {
            experiment_kind: "fixture_replay",
            title: "Replay async error boundary failures",
            hypothesis: "A dry-run replay can distinguish missing propagation evidence from a weak procedural rule.",
            stop_condition: "Stop after the replay produces a pass/fail explanation or safety finding.",
            requested_attention_tokens: 900,
            requested_runtime_seconds: 240,
            shadow_budget_delta_tokens: -120,
            steps: vec![
                run_step(
                    1,
                    "load_fixture",
                    "Load replay fixture family for async error boundaries.",
                    "neutral",
                ),
                run_step(
                    2,
                    "replay_failure",
                    "Replay the fixture against the candidate procedural rule.",
                    "negative",
                ),
                run_step(
                    3,
                    "summarize_evidence",
                    "Produce observation and closure previews without writing storage.",
                    "negative",
                ),
            ],
            observations: vec![run_observation(
                LearningObservationSignal::Negative,
                "fixture_replay_pass",
                Some(0.0),
                &["gap_001", "mem_002", "fixture_async_error_boundary"],
                "Dry-run replay would contradict the candidate rule until more propagation evidence exists.",
            )],
            outcome_preview: run_outcome(
                ExperimentOutcomeStatus::Rejected,
                "Keep async error propagation guidance low-confidence until replay evidence passes.",
                -0.18,
                8,
                &[],
                &["mem_002"],
                &["No durable mutation performed; replay result is an observation preview."],
            ),
            next_actions: run_next_actions("exp_replay_error_boundary", "negative", "rejected"),
        }),
        "exp_database_contract_fixture" => Some(ExperimentRunTemplate {
            experiment_kind: "procedure_revalidation",
            title: "Run database-operation contract fixture",
            hypothesis: "A fixture-only dry run can show whether database-operation memories need stronger integration-test evidence.",
            stop_condition: "Stop after fixture output records stdout/stderr separation and mutation posture.",
            requested_attention_tokens: 700,
            requested_runtime_seconds: 180,
            shadow_budget_delta_tokens: -80,
            steps: vec![
                run_step(
                    1,
                    "select_fixture",
                    "Select deterministic database-operation contract fixture.",
                    "neutral",
                ),
                run_step(
                    2,
                    "revalidate_procedure",
                    "Revalidate the procedure against fixture stdout/stderr contracts.",
                    "positive",
                ),
                run_step(
                    3,
                    "score_decision",
                    "Estimate decision impact and confidence delta without storage writes.",
                    "positive",
                ),
            ],
            observations: vec![run_observation(
                LearningObservationSignal::Positive,
                "procedure_revalidation_pass",
                Some(1.0),
                &["gap_002", "mem_001", "fixture_database_contract"],
                "Dry-run procedure revalidation would support stronger database-operation guidance.",
            )],
            outcome_preview: run_outcome(
                ExperimentOutcomeStatus::Confirmed,
                "Increase confidence for database-operation integration-test guidance.",
                0.22,
                -4,
                &["mem_001"],
                &[],
                &["Dry-run only; promote through learn close after reviewing fixture evidence."],
            ),
            next_actions: run_next_actions(
                "exp_database_contract_fixture",
                "positive",
                "confirmed",
            ),
        }),
        "exp_cli_validation_shadow" => Some(ExperimentRunTemplate {
            experiment_kind: "classifier_disambiguation",
            title: "Shadow CLI validation examples",
            hypothesis: "Shadowing invalid arguments can clarify whether CLI validation rules are resolved or still ambiguous.",
            stop_condition: "Stop after two invalid argument examples produce stable repair text.",
            requested_attention_tokens: 500,
            requested_runtime_seconds: 120,
            shadow_budget_delta_tokens: -40,
            steps: vec![
                run_step(
                    1,
                    "select_shadow_cases",
                    "Select deterministic invalid-argument examples.",
                    "neutral",
                ),
                run_step(
                    2,
                    "compare_repairs",
                    "Compare repair text across classifier outputs.",
                    "neutral",
                ),
                run_step(
                    3,
                    "classify_resolution",
                    "Decide whether the learning gap stays resolved.",
                    "neutral",
                ),
            ],
            observations: vec![run_observation(
                LearningObservationSignal::Neutral,
                "classifier_disambiguation_stable",
                Some(1.0),
                &["gap_003", "mem_003", "fixture_cli_validation"],
                "Dry-run classifier disambiguation would keep the validation convention resolved.",
            )],
            outcome_preview: run_outcome(
                ExperimentOutcomeStatus::Inconclusive,
                "Keep CLI validation convention resolved but do not promote new evidence yet.",
                0.0,
                0,
                &[],
                &[],
                &["No unsafe mutation or ambiguous repair text observed in dry run."],
            ),
            next_actions: run_next_actions("exp_cli_validation_shadow", "neutral", "inconclusive"),
        }),
        "exp_shadow_budget_probe" => Some(ExperimentRunTemplate {
            experiment_kind: "shadow_budget",
            title: "Shadow attention budget against selected pack decisions",
            hypothesis: "A budget shadow can show whether lower attention cost preserves the same learning decision.",
            stop_condition: "Stop after the shadow budget either preserves or changes the selected decision.",
            requested_attention_tokens: 600,
            requested_runtime_seconds: 120,
            shadow_budget_delta_tokens: -240,
            steps: vec![
                run_step(
                    1,
                    "load_budget_fixture",
                    "Load deterministic attention-budget fixture.",
                    "neutral",
                ),
                run_step(
                    2,
                    "simulate_shadow_budget",
                    "Compare baseline and reduced token budgets.",
                    "positive",
                ),
                run_step(
                    3,
                    "explain_delta",
                    "Explain selected decision delta and risk reserve.",
                    "positive",
                ),
            ],
            observations: vec![run_observation(
                LearningObservationSignal::Positive,
                "shadow_budget_preserved_decision",
                Some(1.0),
                &["economy_budget_fixture", "decision_shadow_budget"],
                "Dry-run shadow budget would preserve the decision while reducing token cost.",
            )],
            outcome_preview: run_outcome(
                ExperimentOutcomeStatus::Confirmed,
                "Prefer the lower-cost context budget for this fixture family.",
                0.12,
                -2,
                &["decision_shadow_budget"],
                &[],
                &["Shadow budget is only a dry-run recommendation; no policy was changed."],
            ),
            next_actions: run_next_actions("exp_shadow_budget_probe", "positive", "confirmed"),
        }),
        _ => None,
    }
}

fn run_step(
    order: u32,
    name: impl Into<String>,
    action: impl Into<String>,
    expected_signal: impl Into<String>,
) -> ExperimentRunStep {
    ExperimentRunStep {
        order,
        name: name.into(),
        action: action.into(),
        expected_signal: expected_signal.into(),
        writes_storage: false,
    }
}

fn run_observation(
    signal: LearningObservationSignal,
    measurement_name: impl Into<String>,
    measurement_value: Option<f64>,
    evidence_ids: &[&str],
    note: impl Into<String>,
) -> ExperimentRunObservationPreview {
    ExperimentRunObservationPreview {
        signal: signal.as_str().to_string(),
        measurement_name: measurement_name.into(),
        measurement_value: measurement_value.map(rounded_metric),
        evidence_ids: evidence_ids.iter().map(|id| (*id).to_string()).collect(),
        note: note.into(),
    }
}

fn run_outcome(
    status: ExperimentOutcomeStatus,
    decision_impact: impl Into<String>,
    confidence_delta: f64,
    priority_delta: i32,
    promoted_artifact_ids: &[&str],
    demoted_artifact_ids: &[&str],
    safety_notes: &[&str],
) -> ExperimentRunOutcomePreview {
    ExperimentRunOutcomePreview {
        status: status.as_str().to_string(),
        decision_impact: decision_impact.into(),
        confidence_delta: rounded_metric(confidence_delta),
        priority_delta,
        promoted_artifact_ids: promoted_artifact_ids
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
        demoted_artifact_ids: demoted_artifact_ids
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
        safety_notes: safety_notes
            .iter()
            .map(|note| (*note).to_string())
            .collect(),
    }
}

fn run_next_actions(experiment_id: &str, signal: &str, outcome_status: &str) -> Vec<String> {
    vec![
        format!(
            "ee learn observe {experiment_id} --signal {signal} --measurement-name <name> --dry-run --json"
        ),
        format!(
            "ee learn close {experiment_id} --status {outcome_status} --decision-impact <impact> --dry-run --json"
        ),
    ]
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

fn rounded_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConnection;

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

    fn assert_learning_records_unavailable<T>(result: Result<T, DomainError>) -> TestResult {
        match result {
            Err(DomainError::UnsatisfiedDegradedMode { message, repair }) => {
                assert_eq!(message, LEARNING_RECORDS_UNAVAILABLE_MESSAGE);
                assert_eq!(repair.as_deref(), Some(LEARNING_RECORDS_UNAVAILABLE_REPAIR));
                Ok(())
            }
            Err(error) => Err(format!(
                "expected unsatisfied degraded mode, got {}",
                error.code()
            )),
            Ok(_) => Err("expected unsatisfied degraded mode, got success".to_string()),
        }
    }

    #[test]
    fn agenda_reports_unavailable_until_backed_by_learning_records() -> TestResult {
        let options = LearnAgendaOptions {
            limit: 10,
            include_resolved: false,
            ..Default::default()
        };

        assert_learning_records_unavailable(show_agenda(&options))
    }

    #[test]
    fn uncertainty_reports_unavailable_until_backed_by_learning_records() -> TestResult {
        let options = LearnUncertaintyOptions {
            limit: 10,
            min_uncertainty: 0.5,
            ..Default::default()
        };

        assert_learning_records_unavailable(show_uncertainty(&options))
    }

    #[test]
    fn summary_reports_unavailable_until_backed_by_learning_records() -> TestResult {
        let options = LearnSummaryOptions {
            period: "week".to_owned(),
            detailed: true,
            ..Default::default()
        };

        assert_learning_records_unavailable(show_summary(&options))
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
    fn learn_experiment_proposals_report_unavailable_until_backed_by_records() -> TestResult {
        assert_learning_records_unavailable(propose_experiments(
            &LearnExperimentProposeOptions::default(),
        ))
    }

    #[test]
    fn learn_experiment_run_dry_run_covers_procedure_revalidation() -> TestResult {
        let report = run_experiment(&LearnExperimentRunOptions {
            experiment_id: "exp_database_contract_fixture".to_string(),
            max_attention_tokens: 600,
            max_runtime_seconds: 90,
            dry_run: true,
            ..Default::default()
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.schema, LEARN_EXPERIMENT_RUN_SCHEMA_V1);
        assert_eq!(report.status, "dry_run");
        assert!(report.dry_run);
        assert_eq!(report.experiment_kind, "procedure_revalidation");
        assert_eq!(report.budget.planned_attention_tokens, 600);
        assert_eq!(report.budget.planned_runtime_seconds, 90);
        assert!(report.steps.iter().all(|step| !step.writes_storage));
        assert_eq!(report.observations[0].signal, "positive");
        assert_eq!(report.outcome_preview.status, "confirmed");
        Ok(())
    }

    #[test]
    fn learn_experiment_run_rejects_non_dry_run() -> TestResult {
        let result = run_experiment(&LearnExperimentRunOptions {
            experiment_id: "exp_database_contract_fixture".to_string(),
            dry_run: false,
            ..Default::default()
        });

        assert!(matches!(result, Err(DomainError::PolicyDenied { .. })));
        Ok(())
    }

    #[test]
    fn learn_experiment_run_supports_seeded_kinds() -> TestResult {
        let cases = [
            ("exp_replay_error_boundary", "fixture_replay"),
            ("exp_database_contract_fixture", "procedure_revalidation"),
            ("exp_cli_validation_shadow", "classifier_disambiguation"),
            ("exp_shadow_budget_probe", "shadow_budget"),
        ];

        for (experiment_id, expected_kind) in cases {
            let report = run_experiment(&LearnExperimentRunOptions {
                experiment_id: experiment_id.to_string(),
                dry_run: true,
                ..Default::default()
            })
            .map_err(|e| e.message())?;
            assert_eq!(report.experiment_kind, expected_kind);
            assert!(!report.steps.is_empty());
            assert!(!report.observations.is_empty());
        }
        Ok(())
    }
}
