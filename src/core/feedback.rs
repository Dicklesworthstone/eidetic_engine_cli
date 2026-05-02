//! Preflight and tripwire feedback loop (EE-394).
//!
//! Captures outcome signals from preflight close events and tripwire evaluations
//! to improve future risk assessments and counterfactual scoring.
//!
//! # Feedback Types
//!
//! - **Preflight outcome**: Task success/failure after preflight clearance
//! - **Tripwire false alarm**: Tripwire triggered but task succeeded anyway
//! - **Tripwire true positive**: Tripwire triggered and task failed
//! - **Tripwire miss**: Task failed but tripwire did not fire

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

/// Schema for feedback record.
pub const FEEDBACK_RECORD_SCHEMA_V1: &str = "ee.feedback.record.v1";

/// Schema for feedback summary.
pub const FEEDBACK_SUMMARY_SCHEMA_V1: &str = "ee.feedback.summary.v1";

/// ID prefix for feedback records.
pub const FEEDBACK_RECORD_ID_PREFIX: &str = "fb_";

/// Outcome of a task after preflight assessment.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    /// Task completed successfully.
    Success,
    /// Task failed.
    Failure,
    /// Task was cancelled.
    Cancelled,
    /// Outcome not yet known.
    #[default]
    Unknown,
}

impl TaskOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Success | Self::Failure | Self::Cancelled)
    }
}

impl FromStr for TaskOutcome {
    type Err = ParseTaskOutcomeError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match normalize_label(raw).as_str() {
            "success" | "succeeded" | "passed" => Ok(Self::Success),
            "failure" | "failed" | "error" => Ok(Self::Failure),
            "cancelled" | "canceled" => Ok(Self::Cancelled),
            "unknown" | "pending" => Ok(Self::Unknown),
            other => Err(ParseTaskOutcomeError {
                value: other.to_owned(),
            }),
        }
    }
}

/// Error returned when parsing a task outcome label fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTaskOutcomeError {
    value: String,
}

impl fmt::Display for ParseTaskOutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown task outcome '{}'", self.value)
    }
}

impl std::error::Error for ParseTaskOutcomeError {}

/// How a closed preflight warning should feed future scoring.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightFeedbackKind {
    /// The warning helped avoid or verify the risk.
    Helped,
    /// The warning missed the risk that actually mattered.
    Missed,
    /// The warning was based on stale evidence.
    StaleWarning,
    /// The warning consumed attention but the task outcome showed it was not needed.
    FalseAlarm,
    /// The close event should be retained without score movement.
    #[default]
    Neutral,
}

impl PreflightFeedbackKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Helped => "helped",
            Self::Missed => "missed",
            Self::StaleWarning => "stale_warning",
            Self::FalseAlarm => "false_alarm",
            Self::Neutral => "neutral",
        }
    }

    #[must_use]
    pub const fn signal(self) -> &'static str {
        match self {
            Self::Helped => "helpful",
            Self::Missed => "harmful",
            Self::StaleWarning => "stale",
            Self::FalseAlarm => "inaccurate",
            Self::Neutral => "neutral",
        }
    }
}

impl FromStr for PreflightFeedbackKind {
    type Err = ParsePreflightFeedbackKindError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match normalize_label(raw).as_str() {
            "helped" | "useful" | "confirmed" => Ok(Self::Helped),
            "missed" | "miss" => Ok(Self::Missed),
            "stale" | "stale_warning" => Ok(Self::StaleWarning),
            "false_alarm" | "false_positive" | "noisy" => Ok(Self::FalseAlarm),
            "neutral" | "none" => Ok(Self::Neutral),
            other => Err(ParsePreflightFeedbackKindError {
                value: other.to_owned(),
            }),
        }
    }
}

/// Error returned when parsing a preflight feedback kind fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsePreflightFeedbackKindError {
    value: String,
}

impl fmt::Display for ParsePreflightFeedbackKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown preflight feedback kind '{}'", self.value)
    }
}

impl std::error::Error for ParsePreflightFeedbackKindError {}

/// Classification of tripwire evaluation result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripwireEvaluation {
    /// Tripwire fired and task failed (correctly predicted).
    TruePositive,
    /// Tripwire fired but task succeeded (false alarm).
    FalseAlarm,
    /// Tripwire did not fire and task succeeded.
    TrueNegative,
    /// Tripwire did not fire but task failed (missed risk).
    Miss,
}

impl TripwireEvaluation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TruePositive => "true_positive",
            Self::FalseAlarm => "false_alarm",
            Self::TrueNegative => "true_negative",
            Self::Miss => "miss",
        }
    }

    #[must_use]
    pub const fn is_correct(self) -> bool {
        matches!(self, Self::TruePositive | Self::TrueNegative)
    }

    #[must_use]
    pub const fn affects_scoring(self) -> bool {
        matches!(self, Self::FalseAlarm | Self::Miss)
    }

    #[must_use]
    pub const fn signal(self) -> &'static str {
        match self {
            Self::TruePositive => "helpful",
            Self::FalseAlarm => "inaccurate",
            Self::TrueNegative => "confirmation",
            Self::Miss => "harmful",
        }
    }
}

/// Deterministic score movement preview generated from feedback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedbackScoreEffect {
    pub utility_delta: f64,
    pub confidence_delta: f64,
    pub false_alarm_delta: u32,
    pub priority_multiplier: f64,
    pub scoring_note: String,
}

impl FeedbackScoreEffect {
    #[must_use]
    pub fn new(
        utility_delta: f64,
        confidence_delta: f64,
        false_alarm_delta: u32,
        priority_multiplier: f64,
        scoring_note: impl Into<String>,
    ) -> Self {
        Self {
            utility_delta: round_delta(utility_delta),
            confidence_delta: round_delta(confidence_delta),
            false_alarm_delta,
            priority_multiplier: round_delta(priority_multiplier),
            scoring_note: scoring_note.into(),
        }
    }
}

/// Counterfactual replay hint generated from feedback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CounterfactualFeedbackEffect {
    pub regret_kind: Option<String>,
    pub intervention: String,
    pub evaluation_hint: String,
    pub curation_candidate: bool,
}

impl CounterfactualFeedbackEffect {
    #[must_use]
    pub fn new(
        regret_kind: Option<&str>,
        intervention: impl Into<String>,
        evaluation_hint: impl Into<String>,
        curation_candidate: bool,
    ) -> Self {
        Self {
            regret_kind: regret_kind.map(str::to_owned),
            intervention: intervention.into(),
            evaluation_hint: evaluation_hint.into(),
            curation_candidate,
        }
    }
}

/// A feedback record from preflight/tripwire evaluation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedbackRecord {
    pub schema: String,
    pub id: String,
    pub preflight_run_id: String,
    pub tripwire_id: Option<String>,
    pub task_outcome: TaskOutcome,
    pub evaluation: Option<TripwireEvaluation>,
    pub risk_level_predicted: String,
    pub confidence_delta: f64,
    pub notes: Option<String>,
    pub created_at: String,
}

impl FeedbackRecord {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        preflight_run_id: impl Into<String>,
        task_outcome: TaskOutcome,
    ) -> Self {
        Self {
            schema: FEEDBACK_RECORD_SCHEMA_V1.to_owned(),
            id: id.into(),
            preflight_run_id: preflight_run_id.into(),
            tripwire_id: None,
            task_outcome,
            evaluation: None,
            risk_level_predicted: "unknown".to_string(),
            confidence_delta: 0.0,
            notes: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn with_tripwire(mut self, tripwire_id: impl Into<String>) -> Self {
        self.tripwire_id = Some(tripwire_id.into());
        self
    }

    #[must_use]
    pub fn with_evaluation(mut self, eval: TripwireEvaluation) -> Self {
        self.evaluation = Some(eval);
        self
    }

    #[must_use]
    pub fn with_risk_level(mut self, level: impl Into<String>) -> Self {
        self.risk_level_predicted = level.into();
        self
    }

    #[must_use]
    pub fn with_confidence_delta(mut self, delta: f64) -> Self {
        self.confidence_delta = delta;
        self
    }

    #[must_use]
    pub fn with_notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = Some(notes.into());
        self
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Options for recording preflight outcome feedback.
#[derive(Clone, Debug, Default)]
pub struct RecordOutcomeOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Preflight run ID.
    pub preflight_run_id: String,
    /// Task outcome.
    pub task_outcome: TaskOutcome,
    /// Feedback class assigned to the closed preflight warning.
    pub feedback_kind: PreflightFeedbackKind,
    /// Optional notes about the outcome.
    pub notes: Option<String>,
    /// Dry-run mode.
    pub dry_run: bool,
}

/// Options for recording tripwire feedback.
#[derive(Clone, Debug, Default)]
pub struct RecordTripwireFeedbackOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Preflight run ID.
    pub preflight_run_id: String,
    /// Tripwire ID.
    pub tripwire_id: String,
    /// Whether the tripwire fired.
    pub tripwire_fired: bool,
    /// Task outcome.
    pub task_outcome: TaskOutcome,
    /// Optional notes.
    pub notes: Option<String>,
    /// Dry-run mode.
    pub dry_run: bool,
}

/// Report from recording feedback.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordFeedbackReport {
    pub schema: String,
    pub record_status: String,
    pub record_id: Option<String>,
    pub target_type: String,
    pub target_id: String,
    pub preflight_run_id: String,
    pub tripwire_id: Option<String>,
    pub task_outcome: String,
    pub evaluation: Option<String>,
    pub feedback_kind: Option<String>,
    pub signal: String,
    pub confidence_adjustment: f64,
    pub utility_adjustment: f64,
    pub false_alarm_cost_delta: u32,
    pub score_effect: FeedbackScoreEffect,
    pub counterfactual_effect: CounterfactualFeedbackEffect,
    pub curation_candidates_generated: usize,
    pub durable_mutation: bool,
    pub evidence_preserved: bool,
    pub dry_run: bool,
    pub recorded_at: String,
}

impl RecordFeedbackReport {
    #[must_use]
    pub fn new(
        record_id: Option<String>,
        preflight_run_id: impl Into<String>,
        target_type: impl Into<String>,
        target_id: impl Into<String>,
    ) -> Self {
        Self {
            schema: FEEDBACK_RECORD_SCHEMA_V1.to_owned(),
            record_status: "evaluated".to_owned(),
            record_id,
            target_type: target_type.into(),
            target_id: target_id.into(),
            preflight_run_id: preflight_run_id.into(),
            tripwire_id: None,
            task_outcome: TaskOutcome::Unknown.as_str().to_string(),
            evaluation: None,
            feedback_kind: None,
            signal: "neutral".to_owned(),
            confidence_adjustment: 0.0,
            utility_adjustment: 0.0,
            false_alarm_cost_delta: 0,
            score_effect: FeedbackScoreEffect::new(
                0.0,
                0.0,
                0,
                1.0,
                "No score movement requested.",
            ),
            counterfactual_effect: CounterfactualFeedbackEffect::new(
                None,
                "retain_observation",
                "No counterfactual replay candidate generated.",
                false,
            ),
            curation_candidates_generated: 0,
            durable_mutation: false,
            evidence_preserved: true,
            dry_run: false,
            recorded_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Summary statistics for feedback signals.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FeedbackSummary {
    pub schema: String,
    pub total_records: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub cancelled_count: usize,
    pub true_positive_count: usize,
    pub false_alarm_count: usize,
    pub true_negative_count: usize,
    pub miss_count: usize,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub f1_score: Option<f64>,
    pub summarized_at: String,
}

impl FeedbackSummary {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema: FEEDBACK_SUMMARY_SCHEMA_V1.to_owned(),
            summarized_at: Utc::now().to_rfc3339(),
            ..Default::default()
        }
    }

    pub fn compute_metrics(&mut self) {
        let tp = self.true_positive_count as f64;
        let fp = self.false_alarm_count as f64;
        let fn_ = self.miss_count as f64;

        if tp + fp > 0.0 {
            self.precision = Some(tp / (tp + fp));
        }
        if tp + fn_ > 0.0 {
            self.recall = Some(tp / (tp + fn_));
        }
        if let (Some(p), Some(r)) = (self.precision, self.recall) {
            if p + r > 0.0 {
                self.f1_score = Some(2.0 * p * r / (p + r));
            }
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Record preflight outcome feedback.
pub fn record_preflight_outcome(
    options: &RecordOutcomeOptions,
) -> Result<RecordFeedbackReport, DomainError> {
    require_identifier("preflight run", &options.preflight_run_id)?;
    let record_id =
        (!options.dry_run).then(|| format!("{}{}", FEEDBACK_RECORD_ID_PREFIX, generate_id()));
    let mut report = RecordFeedbackReport::new(
        record_id,
        &options.preflight_run_id,
        "preflight_run",
        &options.preflight_run_id,
    );
    report.task_outcome = options.task_outcome.as_str().to_string();
    report.feedback_kind = Some(options.feedback_kind.as_str().to_string());
    report.signal = options.feedback_kind.signal().to_string();
    report.dry_run = options.dry_run;
    report.record_status = if options.dry_run {
        "dry_run".to_owned()
    } else {
        "evaluated".to_owned()
    };

    let (score_effect, counterfactual_effect) =
        effects_for_preflight_feedback(options.feedback_kind, options.task_outcome);
    apply_effects(&mut report, score_effect, counterfactual_effect);

    Ok(report)
}

/// Record tripwire feedback.
pub fn record_tripwire_feedback(
    options: &RecordTripwireFeedbackOptions,
) -> Result<RecordFeedbackReport, DomainError> {
    require_identifier("preflight run", &options.preflight_run_id)?;
    require_identifier("tripwire", &options.tripwire_id)?;
    let record_id =
        (!options.dry_run).then(|| format!("{}{}", FEEDBACK_RECORD_ID_PREFIX, generate_id()));
    let mut report = RecordFeedbackReport::new(
        record_id,
        &options.preflight_run_id,
        "tripwire",
        &options.tripwire_id,
    );
    report.tripwire_id = Some(options.tripwire_id.clone());
    report.task_outcome = options.task_outcome.as_str().to_string();
    report.dry_run = options.dry_run;
    report.record_status = if options.dry_run {
        "dry_run".to_owned()
    } else {
        "evaluated".to_owned()
    };

    let evaluation = classify_tripwire_result(options.tripwire_fired, options.task_outcome);
    report.evaluation = Some(evaluation.as_str().to_string());
    report.signal = evaluation.signal().to_string();

    let (score_effect, counterfactual_effect) = effects_for_tripwire_evaluation(evaluation);
    apply_effects(&mut report, score_effect, counterfactual_effect);

    Ok(report)
}

/// Infer preflight feedback from a close decision and observed task outcome.
#[must_use]
pub const fn infer_preflight_feedback_kind(
    cleared: bool,
    outcome: TaskOutcome,
) -> PreflightFeedbackKind {
    match (cleared, outcome) {
        (true, TaskOutcome::Success) => PreflightFeedbackKind::Helped,
        (true, TaskOutcome::Failure) => PreflightFeedbackKind::Missed,
        (true, TaskOutcome::Cancelled | TaskOutcome::Unknown) => PreflightFeedbackKind::Neutral,
        (false, TaskOutcome::Success) => PreflightFeedbackKind::FalseAlarm,
        (false, TaskOutcome::Failure | TaskOutcome::Cancelled) => PreflightFeedbackKind::Helped,
        (false, TaskOutcome::Unknown) => PreflightFeedbackKind::Neutral,
    }
}

/// Classify tripwire result based on firing and outcome.
#[must_use]
pub fn classify_tripwire_result(fired: bool, outcome: TaskOutcome) -> TripwireEvaluation {
    match (fired, outcome) {
        (true, TaskOutcome::Failure) => TripwireEvaluation::TruePositive,
        (true, TaskOutcome::Success) => TripwireEvaluation::FalseAlarm,
        (true, TaskOutcome::Cancelled) => TripwireEvaluation::TruePositive,
        (false, TaskOutcome::Success) => TripwireEvaluation::TrueNegative,
        (false, TaskOutcome::Failure) => TripwireEvaluation::Miss,
        (false, TaskOutcome::Cancelled) => TripwireEvaluation::TrueNegative,
        (_, TaskOutcome::Unknown) => TripwireEvaluation::TrueNegative,
    }
}

/// Aggregate feedback records into summary statistics.
pub fn summarize_feedback(records: &[FeedbackRecord]) -> FeedbackSummary {
    let mut summary = FeedbackSummary::new();
    summary.total_records = records.len();

    for record in records {
        match record.task_outcome {
            TaskOutcome::Success => summary.success_count += 1,
            TaskOutcome::Failure => summary.failure_count += 1,
            TaskOutcome::Cancelled => summary.cancelled_count += 1,
            TaskOutcome::Unknown => {}
        }

        if let Some(eval) = record.evaluation {
            match eval {
                TripwireEvaluation::TruePositive => summary.true_positive_count += 1,
                TripwireEvaluation::FalseAlarm => summary.false_alarm_count += 1,
                TripwireEvaluation::TrueNegative => summary.true_negative_count += 1,
                TripwireEvaluation::Miss => summary.miss_count += 1,
            }
        }
    }

    summary.compute_metrics();
    summary
}

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}", timestamp & 0xFFFFFFFF)
}

fn effects_for_preflight_feedback(
    feedback_kind: PreflightFeedbackKind,
    task_outcome: TaskOutcome,
) -> (FeedbackScoreEffect, CounterfactualFeedbackEffect) {
    match feedback_kind {
        PreflightFeedbackKind::Helped => (
            FeedbackScoreEffect::new(
                0.08,
                0.05,
                0,
                1.05,
                "Preflight warning was useful; future matching warnings receive a small boost.",
            ),
            CounterfactualFeedbackEffect::new(
                None,
                "confirm_preflight_warning",
                format!(
                    "Observed {} outcome supports keeping this warning available.",
                    task_outcome.as_str()
                ),
                false,
            ),
        ),
        PreflightFeedbackKind::Missed => (
            FeedbackScoreEffect::new(
                -0.12,
                -0.10,
                0,
                1.15,
                "Preflight missed the observed risk; future replay should search for missing evidence.",
            ),
            CounterfactualFeedbackEffect::new(
                Some("missed"),
                "replay_with_missing_risk_evidence",
                "Counterfactual evaluation should test which memory would have surfaced the missed risk.",
                true,
            ),
        ),
        PreflightFeedbackKind::StaleWarning => (
            FeedbackScoreEffect::new(
                -0.06,
                -0.05,
                0,
                0.85,
                "Warning used stale evidence; reduce confidence until refreshed.",
            ),
            CounterfactualFeedbackEffect::new(
                Some("stale"),
                "refresh_or_replace_stale_warning",
                "Counterfactual evaluation should compare current evidence against the stale warning.",
                true,
            ),
        ),
        PreflightFeedbackKind::FalseAlarm => (
            FeedbackScoreEffect::new(
                -0.15,
                -0.12,
                1,
                0.75,
                "Warning was a false alarm; increase false-alarm cost without deleting evidence.",
            ),
            CounterfactualFeedbackEffect::new(
                Some("noisy"),
                "demote_repeated_false_alarm",
                "Counterfactual evaluation should test whether suppressing this warning preserves the successful outcome.",
                true,
            ),
        ),
        PreflightFeedbackKind::Neutral => (
            FeedbackScoreEffect::new(
                0.0,
                0.0,
                0,
                1.0,
                "Neutral close retained for audit without score movement.",
            ),
            CounterfactualFeedbackEffect::new(
                None,
                "retain_observation",
                "No counterfactual replay candidate generated.",
                false,
            ),
        ),
    }
}

fn effects_for_tripwire_evaluation(
    evaluation: TripwireEvaluation,
) -> (FeedbackScoreEffect, CounterfactualFeedbackEffect) {
    match evaluation {
        TripwireEvaluation::TruePositive => (
            FeedbackScoreEffect::new(
                0.10,
                0.08,
                0,
                1.08,
                "Tripwire predicted a real risk; preserve priority.",
            ),
            CounterfactualFeedbackEffect::new(
                None,
                "confirm_tripwire",
                "Counterfactual evaluation can use this as positive tripwire evidence.",
                false,
            ),
        ),
        TripwireEvaluation::FalseAlarm => (
            FeedbackScoreEffect::new(
                -0.15,
                -0.12,
                1,
                0.75,
                "Tripwire fired but task succeeded; increase false-alarm cost without deleting evidence.",
            ),
            CounterfactualFeedbackEffect::new(
                Some("noisy"),
                "demote_repeated_false_alarm",
                "Replay should test whether suppressing this tripwire preserves the successful outcome.",
                true,
            ),
        ),
        TripwireEvaluation::TrueNegative => (
            FeedbackScoreEffect::new(
                0.02,
                0.01,
                0,
                1.0,
                "Tripwire stayed quiet during a successful outcome.",
            ),
            CounterfactualFeedbackEffect::new(
                None,
                "retain_observation",
                "No counterfactual replay candidate generated.",
                false,
            ),
        ),
        TripwireEvaluation::Miss => (
            FeedbackScoreEffect::new(
                -0.20,
                -0.15,
                0,
                1.20,
                "Tripwire missed a failed outcome; generate a replay candidate.",
            ),
            CounterfactualFeedbackEffect::new(
                Some("missed"),
                "generate_missing_tripwire_candidate",
                "Replay should identify evidence that would have fired before the failure.",
                true,
            ),
        ),
    }
}

fn apply_effects(
    report: &mut RecordFeedbackReport,
    score_effect: FeedbackScoreEffect,
    counterfactual_effect: CounterfactualFeedbackEffect,
) {
    report.confidence_adjustment = score_effect.confidence_delta;
    report.utility_adjustment = score_effect.utility_delta;
    report.false_alarm_cost_delta = score_effect.false_alarm_delta;
    report.curation_candidates_generated = usize::from(counterfactual_effect.curation_candidate);
    report.score_effect = score_effect;
    report.counterfactual_effect = counterfactual_effect;
}

fn require_identifier(label: &str, value: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(DomainError::Usage {
            message: format!("{label} id must not be empty"),
            repair: Some("provide a stable id".to_owned()),
        });
    }
    Ok(())
}

fn normalize_label(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('-', "_")
}

fn round_delta(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn task_outcome_strings_stable() {
        assert_eq!(TaskOutcome::Success.as_str(), "success");
        assert_eq!(TaskOutcome::Failure.as_str(), "failure");
        assert_eq!(TaskOutcome::Cancelled.as_str(), "cancelled");
        assert_eq!(TaskOutcome::Unknown.as_str(), "unknown");
    }

    #[test]
    fn task_outcome_is_terminal() {
        assert!(TaskOutcome::Success.is_terminal());
        assert!(TaskOutcome::Failure.is_terminal());
        assert!(TaskOutcome::Cancelled.is_terminal());
        assert!(!TaskOutcome::Unknown.is_terminal());
    }

    #[test]
    fn tripwire_evaluation_strings_stable() {
        assert_eq!(TripwireEvaluation::TruePositive.as_str(), "true_positive");
        assert_eq!(TripwireEvaluation::FalseAlarm.as_str(), "false_alarm");
        assert_eq!(TripwireEvaluation::TrueNegative.as_str(), "true_negative");
        assert_eq!(TripwireEvaluation::Miss.as_str(), "miss");
    }

    #[test]
    fn preflight_feedback_kind_strings_stable() {
        assert_eq!(PreflightFeedbackKind::Helped.as_str(), "helped");
        assert_eq!(PreflightFeedbackKind::Missed.as_str(), "missed");
        assert_eq!(
            PreflightFeedbackKind::StaleWarning.as_str(),
            "stale_warning"
        );
        assert_eq!(PreflightFeedbackKind::FalseAlarm.as_str(), "false_alarm");
        assert_eq!(PreflightFeedbackKind::Neutral.as_str(), "neutral");
    }

    #[test]
    fn parse_feedback_kind_accepts_cli_aliases() -> TestResult {
        ensure(
            "false-alarm"
                .parse::<PreflightFeedbackKind>()
                .map_err(|error| error.to_string())?,
            PreflightFeedbackKind::FalseAlarm,
            "false alarm alias",
        )?;
        ensure(
            "stale"
                .parse::<PreflightFeedbackKind>()
                .map_err(|error| error.to_string())?,
            PreflightFeedbackKind::StaleWarning,
            "stale alias",
        )
    }

    #[test]
    fn infer_preflight_feedback_from_close_outcome() {
        assert_eq!(
            infer_preflight_feedback_kind(false, TaskOutcome::Success),
            PreflightFeedbackKind::FalseAlarm
        );
        assert_eq!(
            infer_preflight_feedback_kind(true, TaskOutcome::Failure),
            PreflightFeedbackKind::Missed
        );
        assert_eq!(
            infer_preflight_feedback_kind(false, TaskOutcome::Failure),
            PreflightFeedbackKind::Helped
        );
    }

    #[test]
    fn tripwire_evaluation_is_correct() {
        assert!(TripwireEvaluation::TruePositive.is_correct());
        assert!(TripwireEvaluation::TrueNegative.is_correct());
        assert!(!TripwireEvaluation::FalseAlarm.is_correct());
        assert!(!TripwireEvaluation::Miss.is_correct());
    }

    #[test]
    fn tripwire_evaluation_affects_scoring() {
        assert!(!TripwireEvaluation::TruePositive.affects_scoring());
        assert!(!TripwireEvaluation::TrueNegative.affects_scoring());
        assert!(TripwireEvaluation::FalseAlarm.affects_scoring());
        assert!(TripwireEvaluation::Miss.affects_scoring());
    }

    #[test]
    fn classify_tripwire_true_positive() {
        let eval = classify_tripwire_result(true, TaskOutcome::Failure);
        assert_eq!(eval, TripwireEvaluation::TruePositive);
    }

    #[test]
    fn classify_tripwire_false_alarm() {
        let eval = classify_tripwire_result(true, TaskOutcome::Success);
        assert_eq!(eval, TripwireEvaluation::FalseAlarm);
    }

    #[test]
    fn classify_tripwire_true_negative() {
        let eval = classify_tripwire_result(false, TaskOutcome::Success);
        assert_eq!(eval, TripwireEvaluation::TrueNegative);
    }

    #[test]
    fn classify_tripwire_miss() {
        let eval = classify_tripwire_result(false, TaskOutcome::Failure);
        assert_eq!(eval, TripwireEvaluation::Miss);
    }

    #[test]
    fn record_preflight_outcome_success() -> TestResult {
        let options = RecordOutcomeOptions {
            workspace: PathBuf::from("."),
            preflight_run_id: "pfl_test".to_string(),
            task_outcome: TaskOutcome::Success,
            feedback_kind: PreflightFeedbackKind::Helped,
            dry_run: false,
            ..Default::default()
        };

        let report = record_preflight_outcome(&options).map_err(|e| e.message())?;

        ensure(report.task_outcome, "success".to_string(), "outcome")?;
        ensure(
            report.confidence_adjustment > 0.0,
            true,
            "positive adjustment",
        )
    }

    #[test]
    fn record_preflight_outcome_failure() -> TestResult {
        let options = RecordOutcomeOptions {
            workspace: PathBuf::from("."),
            preflight_run_id: "pfl_test".to_string(),
            task_outcome: TaskOutcome::Failure,
            feedback_kind: PreflightFeedbackKind::Missed,
            dry_run: false,
            ..Default::default()
        };

        let report = record_preflight_outcome(&options).map_err(|e| e.message())?;

        ensure(report.task_outcome, "failure".to_string(), "outcome")?;
        ensure(
            report.confidence_adjustment < 0.0,
            true,
            "negative adjustment",
        )?;
        ensure(
            report.counterfactual_effect.regret_kind,
            Some("missed".to_owned()),
            "regret kind",
        )?;
        ensure(
            report.curation_candidates_generated,
            1,
            "generates curation candidate",
        )
    }

    #[test]
    fn record_tripwire_feedback_false_alarm() -> TestResult {
        let options = RecordTripwireFeedbackOptions {
            workspace: PathBuf::from("."),
            preflight_run_id: "pfl_test".to_string(),
            tripwire_id: "tw_001".to_string(),
            tripwire_fired: true,
            task_outcome: TaskOutcome::Success,
            dry_run: false,
            ..Default::default()
        };

        let report = record_tripwire_feedback(&options).map_err(|e| e.message())?;

        ensure(
            report.evaluation,
            Some("false_alarm".to_string()),
            "evaluation",
        )?;
        ensure(
            report.confidence_adjustment < 0.0,
            true,
            "negative adjustment for false alarm",
        )?;
        ensure(
            report.curation_candidates_generated,
            1,
            "generates curation candidate",
        )
    }

    #[test]
    fn record_tripwire_feedback_true_positive() -> TestResult {
        let options = RecordTripwireFeedbackOptions {
            workspace: PathBuf::from("."),
            preflight_run_id: "pfl_test".to_string(),
            tripwire_id: "tw_001".to_string(),
            tripwire_fired: true,
            task_outcome: TaskOutcome::Failure,
            dry_run: false,
            ..Default::default()
        };

        let report = record_tripwire_feedback(&options).map_err(|e| e.message())?;

        ensure(
            report.evaluation,
            Some("true_positive".to_string()),
            "evaluation",
        )?;
        ensure(
            report.confidence_adjustment > 0.0,
            true,
            "positive adjustment for true positive",
        )?;
        ensure(
            report.curation_candidates_generated,
            0,
            "no curation candidate for correct prediction",
        )
    }

    #[test]
    fn record_tripwire_feedback_miss() -> TestResult {
        let options = RecordTripwireFeedbackOptions {
            workspace: PathBuf::from("."),
            preflight_run_id: "pfl_test".to_string(),
            tripwire_id: "tw_001".to_string(),
            tripwire_fired: false,
            task_outcome: TaskOutcome::Failure,
            dry_run: false,
            ..Default::default()
        };

        let report = record_tripwire_feedback(&options).map_err(|e| e.message())?;

        ensure(report.evaluation, Some("miss".to_string()), "evaluation")?;
        ensure(
            report.confidence_adjustment < 0.0,
            true,
            "negative adjustment for miss",
        )?;
        ensure(
            report.curation_candidates_generated,
            1,
            "generates curation candidate for miss",
        )
    }

    #[test]
    fn feedback_record_builder() {
        let record = FeedbackRecord::new("fb_001", "pfl_001", TaskOutcome::Success)
            .with_tripwire("tw_001")
            .with_evaluation(TripwireEvaluation::FalseAlarm)
            .with_risk_level("high")
            .with_confidence_delta(-0.15)
            .with_notes("Task succeeded despite risk");

        assert_eq!(record.id, "fb_001");
        assert_eq!(record.preflight_run_id, "pfl_001");
        assert_eq!(record.tripwire_id, Some("tw_001".to_string()));
        assert_eq!(record.evaluation, Some(TripwireEvaluation::FalseAlarm));
        assert_eq!(record.risk_level_predicted, "high");
        assert!(record.confidence_delta < 0.0);
        assert!(record.notes.is_some());
    }

    #[test]
    fn summarize_feedback_computes_metrics() {
        let records = vec![
            FeedbackRecord::new("fb_1", "pfl_1", TaskOutcome::Failure)
                .with_evaluation(TripwireEvaluation::TruePositive),
            FeedbackRecord::new("fb_2", "pfl_2", TaskOutcome::Success)
                .with_evaluation(TripwireEvaluation::FalseAlarm),
            FeedbackRecord::new("fb_3", "pfl_3", TaskOutcome::Success)
                .with_evaluation(TripwireEvaluation::TrueNegative),
            FeedbackRecord::new("fb_4", "pfl_4", TaskOutcome::Failure)
                .with_evaluation(TripwireEvaluation::Miss),
        ];

        let summary = summarize_feedback(&records);

        assert_eq!(summary.total_records, 4);
        assert_eq!(summary.true_positive_count, 1);
        assert_eq!(summary.false_alarm_count, 1);
        assert_eq!(summary.true_negative_count, 1);
        assert_eq!(summary.miss_count, 1);

        assert!(summary.precision.is_some());
        assert!(summary.recall.is_some());
        assert!(summary.f1_score.is_some());
    }

    #[test]
    fn feedback_record_json_valid() {
        let record = FeedbackRecord::new("fb_test", "pfl_test", TaskOutcome::Success);
        let json = record.to_json();

        assert!(json.contains(FEEDBACK_RECORD_SCHEMA_V1));
        assert!(json.contains("fb_test"));
    }

    #[test]
    fn feedback_summary_json_valid() {
        let summary = FeedbackSummary::new();
        let json = summary.to_json();

        assert!(json.contains(FEEDBACK_SUMMARY_SCHEMA_V1));
    }

    #[test]
    fn dry_run_skips_adjustments() -> TestResult {
        let options = RecordOutcomeOptions {
            workspace: PathBuf::from("."),
            preflight_run_id: "pfl_test".to_string(),
            task_outcome: TaskOutcome::Failure,
            feedback_kind: PreflightFeedbackKind::Missed,
            dry_run: true,
            ..Default::default()
        };

        let report = record_preflight_outcome(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry run flag")?;
        ensure(report.record_id, None, "no record id in dry run")?;
        ensure(report.durable_mutation, false, "no durable mutation")?;
        ensure(
            report.confidence_adjustment < 0.0,
            true,
            "dry run previews adjustment",
        )?;
        ensure(
            report.curation_candidates_generated,
            1,
            "dry run previews candidates",
        )
    }
}
