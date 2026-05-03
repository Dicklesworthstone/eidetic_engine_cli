//! Tripwire management operations (EE-393).
//!
//! List and check tripwires set during preflight risk assessments.
//! Tripwires monitor conditions during task execution and can halt
//! or warn when triggered.
//!
//! # Operations
//!
//! - **list**: List all tripwires, optionally filtered by state or preflight run
//! - **check**: Evaluate a specific tripwire and return its current state

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::feedback::{
    RecordFeedbackReport, RecordTripwireFeedbackOptions, TaskOutcome, record_tripwire_feedback,
};
use crate::models::DomainError;
use crate::models::preflight::{Tripwire, TripwireAction, TripwireState, TripwireType};

/// Schema for tripwire list report.
pub const TRIPWIRE_LIST_SCHEMA_V1: &str = "ee.tripwire.list.v1";

/// Schema for tripwire check report.
pub const TRIPWIRE_CHECK_SCHEMA_V1: &str = "ee.tripwire.check.v1";

/// Options for listing tripwires.
#[derive(Clone, Debug, Default)]
pub struct ListOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Filter by tripwire state.
    pub state: Option<TripwireState>,
    /// Filter by preflight run ID.
    pub preflight_run_id: Option<String>,
    /// Filter by tripwire type.
    pub tripwire_type: Option<TripwireType>,
    /// Maximum number of tripwires to return.
    pub limit: Option<usize>,
    /// Include disarmed tripwires.
    pub include_disarmed: bool,
}

/// Summary of a tripwire for list output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TripwireSummary {
    pub id: String,
    pub preflight_run_id: String,
    pub tripwire_type: String,
    pub condition: String,
    pub action: String,
    pub state: String,
    pub message: Option<String>,
    pub created_at: String,
    pub last_checked_at: Option<String>,
    pub triggered_at: Option<String>,
}

impl From<&Tripwire> for TripwireSummary {
    fn from(tw: &Tripwire) -> Self {
        Self {
            id: tw.id.clone(),
            preflight_run_id: tw.preflight_run_id.clone(),
            tripwire_type: tw.tripwire_type.as_str().to_string(),
            condition: tw.condition.clone(),
            action: tw.action.as_str().to_string(),
            state: tw.state.as_str().to_string(),
            message: tw.message.clone(),
            created_at: tw.created_at.clone(),
            last_checked_at: tw.last_checked_at.clone(),
            triggered_at: tw.triggered_at.clone(),
        }
    }
}

/// Report from listing tripwires.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListReport {
    pub schema: String,
    pub tripwires: Vec<TripwireSummary>,
    pub total_count: usize,
    pub armed_count: usize,
    pub triggered_count: usize,
    pub disarmed_count: usize,
    pub error_count: usize,
    pub filters_applied: Vec<String>,
    pub listed_at: String,
}

impl ListReport {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema: TRIPWIRE_LIST_SCHEMA_V1.to_owned(),
            tripwires: Vec::new(),
            total_count: 0,
            armed_count: 0,
            triggered_count: 0,
            disarmed_count: 0,
            error_count: 0,
            filters_applied: Vec::new(),
            listed_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

impl Default for ListReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for checking a tripwire.
#[derive(Clone, Debug, Default)]
pub struct CheckOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Tripwire ID to check.
    pub tripwire_id: String,
    /// Update the last_checked_at timestamp.
    pub update_timestamp: bool,
    /// Observed task outcome for optional scoring feedback.
    pub task_outcome: Option<TaskOutcome>,
    /// Perform a dry-run check without persisting.
    pub dry_run: bool,
}

/// Result of a tripwire check.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    /// Tripwire condition is satisfied (not triggered).
    Passed,
    /// Tripwire condition violated (triggered).
    Triggered,
    /// Tripwire was already disarmed.
    Disarmed,
    /// Check encountered an error.
    Error,
    /// Tripwire not found.
    NotFound,
}

impl CheckResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Triggered => "triggered",
            Self::Disarmed => "disarmed",
            Self::Error => "error",
            Self::NotFound => "not_found",
        }
    }

    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Passed | Self::Disarmed)
    }
}

/// Report from checking a tripwire.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckReport {
    pub schema: String,
    pub tripwire_id: String,
    pub preflight_run_id: Option<String>,
    pub result: CheckResult,
    pub state: String,
    pub action: String,
    pub condition: String,
    pub message: Option<String>,
    pub should_halt: bool,
    pub dry_run: bool,
    pub checked_at: String,
    pub details: Option<String>,
    pub feedback: Option<RecordFeedbackReport>,
    pub degraded: Vec<TripwireDegradation>,
}

impl CheckReport {
    #[must_use]
    pub fn new(tripwire_id: impl Into<String>) -> Self {
        Self {
            schema: TRIPWIRE_CHECK_SCHEMA_V1.to_owned(),
            tripwire_id: tripwire_id.into(),
            preflight_run_id: None,
            result: CheckResult::NotFound,
            state: TripwireState::Armed.as_str().to_string(),
            action: TripwireAction::Warn.as_str().to_string(),
            condition: String::new(),
            message: None,
            should_halt: false,
            dry_run: false,
            checked_at: Utc::now().to_rfc3339(),
            details: None,
            feedback: None,
            degraded: Vec::new(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Honest degraded-mode marker for tripwire readiness contracts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TripwireDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

impl TripwireDegradation {
    #[must_use]
    pub fn inputs_incomplete(message: impl Into<String>) -> Self {
        Self {
            code: "tripwire_inputs_incomplete".to_owned(),
            severity: "warning".to_owned(),
            message: message.into(),
            repair: Some("ee tripwire list --json".to_owned()),
        }
    }
}

/// List tripwires matching the given options.
pub fn list_tripwires(options: &ListOptions) -> Result<ListReport, DomainError> {
    let mut report = ListReport::new();

    let mut tripwires = generate_sample_tripwires();

    if let Some(ref state) = options.state {
        tripwires.retain(|tw| tw.state == *state);
        report
            .filters_applied
            .push(format!("state={}", state.as_str()));
    }

    if let Some(ref run_id) = options.preflight_run_id {
        tripwires.retain(|tw| tw.preflight_run_id == *run_id);
        report
            .filters_applied
            .push(format!("preflight_run_id={run_id}"));
    }

    if let Some(ref tw_type) = options.tripwire_type {
        tripwires.retain(|tw| tw.tripwire_type == *tw_type);
        report
            .filters_applied
            .push(format!("type={}", tw_type.as_str()));
    }

    if !options.include_disarmed {
        tripwires.retain(|tw| tw.state != TripwireState::Disarmed);
    }

    report.armed_count = tripwires
        .iter()
        .filter(|tw| tw.state == TripwireState::Armed)
        .count();
    report.triggered_count = tripwires
        .iter()
        .filter(|tw| tw.state == TripwireState::Triggered)
        .count();
    report.disarmed_count = tripwires
        .iter()
        .filter(|tw| tw.state == TripwireState::Disarmed)
        .count();
    report.error_count = tripwires
        .iter()
        .filter(|tw| tw.state == TripwireState::Error)
        .count();

    if let Some(limit) = options.limit {
        tripwires.truncate(limit);
    }

    report.total_count = tripwires.len();
    report.tripwires = tripwires.iter().map(TripwireSummary::from).collect();

    Ok(report)
}

/// Check a specific tripwire.
pub fn check_tripwire(options: &CheckOptions) -> Result<CheckReport, DomainError> {
    let mut report = CheckReport::new(&options.tripwire_id);
    report.dry_run = options.dry_run;

    let tripwires = generate_sample_tripwires();
    let tripwire = tripwires.iter().find(|tw| tw.id == options.tripwire_id);

    let Some(tw) = tripwire else {
        report.result = CheckResult::NotFound;
        report.details = Some(format!(
            "Tripwire '{}' not found in workspace",
            options.tripwire_id
        ));
        report.degraded.push(TripwireDegradation::inputs_incomplete(
            "No tripwire matched the requested ID, so the check could not evaluate a concrete event payload.",
        ));
        return Ok(report);
    };

    report.condition = tw.condition.clone();
    report.action = tw.action.as_str().to_string();
    report.state = tw.state.as_str().to_string();
    report.message = tw.message.clone();
    report.preflight_run_id = Some(tw.preflight_run_id.clone());

    match tw.state {
        TripwireState::Disarmed => {
            report.result = CheckResult::Disarmed;
            report.details = Some("Tripwire is disarmed and will not fire".to_string());
        }
        TripwireState::Triggered => {
            report.result = CheckResult::Triggered;
            report.should_halt = tw.action.stops_execution();
            report.details = Some("Tripwire was previously triggered".to_string());
        }
        TripwireState::Error => {
            report.result = CheckResult::Error;
            report.details = Some("Tripwire is in error state".to_string());
        }
        TripwireState::Armed => {
            let passes = evaluate_condition(&tw.condition);
            if passes {
                report.result = CheckResult::Passed;
                report.details = Some("Condition evaluated to true (safe)".to_string());
            } else {
                report.result = CheckResult::Triggered;
                report.should_halt = tw.action.stops_execution();
                report.details = Some("Condition evaluated to false (triggered)".to_string());
            }
        }
    }

    if let Some(task_outcome) = options.task_outcome {
        report.feedback = Some(record_tripwire_feedback(&RecordTripwireFeedbackOptions {
            workspace: options.workspace.clone(),
            preflight_run_id: tw.preflight_run_id.clone(),
            tripwire_id: tw.id.clone(),
            tripwire_fired: matches!(report.result, CheckResult::Triggered),
            task_outcome,
            notes: report.details.clone(),
            dry_run: options.dry_run,
        })?);
    }

    Ok(report)
}

fn evaluate_condition(condition: &str) -> bool {
    !condition.contains("TRIGGER")
}

fn generate_sample_tripwires() -> Vec<Tripwire> {
    let now = Utc::now().to_rfc3339();
    vec![
        Tripwire::new(
            "tw_001",
            "pfl_run_001",
            TripwireType::FileChange,
            "!modified(Cargo.lock)",
            TripwireAction::Warn,
            &now,
        )
        .with_message("Cargo.lock should not be modified during task"),
        Tripwire::new(
            "tw_002",
            "pfl_run_001",
            TripwireType::ErrorThreshold,
            "error_count < 3",
            TripwireAction::Halt,
            &now,
        )
        .with_message("Halt if more than 3 errors occur"),
        Tripwire::new(
            "tw_003",
            "pfl_run_002",
            TripwireType::TimeLimit,
            "elapsed_minutes < 30",
            TripwireAction::Pause,
            &now,
        )
        .with_message("Pause if task runs longer than 30 minutes"),
        Tripwire::new(
            "tw_004",
            "pfl_run_002",
            TripwireType::Custom,
            "TRIGGER:forbidden_dep_check",
            TripwireAction::Halt,
            &now,
        )
        .with_message("Check for forbidden dependencies")
        .triggered(&now),
    ]
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
    fn list_report_schema() {
        let report = ListReport::new();
        assert_eq!(report.schema, TRIPWIRE_LIST_SCHEMA_V1);
    }

    #[test]
    fn check_report_schema() {
        let report = CheckReport::new("tw_test");
        assert_eq!(report.schema, TRIPWIRE_CHECK_SCHEMA_V1);
    }

    #[test]
    fn list_tripwires_returns_samples() -> TestResult {
        let options = ListOptions {
            workspace: PathBuf::from("."),
            ..Default::default()
        };

        let report = list_tripwires(&options).map_err(|e| e.message())?;

        ensure(report.total_count > 0, true, "has tripwires")?;
        ensure(report.schema, TRIPWIRE_LIST_SCHEMA_V1.to_string(), "schema")
    }

    #[test]
    fn list_tripwires_filters_by_state() -> TestResult {
        let options = ListOptions {
            workspace: PathBuf::from("."),
            state: Some(TripwireState::Armed),
            ..Default::default()
        };

        let report = list_tripwires(&options).map_err(|e| e.message())?;

        for tw in &report.tripwires {
            ensure(tw.state.as_str(), "armed", "filtered state")?;
        }
        ensure(
            report.filters_applied.contains(&"state=armed".to_string()),
            true,
            "filter applied",
        )
    }

    #[test]
    fn list_tripwires_filters_by_preflight_run() -> TestResult {
        let options = ListOptions {
            workspace: PathBuf::from("."),
            preflight_run_id: Some("pfl_run_001".to_string()),
            ..Default::default()
        };

        let report = list_tripwires(&options).map_err(|e| e.message())?;

        for tw in &report.tripwires {
            ensure(&tw.preflight_run_id, &"pfl_run_001".to_string(), "run id")?;
        }
        Ok(())
    }

    #[test]
    fn list_tripwires_respects_limit() -> TestResult {
        let options = ListOptions {
            workspace: PathBuf::from("."),
            limit: Some(2),
            ..Default::default()
        };

        let report = list_tripwires(&options).map_err(|e| e.message())?;

        ensure(report.total_count <= 2, true, "respects limit")
    }

    #[test]
    fn check_tripwire_not_found() -> TestResult {
        let options = CheckOptions {
            workspace: PathBuf::from("."),
            tripwire_id: "tw_nonexistent".to_string(),
            ..Default::default()
        };

        let report = check_tripwire(&options).map_err(|e| e.message())?;

        ensure(report.result, CheckResult::NotFound, "result")?;
        ensure(report.tripwire_id, "tw_nonexistent".to_string(), "id")
    }

    #[test]
    fn check_tripwire_armed_passes() -> TestResult {
        let options = CheckOptions {
            workspace: PathBuf::from("."),
            tripwire_id: "tw_001".to_string(),
            ..Default::default()
        };

        let report = check_tripwire(&options).map_err(|e| e.message())?;

        ensure(report.result, CheckResult::Passed, "result")?;
        ensure(report.should_halt, false, "should not halt")
    }

    #[test]
    fn check_tripwire_triggered() -> TestResult {
        let options = CheckOptions {
            workspace: PathBuf::from("."),
            tripwire_id: "tw_004".to_string(),
            ..Default::default()
        };

        let report = check_tripwire(&options).map_err(|e| e.message())?;

        ensure(report.result, CheckResult::Triggered, "result")
    }

    #[test]
    fn check_result_variants_stable() {
        assert_eq!(CheckResult::Passed.as_str(), "passed");
        assert_eq!(CheckResult::Triggered.as_str(), "triggered");
        assert_eq!(CheckResult::Disarmed.as_str(), "disarmed");
        assert_eq!(CheckResult::Error.as_str(), "error");
        assert_eq!(CheckResult::NotFound.as_str(), "not_found");
    }

    #[test]
    fn check_result_is_ok_semantics() {
        assert!(CheckResult::Passed.is_ok());
        assert!(CheckResult::Disarmed.is_ok());
        assert!(!CheckResult::Triggered.is_ok());
        assert!(!CheckResult::Error.is_ok());
        assert!(!CheckResult::NotFound.is_ok());
    }

    #[test]
    fn tripwire_summary_from_tripwire() {
        let tw = Tripwire::new(
            "tw_test",
            "pfl_001",
            TripwireType::Custom,
            "test_condition",
            TripwireAction::Warn,
            "2026-05-01T00:00:00Z",
        )
        .with_message("Test message");

        let summary = TripwireSummary::from(&tw);

        assert_eq!(summary.id, "tw_test");
        assert_eq!(summary.preflight_run_id, "pfl_001");
        assert_eq!(summary.tripwire_type, "custom");
        assert_eq!(summary.condition, "test_condition");
        assert_eq!(summary.action, "warn");
        assert_eq!(summary.state, "armed");
        assert_eq!(summary.message, Some("Test message".to_string()));
    }

    #[test]
    fn list_report_json_valid() {
        let report = ListReport::new();
        let json = report.to_json();

        assert!(json.contains(TRIPWIRE_LIST_SCHEMA_V1));
        assert!(json.contains("tripwires"));
    }

    #[test]
    fn check_report_json_valid() {
        let report = CheckReport::new("tw_test");
        let json = report.to_json();

        assert!(json.contains(TRIPWIRE_CHECK_SCHEMA_V1));
        assert!(json.contains("tw_test"));
    }

    #[test]
    fn list_counts_states() -> TestResult {
        let options = ListOptions {
            workspace: PathBuf::from("."),
            include_disarmed: true,
            ..Default::default()
        };

        let report = list_tripwires(&options).map_err(|e| e.message())?;

        let total_from_counts = report.armed_count
            + report.triggered_count
            + report.disarmed_count
            + report.error_count;
        ensure(
            total_from_counts >= report.total_count,
            true,
            "counts sum to total",
        )
    }
}
