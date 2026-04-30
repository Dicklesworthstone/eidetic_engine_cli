//! Active learning schema contracts (EE-440).
//!
//! These records describe questions, uncertainty estimates, safe experiments,
//! observations, and outcomes. They are contracts for later `ee learn` command
//! implementations; defining them here does not run experiments or mutate memory.

use std::fmt;
use std::str::FromStr;

use serde_json::{Value as JsonValue, json};

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for an active learning question.
pub const LEARNING_QUESTION_SCHEMA_V1: &str = "ee.learning.question.v1";

/// Schema for an uncertainty estimate attached to a question target.
pub const UNCERTAINTY_ESTIMATE_SCHEMA_V1: &str = "ee.learning.uncertainty_estimate.v1";

/// Schema for a safe, dry-run-first learning experiment.
pub const LEARNING_EXPERIMENT_SCHEMA_V1: &str = "ee.learning.experiment.v1";

/// Schema for evidence observed during a learning experiment.
pub const LEARNING_OBSERVATION_SCHEMA_V1: &str = "ee.learning.observation.v1";

/// Schema for the closed outcome of a learning experiment.
pub const EXPERIMENT_OUTCOME_SCHEMA_V1: &str = "ee.learning.experiment_outcome.v1";

/// Schema for the active learning schema catalog.
pub const LEARNING_SCHEMA_CATALOG_V1: &str = "ee.learning.schemas.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

fn bounded_unit(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn rounded_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

// ============================================================================
// Stable Wire Enums
// ============================================================================

/// Lifecycle state for a learning question.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LearningQuestionStatus {
    Open,
    ReadyForExperiment,
    Resolved,
    Deferred,
}

impl LearningQuestionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::ReadyForExperiment => "ready_for_experiment",
            Self::Resolved => "resolved",
            Self::Deferred => "deferred",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Open,
            Self::ReadyForExperiment,
            Self::Resolved,
            Self::Deferred,
        ]
    }
}

impl fmt::Display for LearningQuestionStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LearningQuestionStatus {
    type Err = ParseLearningValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "open" => Ok(Self::Open),
            "ready_for_experiment" => Ok(Self::ReadyForExperiment),
            "resolved" => Ok(Self::Resolved),
            "deferred" => Ok(Self::Deferred),
            _ => Err(ParseLearningValueError::new(
                "learning_question_status",
                input,
                "open, ready_for_experiment, resolved, deferred",
            )),
        }
    }
}

/// Artifact category targeted by an uncertainty estimate or experiment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LearningTargetKind {
    Memory,
    Procedure,
    Tripwire,
    Situation,
    Economy,
    Decision,
}

impl LearningTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Procedure => "procedure",
            Self::Tripwire => "tripwire",
            Self::Situation => "situation",
            Self::Economy => "economy",
            Self::Decision => "decision",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Memory,
            Self::Procedure,
            Self::Tripwire,
            Self::Situation,
            Self::Economy,
            Self::Decision,
        ]
    }
}

impl fmt::Display for LearningTargetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LearningTargetKind {
    type Err = ParseLearningValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "memory" => Ok(Self::Memory),
            "procedure" => Ok(Self::Procedure),
            "tripwire" => Ok(Self::Tripwire),
            "situation" => Ok(Self::Situation),
            "economy" => Ok(Self::Economy),
            "decision" => Ok(Self::Decision),
            _ => Err(ParseLearningValueError::new(
                "learning_target_kind",
                input,
                "memory, procedure, tripwire, situation, economy, decision",
            )),
        }
    }
}

/// Lifecycle state for a proposed learning experiment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LearningExperimentStatus {
    Proposed,
    DryRunReady,
    Observing,
    Closed,
    Rejected,
}

impl LearningExperimentStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::DryRunReady => "dry_run_ready",
            Self::Observing => "observing",
            Self::Closed => "closed",
            Self::Rejected => "rejected",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Proposed,
            Self::DryRunReady,
            Self::Observing,
            Self::Closed,
            Self::Rejected,
        ]
    }
}

impl fmt::Display for LearningExperimentStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LearningExperimentStatus {
    type Err = ParseLearningValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "proposed" => Ok(Self::Proposed),
            "dry_run_ready" => Ok(Self::DryRunReady),
            "observing" => Ok(Self::Observing),
            "closed" => Ok(Self::Closed),
            "rejected" => Ok(Self::Rejected),
            _ => Err(ParseLearningValueError::new(
                "learning_experiment_status",
                input,
                "proposed, dry_run_ready, observing, closed, rejected",
            )),
        }
    }
}

/// Safety boundary that must be honored before an experiment can run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExperimentSafetyBoundary {
    DryRunOnly,
    AskBeforeActing,
    HumanReview,
    Denied,
}

impl ExperimentSafetyBoundary {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DryRunOnly => "dry_run_only",
            Self::AskBeforeActing => "ask_before_acting",
            Self::HumanReview => "human_review",
            Self::Denied => "denied",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::DryRunOnly,
            Self::AskBeforeActing,
            Self::HumanReview,
            Self::Denied,
        ]
    }
}

impl fmt::Display for ExperimentSafetyBoundary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ExperimentSafetyBoundary {
    type Err = ParseLearningValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "dry_run_only" => Ok(Self::DryRunOnly),
            "ask_before_acting" => Ok(Self::AskBeforeActing),
            "human_review" => Ok(Self::HumanReview),
            "denied" => Ok(Self::Denied),
            _ => Err(ParseLearningValueError::new(
                "experiment_safety_boundary",
                input,
                "dry_run_only, ask_before_acting, human_review, denied",
            )),
        }
    }
}

/// Direction of an observation collected during an experiment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LearningObservationSignal {
    Positive,
    Negative,
    Neutral,
    Safety,
}

impl LearningObservationSignal {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Neutral => "neutral",
            Self::Safety => "safety",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Positive, Self::Negative, Self::Neutral, Self::Safety]
    }
}

impl fmt::Display for LearningObservationSignal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LearningObservationSignal {
    type Err = ParseLearningValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "positive" => Ok(Self::Positive),
            "negative" => Ok(Self::Negative),
            "neutral" => Ok(Self::Neutral),
            "safety" => Ok(Self::Safety),
            _ => Err(ParseLearningValueError::new(
                "learning_observation_signal",
                input,
                "positive, negative, neutral, safety",
            )),
        }
    }
}

/// Closed result for a learning experiment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExperimentOutcomeStatus {
    Confirmed,
    Rejected,
    Inconclusive,
    Unsafe,
}

impl ExperimentOutcomeStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
            Self::Inconclusive => "inconclusive",
            Self::Unsafe => "unsafe",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Confirmed,
            Self::Rejected,
            Self::Inconclusive,
            Self::Unsafe,
        ]
    }
}

impl fmt::Display for ExperimentOutcomeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ExperimentOutcomeStatus {
    type Err = ParseLearningValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "confirmed" => Ok(Self::Confirmed),
            "rejected" => Ok(Self::Rejected),
            "inconclusive" => Ok(Self::Inconclusive),
            "unsafe" => Ok(Self::Unsafe),
            _ => Err(ParseLearningValueError::new(
                "experiment_outcome_status",
                input,
                "confirmed, rejected, inconclusive, unsafe",
            )),
        }
    }
}

/// Error returned when a stable learning wire value cannot be parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseLearningValueError {
    field: &'static str,
    value: String,
    expected: &'static str,
}

impl ParseLearningValueError {
    #[must_use]
    pub fn new(field: &'static str, value: impl Into<String>, expected: &'static str) -> Self {
        Self {
            field,
            value: value.into(),
            expected,
        }
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub const fn expected(&self) -> &'static str {
        self.expected
    }
}

impl fmt::Display for ParseLearningValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid {} value '{}'; expected one of: {}",
            self.field, self.value, self.expected
        )
    }
}

impl std::error::Error for ParseLearningValueError {}

// ============================================================================
// Domain Records
// ============================================================================

/// High-value uncertainty that could change a memory decision.
#[derive(Clone, Debug, PartialEq)]
pub struct LearningQuestion {
    pub schema: &'static str,
    pub question_id: String,
    pub topic: String,
    pub prompt: String,
    pub status: LearningQuestionStatus,
    pub target_artifact_ids: Vec<String>,
    pub decision_impact: String,
    pub priority: u8,
    pub uncertainty: f64,
    pub expected_value: f64,
    pub evidence_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl LearningQuestion {
    #[must_use]
    pub fn new(
        question_id: impl Into<String>,
        topic: impl Into<String>,
        prompt: impl Into<String>,
        decision_impact: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            schema: LEARNING_QUESTION_SCHEMA_V1,
            question_id: question_id.into(),
            topic: topic.into(),
            prompt: prompt.into(),
            status: LearningQuestionStatus::Open,
            target_artifact_ids: Vec::new(),
            decision_impact: decision_impact.into(),
            priority: 0,
            uncertainty: 0.5,
            expected_value: 0.0,
            evidence_ids: Vec::new(),
            updated_at: created_at.clone(),
            created_at,
        }
    }

    #[must_use]
    pub const fn with_status(mut self, status: LearningQuestionStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub fn with_target_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.target_artifact_ids.push(artifact_id.into());
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority.min(100);
        self
    }

    #[must_use]
    pub fn with_uncertainty(mut self, uncertainty: f64) -> Self {
        self.uncertainty = bounded_unit(uncertainty);
        self
    }

    #[must_use]
    pub fn with_expected_value(mut self, expected_value: f64) -> Self {
        self.expected_value = bounded_unit(expected_value);
        self
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "questionId": self.question_id,
            "topic": self.topic,
            "prompt": self.prompt,
            "status": self.status.as_str(),
            "targetArtifactIds": self.target_artifact_ids,
            "decisionImpact": self.decision_impact,
            "priority": self.priority,
            "uncertainty": rounded_metric(self.uncertainty),
            "expectedValue": rounded_metric(self.expected_value),
            "evidenceIds": self.evidence_ids,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

/// Quantified uncertainty estimate for a learning target.
#[derive(Clone, Debug, PartialEq)]
pub struct UncertaintyEstimate {
    pub schema: &'static str,
    pub estimate_id: String,
    pub question_id: String,
    pub target_id: String,
    pub target_kind: LearningTargetKind,
    pub uncertainty: f64,
    pub confidence: f64,
    pub sample_size: u32,
    pub method: String,
    pub evidence_ids: Vec<String>,
    pub estimated_at: String,
}

impl UncertaintyEstimate {
    #[must_use]
    pub fn new(
        estimate_id: impl Into<String>,
        question_id: impl Into<String>,
        target_id: impl Into<String>,
        target_kind: LearningTargetKind,
        method: impl Into<String>,
        estimated_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: UNCERTAINTY_ESTIMATE_SCHEMA_V1,
            estimate_id: estimate_id.into(),
            question_id: question_id.into(),
            target_id: target_id.into(),
            target_kind,
            uncertainty: 0.5,
            confidence: 0.5,
            sample_size: 0,
            method: method.into(),
            evidence_ids: Vec::new(),
            estimated_at: estimated_at.into(),
        }
    }

    #[must_use]
    pub fn with_uncertainty(mut self, uncertainty: f64) -> Self {
        self.uncertainty = bounded_unit(uncertainty);
        self
    }

    #[must_use]
    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = bounded_unit(confidence);
        self
    }

    #[must_use]
    pub const fn with_sample_size(mut self, sample_size: u32) -> Self {
        self.sample_size = sample_size;
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "estimateId": self.estimate_id,
            "questionId": self.question_id,
            "targetId": self.target_id,
            "targetKind": self.target_kind.as_str(),
            "uncertainty": rounded_metric(self.uncertainty),
            "confidence": rounded_metric(self.confidence),
            "sampleSize": self.sample_size,
            "method": self.method,
            "evidenceIds": self.evidence_ids,
            "estimatedAt": self.estimated_at,
        })
    }
}

/// Proposed experiment that can safely reduce uncertainty.
#[derive(Clone, Debug, PartialEq)]
pub struct LearningExperiment {
    pub schema: &'static str,
    pub experiment_id: String,
    pub question_id: String,
    pub title: String,
    pub hypothesis: String,
    pub status: LearningExperimentStatus,
    pub safety_boundary: ExperimentSafetyBoundary,
    pub expected_value: f64,
    pub attention_budget_tokens: u32,
    pub max_runtime_seconds: u32,
    pub dry_run_first: bool,
    pub stop_condition: String,
    pub affected_decision_ids: Vec<String>,
    pub proposed_at: String,
}

impl LearningExperiment {
    #[must_use]
    pub fn new(
        experiment_id: impl Into<String>,
        question_id: impl Into<String>,
        title: impl Into<String>,
        hypothesis: impl Into<String>,
        stop_condition: impl Into<String>,
        proposed_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: LEARNING_EXPERIMENT_SCHEMA_V1,
            experiment_id: experiment_id.into(),
            question_id: question_id.into(),
            title: title.into(),
            hypothesis: hypothesis.into(),
            status: LearningExperimentStatus::Proposed,
            safety_boundary: ExperimentSafetyBoundary::DryRunOnly,
            expected_value: 0.0,
            attention_budget_tokens: 0,
            max_runtime_seconds: 0,
            dry_run_first: true,
            stop_condition: stop_condition.into(),
            affected_decision_ids: Vec::new(),
            proposed_at: proposed_at.into(),
        }
    }

    #[must_use]
    pub const fn with_status(mut self, status: LearningExperimentStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub const fn with_safety_boundary(mut self, safety_boundary: ExperimentSafetyBoundary) -> Self {
        self.safety_boundary = safety_boundary;
        self
    }

    #[must_use]
    pub fn with_expected_value(mut self, expected_value: f64) -> Self {
        self.expected_value = bounded_unit(expected_value);
        self
    }

    #[must_use]
    pub const fn with_attention_budget_tokens(mut self, tokens: u32) -> Self {
        self.attention_budget_tokens = tokens;
        self
    }

    #[must_use]
    pub const fn with_max_runtime_seconds(mut self, seconds: u32) -> Self {
        self.max_runtime_seconds = seconds;
        self
    }

    #[must_use]
    pub const fn without_dry_run_first(mut self) -> Self {
        self.dry_run_first = false;
        self
    }

    #[must_use]
    pub fn with_affected_decision(mut self, decision_id: impl Into<String>) -> Self {
        self.affected_decision_ids.push(decision_id.into());
        self
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "experimentId": self.experiment_id,
            "questionId": self.question_id,
            "title": self.title,
            "hypothesis": self.hypothesis,
            "status": self.status.as_str(),
            "safetyBoundary": self.safety_boundary.as_str(),
            "expectedValue": rounded_metric(self.expected_value),
            "attentionBudgetTokens": self.attention_budget_tokens,
            "maxRuntimeSeconds": self.max_runtime_seconds,
            "dryRunFirst": self.dry_run_first,
            "stopCondition": self.stop_condition,
            "affectedDecisionIds": self.affected_decision_ids,
            "proposedAt": self.proposed_at,
        })
    }
}

/// Evidence captured while observing a learning experiment.
#[derive(Clone, Debug, PartialEq)]
pub struct LearningObservation {
    pub schema: &'static str,
    pub observation_id: String,
    pub experiment_id: String,
    pub observed_at: String,
    pub observer: String,
    pub signal: LearningObservationSignal,
    pub measurement_name: String,
    pub measurement_value: Option<f64>,
    pub evidence_ids: Vec<String>,
    pub note: Option<String>,
    pub redaction_status: String,
}

impl LearningObservation {
    #[must_use]
    pub fn new(
        observation_id: impl Into<String>,
        experiment_id: impl Into<String>,
        observed_at: impl Into<String>,
        observer: impl Into<String>,
        measurement_name: impl Into<String>,
    ) -> Self {
        Self {
            schema: LEARNING_OBSERVATION_SCHEMA_V1,
            observation_id: observation_id.into(),
            experiment_id: experiment_id.into(),
            observed_at: observed_at.into(),
            observer: observer.into(),
            signal: LearningObservationSignal::Neutral,
            measurement_name: measurement_name.into(),
            measurement_value: None,
            evidence_ids: Vec::new(),
            note: None,
            redaction_status: "not_required".to_owned(),
        }
    }

    #[must_use]
    pub const fn with_signal(mut self, signal: LearningObservationSignal) -> Self {
        self.signal = signal;
        self
    }

    #[must_use]
    pub fn with_measurement_value(mut self, value: f64) -> Self {
        self.measurement_value = Some(rounded_metric(value));
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    #[must_use]
    pub fn with_redaction_status(mut self, redaction_status: impl Into<String>) -> Self {
        self.redaction_status = redaction_status.into();
        self
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "observationId": self.observation_id,
            "experimentId": self.experiment_id,
            "observedAt": self.observed_at,
            "observer": self.observer,
            "signal": self.signal.as_str(),
            "measurementName": self.measurement_name,
            "measurementValue": self.measurement_value,
            "evidenceIds": self.evidence_ids,
            "note": self.note,
            "redactionStatus": self.redaction_status,
        })
    }
}

/// Final auditable outcome for a learning experiment.
#[derive(Clone, Debug, PartialEq)]
pub struct ExperimentOutcome {
    pub schema: &'static str,
    pub outcome_id: String,
    pub experiment_id: String,
    pub status: ExperimentOutcomeStatus,
    pub closed_at: String,
    pub decision_impact: String,
    pub confidence_delta: f64,
    pub priority_delta: i32,
    pub promoted_artifact_ids: Vec<String>,
    pub demoted_artifact_ids: Vec<String>,
    pub safety_notes: Vec<String>,
    pub audit_ids: Vec<String>,
}

impl ExperimentOutcome {
    #[must_use]
    pub fn new(
        outcome_id: impl Into<String>,
        experiment_id: impl Into<String>,
        closed_at: impl Into<String>,
        decision_impact: impl Into<String>,
    ) -> Self {
        Self {
            schema: EXPERIMENT_OUTCOME_SCHEMA_V1,
            outcome_id: outcome_id.into(),
            experiment_id: experiment_id.into(),
            status: ExperimentOutcomeStatus::Inconclusive,
            closed_at: closed_at.into(),
            decision_impact: decision_impact.into(),
            confidence_delta: 0.0,
            priority_delta: 0,
            promoted_artifact_ids: Vec::new(),
            demoted_artifact_ids: Vec::new(),
            safety_notes: Vec::new(),
            audit_ids: Vec::new(),
        }
    }

    #[must_use]
    pub const fn with_status(mut self, status: ExperimentOutcomeStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub fn with_confidence_delta(mut self, delta: f64) -> Self {
        self.confidence_delta = if delta.is_finite() {
            delta.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self
    }

    #[must_use]
    pub const fn with_priority_delta(mut self, delta: i32) -> Self {
        self.priority_delta = delta;
        self
    }

    #[must_use]
    pub fn with_promoted_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.promoted_artifact_ids.push(artifact_id.into());
        self
    }

    #[must_use]
    pub fn with_demoted_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.demoted_artifact_ids.push(artifact_id.into());
        self
    }

    #[must_use]
    pub fn with_safety_note(mut self, note: impl Into<String>) -> Self {
        self.safety_notes.push(note.into());
        self
    }

    #[must_use]
    pub fn with_audit_id(mut self, audit_id: impl Into<String>) -> Self {
        self.audit_ids.push(audit_id.into());
        self
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "outcomeId": self.outcome_id,
            "experimentId": self.experiment_id,
            "status": self.status.as_str(),
            "closedAt": self.closed_at,
            "decisionImpact": self.decision_impact,
            "confidenceDelta": rounded_metric(self.confidence_delta),
            "priorityDelta": self.priority_delta,
            "promotedArtifactIds": self.promoted_artifact_ids,
            "demotedArtifactIds": self.demoted_artifact_ids,
            "safetyNotes": self.safety_notes,
            "auditIds": self.audit_ids,
        })
    }
}

// ============================================================================
// Schema Catalog
// ============================================================================

/// Field descriptor used by the active learning schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LearningFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl LearningFieldSchema {
    #[must_use]
    pub const fn new(
        name: &'static str,
        type_name: &'static str,
        required: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            type_name,
            required,
            description,
        }
    }
}

/// Stable JSON-schema-like catalog entry for active learning records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LearningObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [LearningFieldSchema],
}

impl LearningObjectSchema {
    #[must_use]
    pub fn required_count(&self) -> usize {
        self.fields.iter().filter(|field| field.required).count()
    }
}

const LEARNING_QUESTION_FIELDS: &[LearningFieldSchema] = &[
    LearningFieldSchema::new("schema", "string", true, "Schema identifier."),
    LearningFieldSchema::new("questionId", "string", true, "Stable question identifier."),
    LearningFieldSchema::new("topic", "string", true, "Question topic or subsystem."),
    LearningFieldSchema::new("prompt", "string", true, "Concrete uncertainty to resolve."),
    LearningFieldSchema::new("status", "string", true, "Question lifecycle status."),
    LearningFieldSchema::new(
        "targetArtifactIds",
        "array<string>",
        true,
        "Artifacts whose treatment could change after resolving the question.",
    ),
    LearningFieldSchema::new(
        "decisionImpact",
        "string",
        true,
        "Decision that could change if uncertainty is reduced.",
    ),
    LearningFieldSchema::new("priority", "integer", true, "Priority from 0 to 100."),
    LearningFieldSchema::new(
        "uncertainty",
        "number",
        true,
        "Current uncertainty estimate from 0.0 to 1.0.",
    ),
    LearningFieldSchema::new(
        "expectedValue",
        "number",
        true,
        "Expected value of reducing the uncertainty from 0.0 to 1.0.",
    ),
    LearningFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence identifiers supporting the question.",
    ),
    LearningFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
    LearningFieldSchema::new("updatedAt", "string", true, "RFC 3339 update timestamp."),
];

const UNCERTAINTY_ESTIMATE_FIELDS: &[LearningFieldSchema] = &[
    LearningFieldSchema::new("schema", "string", true, "Schema identifier."),
    LearningFieldSchema::new("estimateId", "string", true, "Stable estimate identifier."),
    LearningFieldSchema::new(
        "questionId",
        "string",
        true,
        "Parent learning question identifier.",
    ),
    LearningFieldSchema::new(
        "targetId",
        "string",
        true,
        "Artifact or decision being estimated.",
    ),
    LearningFieldSchema::new("targetKind", "string", true, "Target artifact category."),
    LearningFieldSchema::new(
        "uncertainty",
        "number",
        true,
        "Uncertainty from 0.0 to 1.0.",
    ),
    LearningFieldSchema::new("confidence", "number", true, "Confidence from 0.0 to 1.0."),
    LearningFieldSchema::new(
        "sampleSize",
        "integer",
        true,
        "Number of observations or retrievals used.",
    ),
    LearningFieldSchema::new("method", "string", true, "Deterministic estimation method."),
    LearningFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence identifiers used by the estimate.",
    ),
    LearningFieldSchema::new(
        "estimatedAt",
        "string",
        true,
        "RFC 3339 estimate timestamp.",
    ),
];

const LEARNING_EXPERIMENT_FIELDS: &[LearningFieldSchema] = &[
    LearningFieldSchema::new("schema", "string", true, "Schema identifier."),
    LearningFieldSchema::new(
        "experimentId",
        "string",
        true,
        "Stable experiment identifier.",
    ),
    LearningFieldSchema::new(
        "questionId",
        "string",
        true,
        "Learning question identifier.",
    ),
    LearningFieldSchema::new("title", "string", true, "Short experiment title."),
    LearningFieldSchema::new(
        "hypothesis",
        "string",
        true,
        "Hypothesis the experiment can test.",
    ),
    LearningFieldSchema::new("status", "string", true, "Experiment lifecycle status."),
    LearningFieldSchema::new(
        "safetyBoundary",
        "string",
        true,
        "Boundary that must be honored before running the experiment.",
    ),
    LearningFieldSchema::new(
        "expectedValue",
        "number",
        true,
        "Expected value of the experiment from 0.0 to 1.0.",
    ),
    LearningFieldSchema::new(
        "attentionBudgetTokens",
        "integer",
        true,
        "Maximum attention budget the experiment may consume.",
    ),
    LearningFieldSchema::new(
        "maxRuntimeSeconds",
        "integer",
        true,
        "Maximum wall-clock runtime budget.",
    ),
    LearningFieldSchema::new(
        "dryRunFirst",
        "boolean",
        true,
        "Whether dry-run execution is required before mutation.",
    ),
    LearningFieldSchema::new(
        "stopCondition",
        "string",
        true,
        "Condition that stops the experiment.",
    ),
    LearningFieldSchema::new(
        "affectedDecisionIds",
        "array<string>",
        true,
        "Decision identifiers the experiment may influence.",
    ),
    LearningFieldSchema::new("proposedAt", "string", true, "RFC 3339 proposal timestamp."),
];

const LEARNING_OBSERVATION_FIELDS: &[LearningFieldSchema] = &[
    LearningFieldSchema::new("schema", "string", true, "Schema identifier."),
    LearningFieldSchema::new(
        "observationId",
        "string",
        true,
        "Stable observation identifier.",
    ),
    LearningFieldSchema::new(
        "experimentId",
        "string",
        true,
        "Learning experiment identifier.",
    ),
    LearningFieldSchema::new(
        "observedAt",
        "string",
        true,
        "RFC 3339 observation timestamp.",
    ),
    LearningFieldSchema::new(
        "observer",
        "string",
        true,
        "Agent, harness, or tool observer.",
    ),
    LearningFieldSchema::new("signal", "string", true, "Observation signal direction."),
    LearningFieldSchema::new("measurementName", "string", true, "Measured quantity name."),
    LearningFieldSchema::new(
        "measurementValue",
        "number|null",
        false,
        "Measured quantity value when numeric.",
    ),
    LearningFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence identifiers captured with the observation.",
    ),
    LearningFieldSchema::new(
        "note",
        "string|null",
        false,
        "Human or agent observation note.",
    ),
    LearningFieldSchema::new(
        "redactionStatus",
        "string",
        true,
        "Redaction state of evidence.",
    ),
];

const EXPERIMENT_OUTCOME_FIELDS: &[LearningFieldSchema] = &[
    LearningFieldSchema::new("schema", "string", true, "Schema identifier."),
    LearningFieldSchema::new("outcomeId", "string", true, "Stable outcome identifier."),
    LearningFieldSchema::new(
        "experimentId",
        "string",
        true,
        "Learning experiment identifier.",
    ),
    LearningFieldSchema::new("status", "string", true, "Outcome status."),
    LearningFieldSchema::new("closedAt", "string", true, "RFC 3339 closure timestamp."),
    LearningFieldSchema::new(
        "decisionImpact",
        "string",
        true,
        "Decision impact observed after closing the experiment.",
    ),
    LearningFieldSchema::new(
        "confidenceDelta",
        "number",
        true,
        "Change in confidence from -1.0 to 1.0.",
    ),
    LearningFieldSchema::new(
        "priorityDelta",
        "integer",
        true,
        "Change applied to future learning priority.",
    ),
    LearningFieldSchema::new(
        "promotedArtifactIds",
        "array<string>",
        true,
        "Artifacts confirmed by the outcome.",
    ),
    LearningFieldSchema::new(
        "demotedArtifactIds",
        "array<string>",
        true,
        "Artifacts weakened or rejected by the outcome.",
    ),
    LearningFieldSchema::new(
        "safetyNotes",
        "array<string>",
        true,
        "Safety notes, unsafe findings, or stop reasons.",
    ),
    LearningFieldSchema::new(
        "auditIds",
        "array<string>",
        true,
        "Audit records for outcome closure and any mutations.",
    ),
];

#[must_use]
pub const fn learning_schemas() -> [LearningObjectSchema; 5] {
    [
        LearningObjectSchema {
            schema_name: LEARNING_QUESTION_SCHEMA_V1,
            schema_uri: "urn:ee:schema:learning-question:v1",
            kind: "learning_question",
            title: "LearningQuestion",
            description: "High-value uncertainty that could change a memory decision.",
            fields: LEARNING_QUESTION_FIELDS,
        },
        LearningObjectSchema {
            schema_name: UNCERTAINTY_ESTIMATE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:learning-uncertainty-estimate:v1",
            kind: "uncertainty_estimate",
            title: "UncertaintyEstimate",
            description: "Quantified uncertainty and confidence for a learning target.",
            fields: UNCERTAINTY_ESTIMATE_FIELDS,
        },
        LearningObjectSchema {
            schema_name: LEARNING_EXPERIMENT_SCHEMA_V1,
            schema_uri: "urn:ee:schema:learning-experiment:v1",
            kind: "learning_experiment",
            title: "LearningExperiment",
            description: "Safe dry-run-first experiment proposed to reduce uncertainty.",
            fields: LEARNING_EXPERIMENT_FIELDS,
        },
        LearningObjectSchema {
            schema_name: LEARNING_OBSERVATION_SCHEMA_V1,
            schema_uri: "urn:ee:schema:learning-observation:v1",
            kind: "learning_observation",
            title: "LearningObservation",
            description: "Evidence observed while running or reviewing a learning experiment.",
            fields: LEARNING_OBSERVATION_FIELDS,
        },
        LearningObjectSchema {
            schema_name: EXPERIMENT_OUTCOME_SCHEMA_V1,
            schema_uri: "urn:ee:schema:experiment-outcome:v1",
            kind: "experiment_outcome",
            title: "ExperimentOutcome",
            description: "Auditable confirmed, rejected, inconclusive, or unsafe experiment result.",
            fields: EXPERIMENT_OUTCOME_FIELDS,
        },
    ]
}

#[must_use]
pub fn learning_schema_catalog_json() -> String {
    let schemas = learning_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!(
        "  \"schema\": \"{LEARNING_SCHEMA_CATALOG_V1}\",\n"
    ));
    output.push_str("  \"schemas\": [\n");
    for (schema_index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, schema.schema_uri);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, schema.schema_name);
        output.push_str(",\n");
        output.push_str("      \"kind\": ");
        push_json_string(&mut output, schema.kind);
        output.push_str(",\n");
        output.push_str("      \"title\": ");
        push_json_string(&mut output, schema.title);
        output.push_str(",\n");
        output.push_str("      \"description\": ");
        push_json_string(&mut output, schema.description);
        output.push_str(",\n");
        output.push_str("      \"type\": \"object\",\n");
        output.push_str("      \"required\": [\n");
        let mut emitted_required = 0;
        for field in schema.fields {
            if field.required {
                emitted_required += 1;
                output.push_str("        ");
                push_json_string(&mut output, field.name);
                if emitted_required == schema.required_count() {
                    output.push('\n');
                } else {
                    output.push_str(",\n");
                }
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"fields\": [\n");
        for (field_index, field) in schema.fields.iter().enumerate() {
            output.push_str("        {\"name\": ");
            push_json_string(&mut output, field.name);
            output.push_str(", \"type\": ");
            push_json_string(&mut output, field.type_name);
            output.push_str(", \"required\": ");
            output.push_str(if field.required { "true" } else { "false" });
            output.push_str(", \"description\": ");
            push_json_string(&mut output, field.description);
            if field_index + 1 == schema.fields.len() {
                output.push_str("}\n");
            } else {
                output.push_str("},\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"additionalProperties\": false\n");
        if schema_index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEARNING_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/learning_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure(
            LEARNING_QUESTION_SCHEMA_V1,
            "ee.learning.question.v1",
            "question",
        )?;
        ensure(
            UNCERTAINTY_ESTIMATE_SCHEMA_V1,
            "ee.learning.uncertainty_estimate.v1",
            "uncertainty",
        )?;
        ensure(
            LEARNING_EXPERIMENT_SCHEMA_V1,
            "ee.learning.experiment.v1",
            "experiment",
        )?;
        ensure(
            LEARNING_OBSERVATION_SCHEMA_V1,
            "ee.learning.observation.v1",
            "observation",
        )?;
        ensure(
            EXPERIMENT_OUTCOME_SCHEMA_V1,
            "ee.learning.experiment_outcome.v1",
            "outcome",
        )?;
        ensure(
            LEARNING_SCHEMA_CATALOG_V1,
            "ee.learning.schemas.v1",
            "catalog",
        )
    }

    #[test]
    fn stable_wire_enums_round_trip() -> TestResult {
        for status in LearningQuestionStatus::all() {
            ensure(
                LearningQuestionStatus::from_str(status.as_str()),
                Ok(status),
                "question status",
            )?;
        }
        for kind in LearningTargetKind::all() {
            ensure(
                LearningTargetKind::from_str(kind.as_str()),
                Ok(kind),
                "target kind",
            )?;
        }
        for status in LearningExperimentStatus::all() {
            ensure(
                LearningExperimentStatus::from_str(status.as_str()),
                Ok(status),
                "experiment status",
            )?;
        }
        for boundary in ExperimentSafetyBoundary::all() {
            ensure(
                ExperimentSafetyBoundary::from_str(boundary.as_str()),
                Ok(boundary),
                "safety boundary",
            )?;
        }
        for signal in LearningObservationSignal::all() {
            ensure(
                LearningObservationSignal::from_str(signal.as_str()),
                Ok(signal),
                "observation signal",
            )?;
        }
        for status in ExperimentOutcomeStatus::all() {
            ensure(
                ExperimentOutcomeStatus::from_str(status.as_str()),
                Ok(status),
                "outcome status",
            )?;
        }
        ensure(
            ExperimentOutcomeStatus::from_str("success").map_err(|error| error.field()),
            Err("experiment_outcome_status"),
            "invalid outcome field",
        )
    }

    #[test]
    fn learning_record_builders_set_schemas_and_defaults() -> TestResult {
        let question = LearningQuestion::new(
            "learn-q-001",
            "release",
            "Does format drift predict release failure?",
            "May change whether release context includes fmt history.",
            "2026-04-30T12:00:00Z",
        )
        .with_status(LearningQuestionStatus::ReadyForExperiment)
        .with_target_artifact("mem-001")
        .with_evidence("ev-001")
        .with_priority(250)
        .with_uncertainty(1.5)
        .with_expected_value(-0.25);

        ensure(
            question.schema,
            LEARNING_QUESTION_SCHEMA_V1,
            "question schema",
        )?;
        ensure(question.priority, 100, "priority clamp")?;
        ensure(question.uncertainty, 1.0, "uncertainty clamp")?;
        ensure(question.expected_value, 0.0, "expected value clamp")?;

        let estimate = UncertaintyEstimate::new(
            "unc-001",
            "learn-q-001",
            "mem-001",
            LearningTargetKind::Memory,
            "hash_fixture",
            "2026-04-30T12:01:00Z",
        )
        .with_confidence(0.75)
        .with_sample_size(4)
        .with_evidence("ev-002");

        ensure(
            estimate.schema,
            UNCERTAINTY_ESTIMATE_SCHEMA_V1,
            "estimate schema",
        )?;
        ensure(estimate.confidence, 0.75, "estimate confidence")?;

        let experiment = LearningExperiment::new(
            "exp-001",
            "learn-q-001",
            "Replay release preparation",
            "Replay artifacts will expose whether fmt history matters.",
            "Stop after replay report is captured.",
            "2026-04-30T12:02:00Z",
        )
        .with_status(LearningExperimentStatus::DryRunReady)
        .with_safety_boundary(ExperimentSafetyBoundary::AskBeforeActing)
        .with_expected_value(0.8)
        .with_attention_budget_tokens(800)
        .with_max_runtime_seconds(60)
        .with_affected_decision("decision-001");

        ensure(
            experiment.schema,
            LEARNING_EXPERIMENT_SCHEMA_V1,
            "experiment schema",
        )?;
        ensure(experiment.dry_run_first, true, "dry-run-first default")?;

        let observation = LearningObservation::new(
            "obs-001",
            "exp-001",
            "2026-04-30T12:03:00Z",
            "contract-test",
            "release_replay_success",
        )
        .with_signal(LearningObservationSignal::Positive)
        .with_measurement_value(0.66666)
        .with_evidence("ev-003")
        .with_note("Dry-run replay found relevant evidence.");

        ensure(
            observation.schema,
            LEARNING_OBSERVATION_SCHEMA_V1,
            "observation schema",
        )?;
        ensure(
            observation.measurement_value,
            Some(0.667),
            "measurement rounding",
        )?;

        let outcome = ExperimentOutcome::new(
            "out-001",
            "exp-001",
            "2026-04-30T12:04:00Z",
            "Keep fmt history in release context.",
        )
        .with_status(ExperimentOutcomeStatus::Confirmed)
        .with_confidence_delta(2.0)
        .with_priority_delta(-5)
        .with_promoted_artifact("mem-001")
        .with_audit_id("audit-001");

        ensure(
            outcome.schema,
            EXPERIMENT_OUTCOME_SCHEMA_V1,
            "outcome schema",
        )?;
        ensure(outcome.confidence_delta, 1.0, "confidence delta clamp")?;
        ensure(outcome.priority_delta, -5, "priority delta")
    }

    #[test]
    fn data_json_uses_stable_wire_names() -> TestResult {
        let experiment = LearningExperiment::new(
            "exp-001",
            "learn-q-001",
            "Replay release preparation",
            "Replay artifacts will expose whether fmt history matters.",
            "Stop after replay report is captured.",
            "2026-04-30T12:02:00Z",
        )
        .with_safety_boundary(ExperimentSafetyBoundary::HumanReview)
        .with_expected_value(0.81234);

        let json = experiment.data_json();
        ensure(
            json.get("schema").and_then(serde_json::Value::as_str),
            Some(LEARNING_EXPERIMENT_SCHEMA_V1),
            "schema",
        )?;
        ensure(
            json.get("safetyBoundary")
                .and_then(serde_json::Value::as_str),
            Some("human_review"),
            "safety boundary",
        )?;
        ensure(
            json.get("expectedValue")
                .and_then(serde_json::Value::as_f64),
            Some(0.812),
            "rounded expected value",
        )
    }

    #[test]
    fn learning_schema_catalog_order_is_stable() -> TestResult {
        let schemas = learning_schemas();
        ensure(schemas.len(), 5, "schema count")?;
        ensure(
            schemas[0].schema_name,
            LEARNING_QUESTION_SCHEMA_V1,
            "question",
        )?;
        ensure(
            schemas[1].schema_name,
            UNCERTAINTY_ESTIMATE_SCHEMA_V1,
            "uncertainty estimate",
        )?;
        ensure(
            schemas[2].schema_name,
            LEARNING_EXPERIMENT_SCHEMA_V1,
            "experiment",
        )?;
        ensure(
            schemas[3].schema_name,
            LEARNING_OBSERVATION_SCHEMA_V1,
            "observation",
        )?;
        ensure(
            schemas[4].schema_name,
            EXPERIMENT_OUTCOME_SCHEMA_V1,
            "outcome",
        )
    }

    #[test]
    fn learning_schema_catalog_matches_golden_fixture() {
        assert_eq!(learning_schema_catalog_json(), LEARNING_SCHEMA_GOLDEN);
    }

    #[test]
    fn learning_schema_catalog_is_valid_json() -> TestResult {
        let parsed: serde_json::Value = serde_json::from_str(LEARNING_SCHEMA_GOLDEN)
            .map_err(|error| format!("learning schema golden must be valid JSON: {error}"))?;
        ensure(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some(LEARNING_SCHEMA_CATALOG_V1),
            "catalog schema",
        )?;
        let schemas = parsed
            .get("schemas")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "schemas must be an array".to_string())?;
        ensure(schemas.len(), 5, "catalog length")
    }
}
