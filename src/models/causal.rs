//! Causal memory credit and uplift schema contracts (EE-450).
//!
//! These records distinguish mere exposure from plausible influence, replay
//! support, and experiment-backed uplift. They are schema contracts for later
//! `ee causal` commands; defining them here does not promote, demote, or mutate
//! durable memory.

use std::fmt;
use std::str::FromStr;

use serde_json::{Value as JsonValue, json};

use super::decision::DecisionPlane;

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for a memory/artifact exposure to an agent decision.
pub const CAUSAL_EXPOSURE_SCHEMA_V1: &str = "ee.causal.exposure.v1";

/// Schema for the decision trace used by causal credit analysis.
pub const DECISION_TRACE_SCHEMA_V1: &str = "ee.causal.decision_trace.v1";

/// Schema for a bounded uplift estimate.
pub const UPLIFT_ESTIMATE_SCHEMA_V1: &str = "ee.causal.uplift_estimate.v1";

/// Schema for a confounder that can explain apparent uplift.
pub const CONFOUNDER_SCHEMA_V1: &str = "ee.causal.confounder.v1";

/// Schema for a dry-run-first promotion plan.
pub const PROMOTION_PLAN_SCHEMA_V1: &str = "ee.causal.promotion_plan.v1";

/// Schema for the causal schema catalog.
pub const CAUSAL_SCHEMA_CATALOG_V1: &str = "ee.causal.schemas.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

fn bounded_unit(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn bounded_delta(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
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

/// How an artifact was exposed to an agent decision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CausalExposureChannel {
    ContextPack,
    SearchResult,
    WhyExplanation,
    AgentDocs,
    Procedure,
    ManualReference,
}

impl CausalExposureChannel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContextPack => "context_pack",
            Self::SearchResult => "search_result",
            Self::WhyExplanation => "why_explanation",
            Self::AgentDocs => "agent_docs",
            Self::Procedure => "procedure",
            Self::ManualReference => "manual_reference",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::ContextPack,
            Self::SearchResult,
            Self::WhyExplanation,
            Self::AgentDocs,
            Self::Procedure,
            Self::ManualReference,
        ]
    }
}

impl fmt::Display for CausalExposureChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CausalExposureChannel {
    type Err = ParseCausalValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "context_pack" => Ok(Self::ContextPack),
            "search_result" => Ok(Self::SearchResult),
            "why_explanation" => Ok(Self::WhyExplanation),
            "agent_docs" => Ok(Self::AgentDocs),
            "procedure" => Ok(Self::Procedure),
            "manual_reference" => Ok(Self::ManualReference),
            _ => Err(ParseCausalValueError::new(
                "causal_exposure_channel",
                input,
                "context_pack, search_result, why_explanation, agent_docs, procedure, manual_reference",
            )),
        }
    }
}

/// What the decision did with exposed evidence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DecisionTraceOutcome {
    Used,
    Ignored,
    Deferred,
    Rejected,
    Unsafe,
}

impl DecisionTraceOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Used => "used",
            Self::Ignored => "ignored",
            Self::Deferred => "deferred",
            Self::Rejected => "rejected",
            Self::Unsafe => "unsafe",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Used,
            Self::Ignored,
            Self::Deferred,
            Self::Rejected,
            Self::Unsafe,
        ]
    }
}

impl fmt::Display for DecisionTraceOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DecisionTraceOutcome {
    type Err = ParseCausalValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "used" => Ok(Self::Used),
            "ignored" => Ok(Self::Ignored),
            "deferred" => Ok(Self::Deferred),
            "rejected" => Ok(Self::Rejected),
            "unsafe" => Ok(Self::Unsafe),
            _ => Err(ParseCausalValueError::new(
                "decision_trace_outcome",
                input,
                "used, ignored, deferred, rejected, unsafe",
            )),
        }
    }
}

/// Evidence strength behind a causal estimate or promotion plan.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CausalEvidenceStrength {
    ExposureOnly,
    Correlational,
    ReplaySupported,
    ExperimentSupported,
    Rejected,
}

impl CausalEvidenceStrength {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExposureOnly => "exposure_only",
            Self::Correlational => "correlational",
            Self::ReplaySupported => "replay_supported",
            Self::ExperimentSupported => "experiment_supported",
            Self::Rejected => "rejected",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::ExposureOnly,
            Self::Correlational,
            Self::ReplaySupported,
            Self::ExperimentSupported,
            Self::Rejected,
        ]
    }
}

impl fmt::Display for CausalEvidenceStrength {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CausalEvidenceStrength {
    type Err = ParseCausalValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "exposure_only" => Ok(Self::ExposureOnly),
            "correlational" => Ok(Self::Correlational),
            "replay_supported" => Ok(Self::ReplaySupported),
            "experiment_supported" => Ok(Self::ExperimentSupported),
            "rejected" => Ok(Self::Rejected),
            _ => Err(ParseCausalValueError::new(
                "causal_evidence_strength",
                input,
                "exposure_only, correlational, replay_supported, experiment_supported, rejected",
            )),
        }
    }
}

/// Direction of the estimated causal uplift.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UpliftDirection {
    Positive,
    Negative,
    Neutral,
    Unknown,
}

impl UpliftDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Neutral => "neutral",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Positive, Self::Negative, Self::Neutral, Self::Unknown]
    }

    #[must_use]
    pub fn from_uplift(uplift: f64) -> Self {
        if !uplift.is_finite() {
            Self::Unknown
        } else if uplift > 0.001 {
            Self::Positive
        } else if uplift < -0.001 {
            Self::Negative
        } else {
            Self::Neutral
        }
    }
}

impl fmt::Display for UpliftDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for UpliftDirection {
    type Err = ParseCausalValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "positive" => Ok(Self::Positive),
            "negative" => Ok(Self::Negative),
            "neutral" => Ok(Self::Neutral),
            "unknown" => Ok(Self::Unknown),
            _ => Err(ParseCausalValueError::new(
                "uplift_direction",
                input,
                "positive, negative, neutral, unknown",
            )),
        }
    }
}

/// Common confounder classes for uplift analysis.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConfounderKind {
    SelectionBias,
    TaskDifficulty,
    AgentSkill,
    TimeTrend,
    ToolingChange,
    ExternalIntervention,
}

impl ConfounderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SelectionBias => "selection_bias",
            Self::TaskDifficulty => "task_difficulty",
            Self::AgentSkill => "agent_skill",
            Self::TimeTrend => "time_trend",
            Self::ToolingChange => "tooling_change",
            Self::ExternalIntervention => "external_intervention",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::SelectionBias,
            Self::TaskDifficulty,
            Self::AgentSkill,
            Self::TimeTrend,
            Self::ToolingChange,
            Self::ExternalIntervention,
        ]
    }
}

impl fmt::Display for ConfounderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ConfounderKind {
    type Err = ParseCausalValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "selection_bias" => Ok(Self::SelectionBias),
            "task_difficulty" => Ok(Self::TaskDifficulty),
            "agent_skill" => Ok(Self::AgentSkill),
            "time_trend" => Ok(Self::TimeTrend),
            "tooling_change" => Ok(Self::ToolingChange),
            "external_intervention" => Ok(Self::ExternalIntervention),
            _ => Err(ParseCausalValueError::new(
                "confounder_kind",
                input,
                "selection_bias, task_difficulty, agent_skill, time_trend, tooling_change, external_intervention",
            )),
        }
    }
}

/// Planned action for a promotion plan.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PromotionAction {
    Promote,
    Hold,
    Demote,
    Archive,
    Quarantine,
}

impl PromotionAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Promote => "promote",
            Self::Hold => "hold",
            Self::Demote => "demote",
            Self::Archive => "archive",
            Self::Quarantine => "quarantine",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Promote,
            Self::Hold,
            Self::Demote,
            Self::Archive,
            Self::Quarantine,
        ]
    }
}

impl fmt::Display for PromotionAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for PromotionAction {
    type Err = ParseCausalValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "promote" => Ok(Self::Promote),
            "hold" => Ok(Self::Hold),
            "demote" => Ok(Self::Demote),
            "archive" => Ok(Self::Archive),
            "quarantine" => Ok(Self::Quarantine),
            _ => Err(ParseCausalValueError::new(
                "promotion_action",
                input,
                "promote, hold, demote, archive, quarantine",
            )),
        }
    }
}

/// Lifecycle status for a promotion plan.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PromotionPlanStatus {
    Proposed,
    DryRunReady,
    Approved,
    Applied,
    Rejected,
    Superseded,
}

impl PromotionPlanStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::DryRunReady => "dry_run_ready",
            Self::Approved => "approved",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Proposed,
            Self::DryRunReady,
            Self::Approved,
            Self::Applied,
            Self::Rejected,
            Self::Superseded,
        ]
    }
}

impl fmt::Display for PromotionPlanStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for PromotionPlanStatus {
    type Err = ParseCausalValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "proposed" => Ok(Self::Proposed),
            "dry_run_ready" => Ok(Self::DryRunReady),
            "approved" => Ok(Self::Approved),
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            "superseded" => Ok(Self::Superseded),
            _ => Err(ParseCausalValueError::new(
                "promotion_plan_status",
                input,
                "proposed, dry_run_ready, approved, applied, rejected, superseded",
            )),
        }
    }
}

/// Error returned when a stable causal wire value cannot be parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCausalValueError {
    field: &'static str,
    value: String,
    expected: &'static str,
}

impl ParseCausalValueError {
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

impl fmt::Display for ParseCausalValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid {} value '{}'; expected one of: {}",
            self.field, self.value, self.expected
        )
    }
}

impl std::error::Error for ParseCausalValueError {}

// ============================================================================
// Domain Records
// ============================================================================

/// Exposure of a memory or artifact to an agent decision.
#[derive(Clone, Debug, PartialEq)]
pub struct CausalExposure {
    pub schema: &'static str,
    pub exposure_id: String,
    pub artifact_id: String,
    pub artifact_kind: String,
    pub decision_id: String,
    pub channel: CausalExposureChannel,
    pub exposed_at: String,
    pub rank: Option<u32>,
    pub policy_id: Option<String>,
    pub context_pack_id: Option<String>,
    pub trace_id: Option<String>,
    pub evidence_ids: Vec<String>,
}

impl CausalExposure {
    #[must_use]
    pub fn new(
        exposure_id: impl Into<String>,
        artifact_id: impl Into<String>,
        artifact_kind: impl Into<String>,
        decision_id: impl Into<String>,
        channel: CausalExposureChannel,
        exposed_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: CAUSAL_EXPOSURE_SCHEMA_V1,
            exposure_id: exposure_id.into(),
            artifact_id: artifact_id.into(),
            artifact_kind: artifact_kind.into(),
            decision_id: decision_id.into(),
            channel,
            exposed_at: exposed_at.into(),
            rank: None,
            policy_id: None,
            context_pack_id: None,
            trace_id: None,
            evidence_ids: Vec::new(),
        }
    }

    #[must_use]
    pub const fn with_rank(mut self, rank: u32) -> Self {
        self.rank = Some(rank);
        self
    }

    #[must_use]
    pub fn with_policy(mut self, policy_id: impl Into<String>) -> Self {
        self.policy_id = Some(policy_id.into());
        self
    }

    #[must_use]
    pub fn with_context_pack(mut self, context_pack_id: impl Into<String>) -> Self {
        self.context_pack_id = Some(context_pack_id.into());
        self
    }

    #[must_use]
    pub fn with_trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
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
            "exposureId": self.exposure_id,
            "artifactId": self.artifact_id,
            "artifactKind": self.artifact_kind,
            "decisionId": self.decision_id,
            "channel": self.channel.as_str(),
            "exposedAt": self.exposed_at,
            "rank": self.rank,
            "policyId": self.policy_id,
            "contextPackId": self.context_pack_id,
            "traceId": self.trace_id,
            "evidenceIds": self.evidence_ids,
        })
    }
}

/// Decision trace tying exposures to the action actually taken.
#[derive(Clone, Debug, PartialEq)]
pub struct CausalDecisionTrace {
    pub schema: &'static str,
    pub decision_id: String,
    pub trace_id: String,
    pub plane: DecisionPlane,
    pub decided_at: String,
    pub outcome: DecisionTraceOutcome,
    pub agent: String,
    pub task_id: Option<String>,
    pub policy_id: Option<String>,
    pub exposed_artifact_ids: Vec<String>,
    pub selected_artifact_ids: Vec<String>,
    pub rejected_artifact_ids: Vec<String>,
    pub rationale: String,
    pub evidence_ids: Vec<String>,
}

impl CausalDecisionTrace {
    #[must_use]
    pub fn new(
        decision_id: impl Into<String>,
        trace_id: impl Into<String>,
        plane: DecisionPlane,
        decided_at: impl Into<String>,
        agent: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            schema: DECISION_TRACE_SCHEMA_V1,
            decision_id: decision_id.into(),
            trace_id: trace_id.into(),
            plane,
            decided_at: decided_at.into(),
            outcome: DecisionTraceOutcome::Deferred,
            agent: agent.into(),
            task_id: None,
            policy_id: None,
            exposed_artifact_ids: Vec::new(),
            selected_artifact_ids: Vec::new(),
            rejected_artifact_ids: Vec::new(),
            rationale: rationale.into(),
            evidence_ids: Vec::new(),
        }
    }

    #[must_use]
    pub const fn with_outcome(mut self, outcome: DecisionTraceOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    #[must_use]
    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    #[must_use]
    pub fn with_policy(mut self, policy_id: impl Into<String>) -> Self {
        self.policy_id = Some(policy_id.into());
        self
    }

    #[must_use]
    pub fn with_exposed_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.exposed_artifact_ids.push(artifact_id.into());
        self
    }

    #[must_use]
    pub fn with_selected_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.selected_artifact_ids.push(artifact_id.into());
        self
    }

    #[must_use]
    pub fn with_rejected_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.rejected_artifact_ids.push(artifact_id.into());
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
            "decisionId": self.decision_id,
            "traceId": self.trace_id,
            "plane": self.plane.as_str(),
            "decidedAt": self.decided_at,
            "outcome": self.outcome.as_str(),
            "agent": self.agent,
            "taskId": self.task_id,
            "policyId": self.policy_id,
            "exposedArtifactIds": self.exposed_artifact_ids,
            "selectedArtifactIds": self.selected_artifact_ids,
            "rejectedArtifactIds": self.rejected_artifact_ids,
            "rationale": self.rationale,
            "evidenceIds": self.evidence_ids,
        })
    }
}

/// Bounded estimate of the change associated with artifact exposure.
#[derive(Clone, Debug, PartialEq)]
pub struct UpliftEstimate {
    pub schema: &'static str,
    pub estimate_id: String,
    pub artifact_id: String,
    pub decision_id: String,
    pub baseline_success_rate: f64,
    pub observed_success_rate: f64,
    pub uplift: f64,
    pub direction: UpliftDirection,
    pub confidence: f64,
    pub sample_size: u32,
    pub evidence_strength: CausalEvidenceStrength,
    pub method: String,
    pub exposure_ids: Vec<String>,
    pub confounder_ids: Vec<String>,
    pub estimated_at: String,
}

impl UpliftEstimate {
    #[must_use]
    pub fn new(
        estimate_id: impl Into<String>,
        artifact_id: impl Into<String>,
        decision_id: impl Into<String>,
        method: impl Into<String>,
        estimated_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: UPLIFT_ESTIMATE_SCHEMA_V1,
            estimate_id: estimate_id.into(),
            artifact_id: artifact_id.into(),
            decision_id: decision_id.into(),
            baseline_success_rate: 0.0,
            observed_success_rate: 0.0,
            uplift: 0.0,
            direction: UpliftDirection::Neutral,
            confidence: 0.0,
            sample_size: 0,
            evidence_strength: CausalEvidenceStrength::ExposureOnly,
            method: method.into(),
            exposure_ids: Vec::new(),
            confounder_ids: Vec::new(),
            estimated_at: estimated_at.into(),
        }
    }

    #[must_use]
    pub fn with_rates(mut self, baseline_success_rate: f64, observed_success_rate: f64) -> Self {
        self.baseline_success_rate = bounded_unit(baseline_success_rate);
        self.observed_success_rate = bounded_unit(observed_success_rate);
        self.uplift = bounded_delta(self.observed_success_rate - self.baseline_success_rate);
        self.direction = UpliftDirection::from_uplift(self.uplift);
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
    pub const fn with_evidence_strength(mut self, strength: CausalEvidenceStrength) -> Self {
        self.evidence_strength = strength;
        self
    }

    #[must_use]
    pub fn with_exposure(mut self, exposure_id: impl Into<String>) -> Self {
        self.exposure_ids.push(exposure_id.into());
        self
    }

    #[must_use]
    pub fn with_confounder(mut self, confounder_id: impl Into<String>) -> Self {
        self.confounder_ids.push(confounder_id.into());
        self
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "estimateId": self.estimate_id,
            "artifactId": self.artifact_id,
            "decisionId": self.decision_id,
            "baselineSuccessRate": rounded_metric(self.baseline_success_rate),
            "observedSuccessRate": rounded_metric(self.observed_success_rate),
            "uplift": rounded_metric(self.uplift),
            "direction": self.direction.as_str(),
            "confidence": rounded_metric(self.confidence),
            "sampleSize": self.sample_size,
            "evidenceStrength": self.evidence_strength.as_str(),
            "method": self.method,
            "exposureIds": self.exposure_ids,
            "confounderIds": self.confounder_ids,
            "estimatedAt": self.estimated_at,
        })
    }
}

/// Confounder that may explain apparent causal uplift.
#[derive(Clone, Debug, PartialEq)]
pub struct CausalConfounder {
    pub schema: &'static str,
    pub confounder_id: String,
    pub kind: ConfounderKind,
    pub description: String,
    pub severity: f64,
    pub mitigation: String,
    pub affected_artifact_ids: Vec<String>,
    pub affected_decision_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
}

impl CausalConfounder {
    #[must_use]
    pub fn new(
        confounder_id: impl Into<String>,
        kind: ConfounderKind,
        description: impl Into<String>,
        mitigation: impl Into<String>,
    ) -> Self {
        Self {
            schema: CONFOUNDER_SCHEMA_V1,
            confounder_id: confounder_id.into(),
            kind,
            description: description.into(),
            severity: 0.0,
            mitigation: mitigation.into(),
            affected_artifact_ids: Vec::new(),
            affected_decision_ids: Vec::new(),
            evidence_ids: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_severity(mut self, severity: f64) -> Self {
        self.severity = bounded_unit(severity);
        self
    }

    #[must_use]
    pub fn with_affected_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.affected_artifact_ids.push(artifact_id.into());
        self
    }

    #[must_use]
    pub fn with_affected_decision(mut self, decision_id: impl Into<String>) -> Self {
        self.affected_decision_ids.push(decision_id.into());
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
            "confounderId": self.confounder_id,
            "kind": self.kind.as_str(),
            "description": self.description,
            "severity": rounded_metric(self.severity),
            "mitigation": self.mitigation,
            "affectedArtifactIds": self.affected_artifact_ids,
            "affectedDecisionIds": self.affected_decision_ids,
            "evidenceIds": self.evidence_ids,
        })
    }
}

/// Dry-run-first plan for changing an artifact's memory posture.
#[derive(Clone, Debug, PartialEq)]
pub struct PromotionPlan {
    pub schema: &'static str,
    pub plan_id: String,
    pub artifact_id: String,
    pub action: PromotionAction,
    pub status: PromotionPlanStatus,
    pub evidence_strength: CausalEvidenceStrength,
    pub minimum_uplift: f64,
    pub estimated_uplift: f64,
    pub required_evidence_ids: Vec<String>,
    pub blocking_confounder_ids: Vec<String>,
    pub dry_run_first: bool,
    pub audit_ids: Vec<String>,
    pub created_at: String,
}

impl PromotionPlan {
    #[must_use]
    pub fn new(
        plan_id: impl Into<String>,
        artifact_id: impl Into<String>,
        action: PromotionAction,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: PROMOTION_PLAN_SCHEMA_V1,
            plan_id: plan_id.into(),
            artifact_id: artifact_id.into(),
            action,
            status: PromotionPlanStatus::Proposed,
            evidence_strength: CausalEvidenceStrength::ExposureOnly,
            minimum_uplift: 0.0,
            estimated_uplift: 0.0,
            required_evidence_ids: Vec::new(),
            blocking_confounder_ids: Vec::new(),
            dry_run_first: true,
            audit_ids: Vec::new(),
            created_at: created_at.into(),
        }
    }

    #[must_use]
    pub const fn with_status(mut self, status: PromotionPlanStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub const fn with_evidence_strength(mut self, strength: CausalEvidenceStrength) -> Self {
        self.evidence_strength = strength;
        self
    }

    #[must_use]
    pub fn with_minimum_uplift(mut self, uplift: f64) -> Self {
        self.minimum_uplift = bounded_delta(uplift);
        self
    }

    #[must_use]
    pub fn with_estimated_uplift(mut self, uplift: f64) -> Self {
        self.estimated_uplift = bounded_delta(uplift);
        self
    }

    #[must_use]
    pub fn with_required_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.required_evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub fn with_blocking_confounder(mut self, confounder_id: impl Into<String>) -> Self {
        self.blocking_confounder_ids.push(confounder_id.into());
        self
    }

    #[must_use]
    pub const fn without_dry_run_first(mut self) -> Self {
        self.dry_run_first = false;
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
            "planId": self.plan_id,
            "artifactId": self.artifact_id,
            "action": self.action.as_str(),
            "status": self.status.as_str(),
            "evidenceStrength": self.evidence_strength.as_str(),
            "minimumUplift": rounded_metric(self.minimum_uplift),
            "estimatedUplift": rounded_metric(self.estimated_uplift),
            "requiredEvidenceIds": self.required_evidence_ids,
            "blockingConfounderIds": self.blocking_confounder_ids,
            "dryRunFirst": self.dry_run_first,
            "auditIds": self.audit_ids,
            "createdAt": self.created_at,
        })
    }
}

// ============================================================================
// Schema Catalog
// ============================================================================

/// Field descriptor used by the causal schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CausalFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl CausalFieldSchema {
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

/// Stable JSON-schema-like catalog entry for causal records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CausalObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [CausalFieldSchema],
}

impl CausalObjectSchema {
    #[must_use]
    pub fn required_count(&self) -> usize {
        self.fields.iter().filter(|field| field.required).count()
    }
}

const CAUSAL_EXPOSURE_FIELDS: &[CausalFieldSchema] = &[
    CausalFieldSchema::new("schema", "string", true, "Schema identifier."),
    CausalFieldSchema::new("exposureId", "string", true, "Stable exposure identifier."),
    CausalFieldSchema::new(
        "artifactId",
        "string",
        true,
        "Exposed memory or artifact identifier.",
    ),
    CausalFieldSchema::new("artifactKind", "string", true, "Exposed artifact category."),
    CausalFieldSchema::new(
        "decisionId",
        "string",
        true,
        "Decision that received the exposure.",
    ),
    CausalFieldSchema::new("channel", "string", true, "Exposure channel."),
    CausalFieldSchema::new("exposedAt", "string", true, "RFC 3339 exposure timestamp."),
    CausalFieldSchema::new(
        "rank",
        "integer|null",
        false,
        "Rank or slot when exposure was ordered.",
    ),
    CausalFieldSchema::new(
        "policyId",
        "string|null",
        false,
        "Policy active when exposed.",
    ),
    CausalFieldSchema::new(
        "contextPackId",
        "string|null",
        false,
        "Context pack containing the exposure.",
    ),
    CausalFieldSchema::new(
        "traceId",
        "string|null",
        false,
        "Trace linking related decisions.",
    ),
    CausalFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence supporting exposure capture.",
    ),
];

const DECISION_TRACE_FIELDS: &[CausalFieldSchema] = &[
    CausalFieldSchema::new("schema", "string", true, "Schema identifier."),
    CausalFieldSchema::new("decisionId", "string", true, "Stable decision identifier."),
    CausalFieldSchema::new(
        "traceId",
        "string",
        true,
        "Trace linking exposures and outcomes.",
    ),
    CausalFieldSchema::new("plane", "string", true, "Decision plane."),
    CausalFieldSchema::new("decidedAt", "string", true, "RFC 3339 decision timestamp."),
    CausalFieldSchema::new(
        "outcome",
        "string",
        true,
        "How the decision treated exposed evidence.",
    ),
    CausalFieldSchema::new(
        "agent",
        "string",
        true,
        "Agent, harness, or tool that made the decision.",
    ),
    CausalFieldSchema::new(
        "taskId",
        "string|null",
        false,
        "Task or run associated with the decision.",
    ),
    CausalFieldSchema::new(
        "policyId",
        "string|null",
        false,
        "Policy that governed the decision.",
    ),
    CausalFieldSchema::new(
        "exposedArtifactIds",
        "array<string>",
        true,
        "Artifacts available to the decision.",
    ),
    CausalFieldSchema::new(
        "selectedArtifactIds",
        "array<string>",
        true,
        "Artifacts actually used.",
    ),
    CausalFieldSchema::new(
        "rejectedArtifactIds",
        "array<string>",
        true,
        "Artifacts rejected or ignored.",
    ),
    CausalFieldSchema::new(
        "rationale",
        "string",
        true,
        "Decision rationale or explanation.",
    ),
    CausalFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence supporting the trace.",
    ),
];

const UPLIFT_ESTIMATE_FIELDS: &[CausalFieldSchema] = &[
    CausalFieldSchema::new("schema", "string", true, "Schema identifier."),
    CausalFieldSchema::new(
        "estimateId",
        "string",
        true,
        "Stable uplift estimate identifier.",
    ),
    CausalFieldSchema::new(
        "artifactId",
        "string",
        true,
        "Artifact whose influence is estimated.",
    ),
    CausalFieldSchema::new(
        "decisionId",
        "string",
        true,
        "Decision or decision class being estimated.",
    ),
    CausalFieldSchema::new(
        "baselineSuccessRate",
        "number",
        true,
        "Baseline outcome rate from 0.0 to 1.0.",
    ),
    CausalFieldSchema::new(
        "observedSuccessRate",
        "number",
        true,
        "Observed outcome rate from 0.0 to 1.0.",
    ),
    CausalFieldSchema::new(
        "uplift",
        "number",
        true,
        "Observed minus baseline rate from -1.0 to 1.0.",
    ),
    CausalFieldSchema::new(
        "direction",
        "string",
        true,
        "Direction of the uplift estimate.",
    ),
    CausalFieldSchema::new("confidence", "number", true, "Confidence from 0.0 to 1.0."),
    CausalFieldSchema::new(
        "sampleSize",
        "integer",
        true,
        "Number of observations used.",
    ),
    CausalFieldSchema::new(
        "evidenceStrength",
        "string",
        true,
        "Strength of causal evidence.",
    ),
    CausalFieldSchema::new("method", "string", true, "Deterministic estimation method."),
    CausalFieldSchema::new(
        "exposureIds",
        "array<string>",
        true,
        "Exposure records used by the estimate.",
    ),
    CausalFieldSchema::new(
        "confounderIds",
        "array<string>",
        true,
        "Known confounders considered.",
    ),
    CausalFieldSchema::new(
        "estimatedAt",
        "string",
        true,
        "RFC 3339 estimate timestamp.",
    ),
];

const CONFOUNDER_FIELDS: &[CausalFieldSchema] = &[
    CausalFieldSchema::new("schema", "string", true, "Schema identifier."),
    CausalFieldSchema::new(
        "confounderId",
        "string",
        true,
        "Stable confounder identifier.",
    ),
    CausalFieldSchema::new("kind", "string", true, "Confounder class."),
    CausalFieldSchema::new(
        "description",
        "string",
        true,
        "Why this can explain apparent uplift.",
    ),
    CausalFieldSchema::new("severity", "number", true, "Severity from 0.0 to 1.0."),
    CausalFieldSchema::new(
        "mitigation",
        "string",
        true,
        "How to control or account for the confounder.",
    ),
    CausalFieldSchema::new(
        "affectedArtifactIds",
        "array<string>",
        true,
        "Artifacts affected by this confounder.",
    ),
    CausalFieldSchema::new(
        "affectedDecisionIds",
        "array<string>",
        true,
        "Decisions affected by this confounder.",
    ),
    CausalFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence supporting the confounder.",
    ),
];

const PROMOTION_PLAN_FIELDS: &[CausalFieldSchema] = &[
    CausalFieldSchema::new("schema", "string", true, "Schema identifier."),
    CausalFieldSchema::new(
        "planId",
        "string",
        true,
        "Stable promotion plan identifier.",
    ),
    CausalFieldSchema::new(
        "artifactId",
        "string",
        true,
        "Artifact targeted by the plan.",
    ),
    CausalFieldSchema::new("action", "string", true, "Planned memory posture action."),
    CausalFieldSchema::new("status", "string", true, "Promotion plan lifecycle status."),
    CausalFieldSchema::new(
        "evidenceStrength",
        "string",
        true,
        "Evidence strength required by the plan.",
    ),
    CausalFieldSchema::new(
        "minimumUplift",
        "number",
        true,
        "Minimum uplift threshold from -1.0 to 1.0.",
    ),
    CausalFieldSchema::new(
        "estimatedUplift",
        "number",
        true,
        "Current estimated uplift from -1.0 to 1.0.",
    ),
    CausalFieldSchema::new(
        "requiredEvidenceIds",
        "array<string>",
        true,
        "Evidence required before applying the plan.",
    ),
    CausalFieldSchema::new(
        "blockingConfounderIds",
        "array<string>",
        true,
        "Confounders blocking promotion.",
    ),
    CausalFieldSchema::new(
        "dryRunFirst",
        "boolean",
        true,
        "Whether dry-run verification is required before mutation.",
    ),
    CausalFieldSchema::new(
        "auditIds",
        "array<string>",
        true,
        "Audit records attached to the plan.",
    ),
    CausalFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
];

#[must_use]
pub const fn causal_schemas() -> [CausalObjectSchema; 5] {
    [
        CausalObjectSchema {
            schema_name: CAUSAL_EXPOSURE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:causal-exposure:v1",
            kind: "causal_exposure",
            title: "CausalExposure",
            description: "Exposure of a memory or artifact to an agent decision.",
            fields: CAUSAL_EXPOSURE_FIELDS,
        },
        CausalObjectSchema {
            schema_name: DECISION_TRACE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:causal-decision-trace:v1",
            kind: "decision_trace",
            title: "CausalDecisionTrace",
            description: "Decision trace tying exposed artifacts to selected and rejected evidence.",
            fields: DECISION_TRACE_FIELDS,
        },
        CausalObjectSchema {
            schema_name: UPLIFT_ESTIMATE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:causal-uplift-estimate:v1",
            kind: "uplift_estimate",
            title: "UpliftEstimate",
            description: "Bounded estimate of outcome change associated with an artifact exposure.",
            fields: UPLIFT_ESTIMATE_FIELDS,
        },
        CausalObjectSchema {
            schema_name: CONFOUNDER_SCHEMA_V1,
            schema_uri: "urn:ee:schema:causal-confounder:v1",
            kind: "confounder",
            title: "CausalConfounder",
            description: "Alternative explanation that can weaken or block causal claims.",
            fields: CONFOUNDER_FIELDS,
        },
        CausalObjectSchema {
            schema_name: PROMOTION_PLAN_SCHEMA_V1,
            schema_uri: "urn:ee:schema:causal-promotion-plan:v1",
            kind: "promotion_plan",
            title: "PromotionPlan",
            description: "Dry-run-first plan for promotion, demotion, archive, or quarantine decisions.",
            fields: PROMOTION_PLAN_FIELDS,
        },
    ]
}

#[must_use]
pub fn causal_schema_catalog_json() -> String {
    let schemas = causal_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!("  \"schema\": \"{CAUSAL_SCHEMA_CATALOG_V1}\",\n"));
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

    const CAUSAL_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/causal_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn causal_schema_constants_are_stable() -> TestResult {
        ensure(
            CAUSAL_EXPOSURE_SCHEMA_V1,
            "ee.causal.exposure.v1",
            "exposure",
        )?;
        ensure(
            DECISION_TRACE_SCHEMA_V1,
            "ee.causal.decision_trace.v1",
            "decision trace",
        )?;
        ensure(
            UPLIFT_ESTIMATE_SCHEMA_V1,
            "ee.causal.uplift_estimate.v1",
            "uplift",
        )?;
        ensure(
            CONFOUNDER_SCHEMA_V1,
            "ee.causal.confounder.v1",
            "confounder",
        )?;
        ensure(
            PROMOTION_PLAN_SCHEMA_V1,
            "ee.causal.promotion_plan.v1",
            "promotion plan",
        )?;
        ensure(CAUSAL_SCHEMA_CATALOG_V1, "ee.causal.schemas.v1", "catalog")
    }

    #[test]
    fn stable_wire_enums_round_trip() -> TestResult {
        for channel in CausalExposureChannel::all() {
            ensure(
                CausalExposureChannel::from_str(channel.as_str()),
                Ok(channel),
                "channel",
            )?;
        }
        for outcome in DecisionTraceOutcome::all() {
            ensure(
                DecisionTraceOutcome::from_str(outcome.as_str()),
                Ok(outcome),
                "outcome",
            )?;
        }
        for strength in CausalEvidenceStrength::all() {
            ensure(
                CausalEvidenceStrength::from_str(strength.as_str()),
                Ok(strength),
                "strength",
            )?;
        }
        for direction in UpliftDirection::all() {
            ensure(
                UpliftDirection::from_str(direction.as_str()),
                Ok(direction),
                "direction",
            )?;
        }
        for kind in ConfounderKind::all() {
            ensure(
                ConfounderKind::from_str(kind.as_str()),
                Ok(kind),
                "confounder kind",
            )?;
        }
        for action in PromotionAction::all() {
            ensure(
                PromotionAction::from_str(action.as_str()),
                Ok(action),
                "action",
            )?;
        }
        for status in PromotionPlanStatus::all() {
            ensure(
                PromotionPlanStatus::from_str(status.as_str()),
                Ok(status),
                "status",
            )?;
        }
        ensure(
            PromotionAction::from_str("ship").map_err(|error| error.field()),
            Err("promotion_action"),
            "invalid action field",
        )
    }

    #[test]
    fn causal_record_builders_set_schemas_and_defaults() -> TestResult {
        let exposure = CausalExposure::new(
            "cxp-001",
            "mem-release",
            "memory",
            "dec-001",
            CausalExposureChannel::ContextPack,
            "2026-04-30T12:00:00Z",
        )
        .with_rank(2)
        .with_policy("policy-default")
        .with_context_pack("pack-001")
        .with_trace("trace-001")
        .with_evidence("ev-001");
        ensure(
            exposure.schema,
            CAUSAL_EXPOSURE_SCHEMA_V1,
            "exposure schema",
        )?;
        ensure(exposure.rank, Some(2), "rank")?;

        let trace = CausalDecisionTrace::new(
            "dec-001",
            "trace-001",
            DecisionPlane::Packing,
            "2026-04-30T12:01:00Z",
            "codex",
            "Selected release memory for the pack.",
        )
        .with_outcome(DecisionTraceOutcome::Used)
        .with_task("task-001")
        .with_policy("policy-default")
        .with_exposed_artifact("mem-release")
        .with_selected_artifact("mem-release")
        .with_rejected_artifact("mem-old")
        .with_evidence("ev-002");
        ensure(trace.schema, DECISION_TRACE_SCHEMA_V1, "trace schema")?;
        ensure(trace.outcome, DecisionTraceOutcome::Used, "trace outcome")?;

        let estimate = UpliftEstimate::new(
            "uplift-001",
            "mem-release",
            "dec-001",
            "replay_fixture",
            "2026-04-30T12:02:00Z",
        )
        .with_rates(0.25, 0.7)
        .with_confidence(2.0)
        .with_sample_size(8)
        .with_evidence_strength(CausalEvidenceStrength::ReplaySupported)
        .with_exposure("cxp-001")
        .with_confounder("conf-001");
        ensure(
            estimate.schema,
            UPLIFT_ESTIMATE_SCHEMA_V1,
            "estimate schema",
        )?;
        ensure(estimate.confidence, 1.0, "confidence clamp")?;
        ensure(estimate.uplift, 0.44999999999999996, "uplift")?;
        ensure(estimate.direction, UpliftDirection::Positive, "direction")?;

        let confounder = CausalConfounder::new(
            "conf-001",
            ConfounderKind::TaskDifficulty,
            "Release tasks in the sample were easier than baseline.",
            "Stratify future estimates by task difficulty.",
        )
        .with_severity(1.7)
        .with_affected_artifact("mem-release")
        .with_affected_decision("dec-001")
        .with_evidence("ev-003");
        ensure(confounder.schema, CONFOUNDER_SCHEMA_V1, "confounder schema")?;
        ensure(confounder.severity, 1.0, "severity clamp")?;

        let plan = PromotionPlan::new(
            "prom-001",
            "mem-release",
            PromotionAction::Promote,
            "2026-04-30T12:03:00Z",
        )
        .with_status(PromotionPlanStatus::DryRunReady)
        .with_evidence_strength(CausalEvidenceStrength::ReplaySupported)
        .with_minimum_uplift(-2.0)
        .with_estimated_uplift(0.45)
        .with_required_evidence("ev-004")
        .with_blocking_confounder("conf-001")
        .with_audit_id("audit-001");
        ensure(plan.schema, PROMOTION_PLAN_SCHEMA_V1, "plan schema")?;
        ensure(plan.dry_run_first, true, "dry-run-first default")?;
        ensure(plan.minimum_uplift, -1.0, "minimum uplift clamp")
    }

    #[test]
    fn data_json_uses_stable_wire_names() -> TestResult {
        let estimate = UpliftEstimate::new(
            "uplift-001",
            "mem-release",
            "dec-001",
            "replay_fixture",
            "2026-04-30T12:02:00Z",
        )
        .with_rates(0.33333, 0.77777)
        .with_confidence(0.81234)
        .with_evidence_strength(CausalEvidenceStrength::ReplaySupported);

        let json = estimate.data_json();
        ensure(
            json.get("schema").and_then(serde_json::Value::as_str),
            Some(UPLIFT_ESTIMATE_SCHEMA_V1),
            "schema",
        )?;
        ensure(
            json.get("evidenceStrength")
                .and_then(serde_json::Value::as_str),
            Some("replay_supported"),
            "evidence strength",
        )?;
        ensure(
            json.get("uplift").and_then(serde_json::Value::as_f64),
            Some(0.444),
            "rounded uplift",
        )?;
        ensure(
            json.get("direction").and_then(serde_json::Value::as_str),
            Some("positive"),
            "direction",
        )
    }

    #[test]
    fn causal_schema_catalog_order_is_stable() -> TestResult {
        let schemas = causal_schemas();
        ensure(schemas.len(), 5, "schema count")?;
        ensure(
            schemas[0].schema_name,
            CAUSAL_EXPOSURE_SCHEMA_V1,
            "exposure",
        )?;
        ensure(
            schemas[1].schema_name,
            DECISION_TRACE_SCHEMA_V1,
            "decision trace",
        )?;
        ensure(schemas[2].schema_name, UPLIFT_ESTIMATE_SCHEMA_V1, "uplift")?;
        ensure(schemas[3].schema_name, CONFOUNDER_SCHEMA_V1, "confounder")?;
        ensure(
            schemas[4].schema_name,
            PROMOTION_PLAN_SCHEMA_V1,
            "promotion plan",
        )
    }

    #[test]
    fn causal_schema_catalog_matches_golden_fixture() {
        assert_eq!(causal_schema_catalog_json(), CAUSAL_SCHEMA_GOLDEN);
    }

    #[test]
    fn causal_schema_catalog_is_valid_json() -> TestResult {
        let parsed: serde_json::Value = serde_json::from_str(CAUSAL_SCHEMA_GOLDEN)
            .map_err(|error| format!("causal schema golden must be valid JSON: {error}"))?;
        ensure(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some(CAUSAL_SCHEMA_CATALOG_V1),
            "catalog schema",
        )?;
        let schemas = parsed
            .get("schemas")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "schemas must be an array".to_string())?;
        ensure(schemas.len(), 5, "catalog length")
    }
}
