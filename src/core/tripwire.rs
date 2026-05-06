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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::feedback::{
    RecordFeedbackReport, RecordTripwireFeedbackOptions, TaskOutcome, record_tripwire_feedback,
};
use crate::db::{
    CreateCurationCandidateInput, CreateTripwireCheckEventInput, DbConnection, StoredTripwire,
};
use crate::models::preflight::{Tripwire, TripwireAction, TripwireState, TripwireType};
use crate::models::{DomainError, WorkspaceId};

/// Schema for tripwire list report.
pub const TRIPWIRE_LIST_SCHEMA_V1: &str = "ee.tripwire.list.v1";

/// Schema for tripwire check report.
pub const TRIPWIRE_CHECK_SCHEMA_V1: &str = "ee.tripwire.check.v1";

/// Options for listing tripwires.
#[derive(Clone, Debug, Default)]
pub struct ListOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Optional database path. When absent, returns an honest empty projection.
    pub database_path: Option<PathBuf>,
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
    /// Optional database path. When absent, reports not found without guessing.
    pub database_path: Option<PathBuf>,
    /// Tripwire ID to check.
    pub tripwire_id: String,
    /// Explicit event data for deterministic condition evaluation.
    pub event_payload: TripwireEventPayload,
    /// Update the last_checked_at timestamp.
    pub update_timestamp: bool,
    /// Observed task outcome for optional scoring feedback.
    pub task_outcome: Option<TaskOutcome>,
    /// Perform a dry-run check without persisting.
    pub dry_run: bool,
}

/// Explicit event data supplied by a tripwire check caller.
///
/// The evaluator intentionally accepts concrete fields instead of reading
/// ambient task state. This keeps condition outcomes replayable and makes
/// missing inputs visible to callers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TripwireEventPayload {
    /// Current task text to evaluate against generated task-term conditions.
    pub task_input: Option<String>,
    /// Explicit source relevance answers keyed as `<source-kind>:<source-id>`.
    pub source_relevance: BTreeMap<String, bool>,
    /// Optional structured event payload for `event:<key.path>=<glob>` conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_data: Option<serde_json::Value>,
}

impl TripwireEventPayload {
    #[must_use]
    pub fn with_task_input(mut self, task_input: impl Into<String>) -> Self {
        self.task_input = Some(task_input.into());
        self
    }

    #[must_use]
    pub fn with_source_relevance(
        mut self,
        source_kind: impl AsRef<str>,
        source_id: impl AsRef<str>,
        relevant: bool,
    ) -> Self {
        self.source_relevance.insert(
            source_relevance_key(source_kind.as_ref(), source_id.as_ref()),
            relevant,
        );
        self
    }

    /// Attach a structured event JSON document used by `event:<path>=<glob>` conditions.
    #[must_use]
    pub fn with_event_data(mut self, event_data: serde_json::Value) -> Self {
        self.event_data = Some(event_data);
        self
    }
}

/// Deterministic result from evaluating a supported tripwire condition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionEvaluationResult {
    /// The explicit event payload satisfied the condition.
    Satisfied,
    /// The explicit event payload did not satisfy the condition.
    Unsatisfied,
    /// The condition form is not supported by the deterministic evaluator.
    UnsupportedCondition,
    /// The condition is supported, but required event payload fields are absent.
    MissingInput,
}

impl ConditionEvaluationResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::Unsatisfied => "unsatisfied",
            Self::UnsupportedCondition => "unsupported_condition",
            Self::MissingInput => "missing_input",
        }
    }
}

/// Explanation for a tripwire condition evaluation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TripwireConditionEvaluation {
    pub result: ConditionEvaluationResult,
    pub condition: String,
    pub details: String,
    pub matched_terms: Vec<String>,
    pub source_key: Option<String>,
}

impl TripwireConditionEvaluation {
    fn new(
        result: ConditionEvaluationResult,
        condition: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self {
            result,
            condition: condition.into(),
            details: details.into(),
            matched_terms: Vec::new(),
            source_key: None,
        }
    }

    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.result == ConditionEvaluationResult::Satisfied
    }
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
    pub condition_evaluation: Option<TripwireConditionEvaluation>,
    pub event_payload_hash: Option<String>,
    pub durable_mutation: bool,
    pub mutation_posture: String,
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
            condition_evaluation: None,
            event_payload_hash: None,
            durable_mutation: false,
            mutation_posture: "no_persisted_tripwire".to_owned(),
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

    #[must_use]
    pub fn unsupported_condition(message: impl Into<String>) -> Self {
        Self {
            code: "unsupported_condition".to_owned(),
            severity: "warning".to_owned(),
            message: message.into(),
            repair: Some(
                "provide a generated task_contains_any(...) or source:<kind>:<id> remains relevant condition"
                    .to_owned(),
            ),
        }
    }
}

/// List tripwires matching the given options.
pub fn list_tripwires(options: &ListOptions) -> Result<ListReport, DomainError> {
    if let Some(database_path) = options.database_path.as_deref() {
        return list_tripwires_from_database(options, database_path);
    }

    Ok(list_tripwires_from_records(&[], options))
}

/// Project stored tripwire records into a filtered list report.
#[must_use]
pub fn list_tripwires_from_records(tripwires: &[Tripwire], options: &ListOptions) -> ListReport {
    let mut report = ListReport::new();

    if let Some(ref state) = options.state {
        report
            .filters_applied
            .push(format!("state={}", state.as_str()));
    }

    if let Some(ref run_id) = options.preflight_run_id {
        report
            .filters_applied
            .push(format!("preflight_run_id={run_id}"));
    }

    if let Some(ref tw_type) = options.tripwire_type {
        report
            .filters_applied
            .push(format!("type={}", tw_type.as_str()));
    }

    if !options.include_disarmed {
        report
            .filters_applied
            .push("include_disarmed=false".to_owned());
    }

    if let Some(limit) = options.limit {
        report.filters_applied.push(format!("limit={limit}"));
    }

    let mut filtered: Vec<_> = tripwires
        .iter()
        .filter(|tripwire| options.state.is_none_or(|state| tripwire.state == state))
        .filter(|tripwire| {
            options
                .preflight_run_id
                .as_ref()
                .is_none_or(|run_id| &tripwire.preflight_run_id == run_id)
        })
        .filter(|tripwire| {
            options
                .tripwire_type
                .is_none_or(|tripwire_type| tripwire.tripwire_type == tripwire_type)
        })
        .filter(|tripwire| options.include_disarmed || tripwire.state != TripwireState::Disarmed)
        .cloned()
        .collect();

    filtered.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    if let Some(limit) = options.limit {
        filtered.truncate(limit);
    }

    report.tripwires = filtered.iter().map(TripwireSummary::from).collect();
    report.total_count = report.tripwires.len();
    report.armed_count = filtered
        .iter()
        .filter(|tripwire| tripwire.state == TripwireState::Armed)
        .count();
    report.triggered_count = filtered
        .iter()
        .filter(|tripwire| tripwire.state == TripwireState::Triggered)
        .count();
    report.disarmed_count = filtered
        .iter()
        .filter(|tripwire| tripwire.state == TripwireState::Disarmed)
        .count();
    report.error_count = filtered
        .iter()
        .filter(|tripwire| tripwire.state == TripwireState::Error)
        .count();
    report
}

/// Check a specific tripwire.
pub fn check_tripwire(options: &CheckOptions) -> Result<CheckReport, DomainError> {
    if let Some(database_path) = options.database_path.as_deref() {
        return check_tripwire_from_database(options, database_path);
    }

    let mut report = CheckReport::new(&options.tripwire_id);
    report.dry_run = options.dry_run;

    report.result = CheckResult::NotFound;
    report.details = Some(format!(
        "Tripwire '{}' not found in persisted tripwire store",
        options.tripwire_id
    ));
    report.degraded.push(TripwireDegradation::inputs_incomplete(
        "No persisted tripwire matched the requested ID, so the check could not evaluate a concrete event payload.",
    ));
    Ok(report)
}

/// Evaluate a concrete tripwire record without opening storage.
pub fn check_tripwire_record(
    tripwire: &Tripwire,
    options: &CheckOptions,
) -> Result<CheckReport, DomainError> {
    let checked_at = Utc::now().to_rfc3339();
    let event_payload_hash = hash_event_payload(&options.event_payload);
    let mut report = CheckReport::new(&tripwire.id);
    report.preflight_run_id = Some(tripwire.preflight_run_id.clone());
    report.action = tripwire.action.as_str().to_owned();
    report.condition = tripwire.condition.clone();
    report.message = tripwire.message.clone();
    report.dry_run = options.dry_run;
    report.checked_at = checked_at.clone();
    report.event_payload_hash = Some(event_payload_hash);
    report.state = tripwire.state.as_str().to_owned();
    report.mutation_posture = if options.dry_run {
        "dry_run_no_mutation".to_owned()
    } else {
        "evaluated_without_store_mutation".to_owned()
    };

    if tripwire.state == TripwireState::Disarmed {
        report.result = CheckResult::Disarmed;
        report.details = Some("Tripwire is disarmed; condition was not evaluated.".to_owned());
        return Ok(report);
    }

    let evaluation = evaluate_tripwire_condition(&tripwire.condition, &options.event_payload);
    report.condition_evaluation = Some(evaluation.clone());
    report.details = Some(evaluation.details.clone());

    match evaluation.result {
        ConditionEvaluationResult::Satisfied => {
            report.result = CheckResult::Triggered;
            report.state = TripwireState::Triggered.as_str().to_owned();
            report.should_halt = tripwire.action.stops_execution();
        }
        ConditionEvaluationResult::Unsatisfied => {
            report.result = CheckResult::Passed;
            report.state = TripwireState::Armed.as_str().to_owned();
            report.should_halt = false;
        }
        ConditionEvaluationResult::MissingInput => {
            report.result = CheckResult::Error;
            report.state = TripwireState::Error.as_str().to_owned();
            report.should_halt = false;
            report.degraded.push(TripwireDegradation::inputs_incomplete(
                evaluation.details.clone(),
            ));
        }
        ConditionEvaluationResult::UnsupportedCondition => {
            report.result = CheckResult::Error;
            report.state = TripwireState::Error.as_str().to_owned();
            report.should_halt = false;
            report
                .degraded
                .push(TripwireDegradation::unsupported_condition(
                    evaluation.details.clone(),
                ));
        }
    }

    if let Some(task_outcome) = options.task_outcome {
        report.feedback = Some(record_tripwire_feedback(&RecordTripwireFeedbackOptions {
            workspace: options.workspace.clone(),
            preflight_run_id: tripwire.preflight_run_id.clone(),
            tripwire_id: tripwire.id.clone(),
            tripwire_fired: report.result == CheckResult::Triggered,
            task_outcome,
            notes: report.details.clone(),
            dry_run: options.dry_run,
        })?);
    }

    Ok(report)
}

fn list_tripwires_from_database(
    options: &ListOptions,
    database_path: &Path,
) -> Result<ListReport, DomainError> {
    let connection = open_tripwire_database(database_path)?;
    let workspace_path = resolve_workspace_path(&options.workspace);
    let workspace_id = resolve_workspace_id(&connection, &workspace_path)?;
    let stored = connection
        .list_tripwires(
            &workspace_id,
            options.state.map(TripwireState::as_str),
            options.preflight_run_id.as_deref(),
            options.tripwire_type.map(TripwireType::as_str),
            options.include_disarmed,
            options.limit,
        )
        .map_err(storage_error("Failed to list tripwires"))?;
    let tripwires = stored
        .iter()
        .map(tripwire_from_stored)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list_tripwires_from_records(&tripwires, options))
}

fn check_tripwire_from_database(
    options: &CheckOptions,
    database_path: &Path,
) -> Result<CheckReport, DomainError> {
    let connection = open_tripwire_database(database_path)?;
    let Some(stored) = connection
        .get_tripwire(&options.tripwire_id)
        .map_err(storage_error("Failed to read tripwire"))?
    else {
        return check_tripwire(&CheckOptions {
            database_path: None,
            ..options.clone()
        });
    };

    let tripwire = tripwire_from_stored(&stored)?;
    let mut report = check_tripwire_record(&tripwire, options)?;
    if options.dry_run {
        return Ok(report);
    }

    let durable_state_update = options.update_timestamp
        && matches!(report.result, CheckResult::Passed | CheckResult::Triggered);
    if durable_state_update {
        let triggered_at =
            (report.result == CheckResult::Triggered).then_some(report.checked_at.as_str());
        connection
            .update_tripwire_check_state(
                &stored.id,
                &report.state,
                &report.checked_at,
                triggered_at,
            )
            .map_err(storage_error("Failed to update tripwire state"))?;
    }

    let mutation_posture = if durable_state_update {
        "state_update_and_check_event_persisted"
    } else {
        "check_event_persisted"
    };
    report.durable_mutation = true;
    report.mutation_posture = mutation_posture.to_owned();
    let event_id = stable_check_event_id(
        &stored.id,
        &report.checked_at,
        report
            .event_payload_hash
            .as_deref()
            .unwrap_or("blake3:missing"),
    );
    connection
        .insert_tripwire_check_event(
            &event_id,
            &CreateTripwireCheckEventInput {
                workspace_id: stored.workspace_id.clone(),
                tripwire_id: stored.id.clone(),
                preflight_run_id: stored.preflight_run_id.clone(),
                checked_at: report.checked_at.clone(),
                event_payload_hash: report
                    .event_payload_hash
                    .clone()
                    .unwrap_or_else(|| "blake3:missing".to_owned()),
                condition_result: report
                    .condition_evaluation
                    .as_ref()
                    .map_or("missing_input", |evaluation| evaluation.result.as_str())
                    .to_owned(),
                check_result: report.result.as_str().to_owned(),
                should_halt: report.should_halt,
                dry_run: report.dry_run,
                durable_mutation: report.durable_mutation,
                mutation_posture: report.mutation_posture.clone(),
                details: report.details.clone(),
                schema: report.schema.clone(),
            },
        )
        .map_err(storage_error("Failed to record tripwire check event"))?;

    Ok(report)
}

fn open_tripwire_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
        message: format!("Failed to open tripwire database: {error}"),
        repair: Some("ee init --workspace .".to_owned()),
    })
}

fn storage_error(
    context: &'static str,
) -> impl Fn(crate::db::DbError) -> DomainError + Copy + 'static {
    move |error| DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("ee doctor".to_owned()),
    }
}

fn resolve_workspace_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    absolute.canonicalize().unwrap_or(absolute)
}

fn resolve_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DomainError> {
    let path = workspace_path.to_string_lossy().into_owned();
    let stored = connection
        .get_workspace_by_path(&path)
        .map_err(storage_error("Failed to query tripwire workspace"))?;
    Ok(stored.map_or_else(
        || stable_workspace_id(workspace_path),
        |workspace| workspace.id,
    ))
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn tripwire_from_stored(stored: &StoredTripwire) -> Result<Tripwire, DomainError> {
    let tripwire_type =
        TripwireType::from_str(&stored.tripwire_type).map_err(|error| DomainError::Storage {
            message: format!("Stored tripwire {} has invalid type: {error}", stored.id),
            repair: Some("ee doctor".to_owned()),
        })?;
    let action =
        TripwireAction::from_str(&stored.action).map_err(|error| DomainError::Storage {
            message: format!("Stored tripwire {} has invalid action: {error}", stored.id),
            repair: Some("ee doctor".to_owned()),
        })?;
    let state = TripwireState::from_str(&stored.state).map_err(|error| DomainError::Storage {
        message: format!("Stored tripwire {} has invalid state: {error}", stored.id),
        repair: Some("ee doctor".to_owned()),
    })?;

    let mut tripwire = Tripwire::new(
        &stored.id,
        &stored.preflight_run_id,
        tripwire_type,
        &stored.condition,
        action,
        &stored.created_at,
    );
    tripwire.state = state;
    tripwire.message = stored.message.clone();
    tripwire.last_checked_at = stored.last_checked_at.clone();
    tripwire.triggered_at = stored.triggered_at.clone();
    Ok(tripwire)
}

fn hash_event_payload(payload: &TripwireEventPayload) -> String {
    let encoded = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
    format!("blake3:{}", blake3::hash(&encoded).to_hex())
}

fn stable_check_event_id(tripwire_id: &str, checked_at: &str, event_payload_hash: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tripwire_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(checked_at.as_bytes());
    hasher.update(b"\0");
    hasher.update(event_payload_hash.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    format!("tchk_{}", &digest[..26])
}

/// Evaluate a generated tripwire condition against explicit event data.
///
/// Supported condition forms are the deterministic strings emitted by
/// `core::preflight`: `task_contains_any("term", ...)` and
/// `source:<kind>:<source-id> remains relevant`. Unknown or malformed forms
/// return `unsupported_condition` instead of guessing.
#[must_use]
pub fn evaluate_tripwire_condition(
    condition: &str,
    payload: &TripwireEventPayload,
) -> TripwireConditionEvaluation {
    let condition = condition.trim();

    if let Some(parsed) = parse_task_contains_any_condition(condition) {
        return match parsed {
            Ok(terms) => evaluate_task_contains_any(condition, &terms, payload),
            Err(details) => TripwireConditionEvaluation::new(
                ConditionEvaluationResult::UnsupportedCondition,
                condition,
                details,
            ),
        };
    }

    if let Some(parsed) = parse_source_relevance_condition(condition) {
        return match parsed {
            Ok((source_kind, source_id)) => {
                evaluate_source_relevance(condition, &source_kind, &source_id, payload)
            }
            Err(details) => TripwireConditionEvaluation::new(
                ConditionEvaluationResult::UnsupportedCondition,
                condition,
                details,
            ),
        };
    }

    if let Some(parsed) = parse_event_match_condition(condition) {
        return match parsed {
            Ok(spec) => evaluate_event_match(condition, &spec, payload),
            Err(details) => TripwireConditionEvaluation::new(
                ConditionEvaluationResult::UnsupportedCondition,
                condition,
                details,
            ),
        };
    }

    TripwireConditionEvaluation::new(
        ConditionEvaluationResult::UnsupportedCondition,
        condition,
        "Condition form is not supported by the deterministic tripwire evaluator",
    )
}

fn evaluate_task_contains_any(
    condition: &str,
    terms: &[String],
    payload: &TripwireEventPayload,
) -> TripwireConditionEvaluation {
    let Some(task_input) = payload.task_input.as_ref() else {
        return TripwireConditionEvaluation::new(
            ConditionEvaluationResult::MissingInput,
            condition,
            "Condition requires event payload field `task_input`",
        );
    };

    let task_input = task_input.to_lowercase();
    let matched_terms: Vec<_> = terms
        .iter()
        .filter(|term| task_input.contains(term.as_str()))
        .cloned()
        .collect();

    let result = if matched_terms.is_empty() {
        ConditionEvaluationResult::Unsatisfied
    } else {
        ConditionEvaluationResult::Satisfied
    };
    let mut evaluation = TripwireConditionEvaluation::new(
        result,
        condition,
        if matched_terms.is_empty() {
            "No generated tripwire terms matched the explicit task input"
        } else {
            "At least one generated tripwire term matched the explicit task input"
        },
    );
    evaluation.matched_terms = matched_terms;
    evaluation
}

fn evaluate_source_relevance(
    condition: &str,
    source_kind: &str,
    source_id: &str,
    payload: &TripwireEventPayload,
) -> TripwireConditionEvaluation {
    let source_key = source_relevance_key(source_kind, source_id);
    let Some(relevant) = payload.source_relevance.get(&source_key) else {
        let mut evaluation = TripwireConditionEvaluation::new(
            ConditionEvaluationResult::MissingInput,
            condition,
            format!("Condition requires source relevance input for `{source_key}`"),
        );
        evaluation.source_key = Some(source_key);
        return evaluation;
    };

    let result = if *relevant {
        ConditionEvaluationResult::Satisfied
    } else {
        ConditionEvaluationResult::Unsatisfied
    };
    let mut evaluation = TripwireConditionEvaluation::new(
        result,
        condition,
        if *relevant {
            "Explicit event payload marks the source as still relevant"
        } else {
            "Explicit event payload marks the source as not relevant"
        },
    );
    evaluation.source_key = Some(source_key);
    evaluation
}

fn parse_task_contains_any_condition(condition: &str) -> Option<Result<Vec<String>, String>> {
    let raw_args = condition
        .strip_prefix("task_contains_any(")?
        .strip_suffix(')')?;
    let json_args = format!("[{raw_args}]");
    let parsed = serde_json::from_str::<Vec<String>>(&json_args)
        .map_err(|error| format!("Malformed task_contains_any condition arguments: {error}"));

    Some(parsed.and_then(|terms| {
        let normalized = normalize_condition_terms(terms);
        if normalized.is_empty() {
            Err("task_contains_any condition must include at least one term".to_owned())
        } else {
            Ok(normalized)
        }
    }))
}

fn normalize_condition_terms(terms: Vec<String>) -> Vec<String> {
    let mut normalized: Vec<_> = terms
        .into_iter()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn parse_source_relevance_condition(condition: &str) -> Option<Result<(String, String), String>> {
    let raw = condition
        .strip_prefix("source:")?
        .strip_suffix(" remains relevant")?;
    let Some((source_kind, source_id)) = raw.split_once(':') else {
        return Some(Err(
            "source relevance condition must include source kind and source id".to_owned(),
        ));
    };
    let source_kind = source_kind.trim();
    let source_id = source_id.trim();
    if source_kind.is_empty() || source_id.is_empty() {
        return Some(Err(
            "source relevance condition must include non-empty source kind and source id"
                .to_owned(),
        ));
    }
    Some(Ok((source_kind.to_owned(), source_id.to_owned())))
}

fn source_relevance_key(source_kind: &str, source_id: &str) -> String {
    format!("{}:{}", source_kind.trim(), source_id.trim())
}

/// A parsed `event:<key.path>=<glob-pattern>` condition specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventMatchSpec {
    /// Dotted JSON pointer path (e.g. `command.path`, `tool.name`).
    pub path: String,
    /// Glob pattern to match the value at `path` against.
    pub pattern: String,
}

fn parse_event_match_condition(condition: &str) -> Option<Result<EventMatchSpec, String>> {
    let raw = condition.strip_prefix("event:")?;
    let Some((path, pattern)) = raw.split_once('=') else {
        return Some(Err(
            "event match condition must be `event:<key.path>=<glob>`".to_owned(),
        ));
    };
    let path = path.trim();
    let pattern = pattern.trim();
    if path.is_empty() || pattern.is_empty() {
        return Some(Err(
            "event match condition requires a non-empty key path and glob pattern".to_owned(),
        ));
    }
    if path.contains("..") || path.starts_with('.') || path.ends_with('.') {
        return Some(Err(format!(
            "event match condition has malformed key path `{path}`"
        )));
    }
    let pattern = strip_optional_quotes(pattern);
    Some(Ok(EventMatchSpec {
        path: path.to_owned(),
        pattern: pattern.to_owned(),
    }))
}

fn strip_optional_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn evaluate_event_match(
    condition: &str,
    spec: &EventMatchSpec,
    payload: &TripwireEventPayload,
) -> TripwireConditionEvaluation {
    let Some(event_data) = payload.event_data.as_ref() else {
        let mut evaluation = TripwireConditionEvaluation::new(
            ConditionEvaluationResult::MissingInput,
            condition,
            format!(
                "Condition requires event payload field `event_data` for path `{}`",
                spec.path
            ),
        );
        evaluation.source_key = Some(spec.path.clone());
        return evaluation;
    };

    let Some(value) = lookup_event_path(event_data, &spec.path) else {
        let mut evaluation = TripwireConditionEvaluation::new(
            ConditionEvaluationResult::Unsatisfied,
            condition,
            format!(
                "Event payload has no value at path `{}`; tripwire did not match",
                spec.path
            ),
        );
        evaluation.source_key = Some(spec.path.clone());
        return evaluation;
    };

    let candidate = json_value_to_match_string(value);
    let matched = glob_match(&spec.pattern, &candidate);

    let result = if matched {
        ConditionEvaluationResult::Satisfied
    } else {
        ConditionEvaluationResult::Unsatisfied
    };
    let mut evaluation = TripwireConditionEvaluation::new(
        result,
        condition,
        if matched {
            format!(
                "Event payload value at `{}` matched glob `{}`",
                spec.path, spec.pattern
            )
        } else {
            format!(
                "Event payload value at `{}` did not match glob `{}`",
                spec.path, spec.pattern
            )
        },
    );
    evaluation.source_key = Some(spec.path.clone());
    evaluation.matched_terms = vec![format!("{}={}", spec.path, candidate)];
    evaluation
}

fn lookup_event_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = match current {
            serde_json::Value::Object(map) => map.get(segment)?,
            serde_json::Value::Array(items) => {
                let index: usize = segment.parse().ok()?;
                items.get(index)?
            }
            _ => return None,
        };
    }
    Some(current)
}

fn json_value_to_match_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

/// Match `text` against a glob `pattern` supporting `*`, `?`, and literal characters.
///
/// Globs are anchored: the pattern must consume the whole string. `*` matches any
/// run of characters (including empty); `?` matches exactly one character. There
/// is no character-class support — keep the language deterministic and small.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern_bytes = pattern.as_bytes();
    let text_bytes = text.as_bytes();
    glob_match_bytes(pattern_bytes, text_bytes)
}

fn glob_match_bytes(pattern: &[u8], text: &[u8]) -> bool {
    let mut pi = 0_usize;
    let mut ti = 0_usize;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0_usize;

    while ti < text.len() {
        if pi < pattern.len() {
            match pattern[pi] {
                b'*' => {
                    star_pi = Some(pi);
                    star_ti = ti;
                    pi += 1;
                    continue;
                }
                b'?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                byte if byte == text[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }

        if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

// ============================================================================
// Harm-feedback → tripwire candidate promotion
// ============================================================================

/// Schema for the harm-feedback tripwire promotion proposal.
pub const TRIPWIRE_HARM_PROMOTION_SCHEMA_V1: &str = "ee.tripwire.harm_promotion.v1";

/// Default minimum count of harmful feedback events that triggers a promotion.
pub const DEFAULT_HARM_PROMOTION_THRESHOLD: u32 = 3;

/// Source-type token recorded on candidates produced by harm-feedback promotion.
pub const HARM_PROMOTION_SOURCE_TYPE: &str = "feedback_event";

/// Inputs for proposing a tripwire candidate from accumulated harmful feedback.
#[derive(Clone, Debug)]
pub struct HarmFeedbackPromotionOptions {
    /// Workspace whose memory accumulated the harmful signals.
    pub workspace_id: String,
    /// Memory whose harmful feedback crossed the threshold.
    pub memory_id: String,
    /// Observed harmful feedback count (over the configured time window).
    pub harm_count: u32,
    /// Promotion threshold (events at or above this count promote).
    pub threshold: u32,
    /// Optional summary text from the offending memory; informs the proposed condition.
    pub memory_summary: Option<String>,
    /// Time window in seconds the harm count is observed across (informational).
    pub window_seconds: u64,
    /// Suggested condition string to attach to the proposed tripwire.
    /// When `None`, a deterministic `source:memory:<id>` condition is built.
    pub suggested_condition: Option<String>,
}

/// Outcome from a single harm-feedback promotion attempt.
#[derive(Clone, Debug)]
pub enum HarmFeedbackPromotionOutcome {
    /// Harm count was below the configured threshold; no candidate proposed.
    BelowThreshold { harm_count: u32, threshold: u32 },
    /// A `rule` curation candidate was prepared (not yet inserted).
    Promoted(Box<HarmFeedbackPromotionProposal>),
}

/// Concrete proposal returned by a successful promotion attempt.
#[derive(Clone, Debug)]
pub struct HarmFeedbackPromotionProposal {
    /// Stable candidate id (derivable from inputs).
    pub candidate_id: String,
    /// Insertable curation row.
    pub input: CreateCurationCandidateInput,
    /// Tripwire condition that the proposed rule would arm.
    pub condition: String,
    /// Source memory id (echoed for convenience).
    pub memory_id: String,
    /// Threshold that was crossed.
    pub harm_count: u32,
    pub threshold: u32,
}

impl HarmFeedbackPromotionProposal {
    /// Render the proposal as a stable JSON document for `ee curate candidates`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": TRIPWIRE_HARM_PROMOTION_SCHEMA_V1,
            "candidateId": self.candidate_id,
            "candidateType": self.input.candidate_type,
            "memoryId": self.memory_id,
            "harmCount": self.harm_count,
            "threshold": self.threshold,
            "condition": self.condition,
            "reason": self.input.reason,
            "sourceType": self.input.source_type,
            "sourceId": self.input.source_id,
            "confidence": self.input.confidence,
        })
    }
}

/// Decide whether the supplied harm-feedback count should promote a tripwire candidate,
/// and if so, return a deterministic `CreateCurationCandidateInput` ready to insert.
#[must_use]
pub fn propose_tripwire_from_harmful_feedback(
    options: &HarmFeedbackPromotionOptions,
) -> HarmFeedbackPromotionOutcome {
    if options.harm_count < options.threshold {
        return HarmFeedbackPromotionOutcome::BelowThreshold {
            harm_count: options.harm_count,
            threshold: options.threshold,
        };
    }

    let condition = options
        .suggested_condition
        .clone()
        .unwrap_or_else(|| format!("source:memory:{} remains relevant", options.memory_id));

    let candidate_id = stable_promotion_candidate_id(
        &options.workspace_id,
        &options.memory_id,
        options.harm_count,
        &condition,
    );

    let summary_excerpt = options
        .memory_summary
        .as_deref()
        .map(truncate_for_reason)
        .unwrap_or_else(|| format!("memory {}", options.memory_id));

    let reason = format!(
        "Auto-proposed tripwire from {harm_count} harmful feedback events on {summary} (threshold={threshold}, window={window}s); condition `{condition}`",
        harm_count = options.harm_count,
        summary = summary_excerpt,
        threshold = options.threshold,
        window = options.window_seconds,
        condition = condition,
    );

    let input = CreateCurationCandidateInput {
        workspace_id: options.workspace_id.clone(),
        candidate_type: "rule".to_owned(),
        target_memory_id: options.memory_id.clone(),
        proposed_content: Some(condition.clone()),
        proposed_confidence: Some(0.55),
        proposed_trust_class: Some("agent_assertion".to_owned()),
        source_type: HARM_PROMOTION_SOURCE_TYPE.to_owned(),
        source_id: Some(format!("harm_feedback:{}", options.memory_id)),
        reason,
        confidence: 0.55,
        status: Some("pending".to_owned()),
        created_at: None,
        ttl_expires_at: None,
    };

    HarmFeedbackPromotionOutcome::Promoted(Box::new(HarmFeedbackPromotionProposal {
        candidate_id,
        input,
        condition,
        memory_id: options.memory_id.clone(),
        harm_count: options.harm_count,
        threshold: options.threshold,
    }))
}

fn stable_promotion_candidate_id(
    workspace_id: &str,
    memory_id: &str,
    harm_count: u32,
    condition: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tripwire_harm_promotion:");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(memory_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(harm_count.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(condition.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    format!("curate_{}", &digest[..26])
}

fn truncate_for_reason(text: &str) -> String {
    const MAX: usize = 96;
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_owned();
    }
    let mut out: String = trimmed.chars().take(MAX).collect();
    out.push('…');
    out
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
    fn list_tripwires_returns_empty_state_without_samples() -> TestResult {
        let options = ListOptions {
            workspace: PathBuf::from("."),
            ..Default::default()
        };

        let report = list_tripwires(&options).map_err(|e| e.message())?;

        ensure(report.total_count, 0, "no tripwires")?;
        ensure(report.tripwires.is_empty(), true, "empty tripwire list")?;
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
    fn task_contains_any_condition_is_satisfied_from_explicit_payload() -> TestResult {
        let payload = TripwireEventPayload::default()
            .with_task_input("Prepare deploy migration release notes");

        let evaluation =
            evaluate_tripwire_condition("task_contains_any(\"deploy\", \"migration\")", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::Satisfied,
            "result",
        )?;
        ensure(
            evaluation.matched_terms,
            vec!["deploy".to_owned(), "migration".to_owned()],
            "matched terms",
        )
    }

    #[test]
    fn task_contains_any_condition_reports_unsatisfied() -> TestResult {
        let payload = TripwireEventPayload::default().with_task_input("format docs");

        let evaluation =
            evaluate_tripwire_condition("task_contains_any(\"deploy\", \"migration\")", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::Unsatisfied,
            "result",
        )?;
        ensure(evaluation.matched_terms.is_empty(), true, "matched terms")
    }

    #[test]
    fn task_contains_any_condition_requires_task_input() -> TestResult {
        let evaluation = evaluate_tripwire_condition(
            "task_contains_any(\"deploy\", \"migration\")",
            &TripwireEventPayload::default(),
        );

        ensure(
            evaluation.result,
            ConditionEvaluationResult::MissingInput,
            "result",
        )
    }

    #[test]
    fn source_relevance_condition_uses_explicit_payload() -> TestResult {
        let payload = TripwireEventPayload::default().with_source_relevance(
            "dependency_contract",
            "dep_no_tokio",
            false,
        );

        let evaluation = evaluate_tripwire_condition(
            "source:dependency_contract:dep_no_tokio remains relevant",
            &payload,
        );

        ensure(
            evaluation.result,
            ConditionEvaluationResult::Unsatisfied,
            "result",
        )?;
        ensure(
            evaluation.source_key,
            Some("dependency_contract:dep_no_tokio".to_owned()),
            "source key",
        )
    }

    #[test]
    fn unsupported_condition_is_reported_without_guessing() -> TestResult {
        let evaluation = evaluate_tripwire_condition(
            "error_count < 3",
            &TripwireEventPayload::default().with_task_input("deploy"),
        );

        ensure(
            evaluation.result,
            ConditionEvaluationResult::UnsupportedCondition,
            "result",
        )
    }

    #[test]
    fn list_tripwires_from_records_filters_counts_and_orders() -> TestResult {
        let first = Tripwire::new(
            "tw_b",
            "pf_release",
            TripwireType::Custom,
            "task_contains_any(\"deploy\")",
            TripwireAction::Warn,
            "2026-05-03T20:01:00Z",
        )
        .triggered("2026-05-03T20:02:00Z");
        let second = Tripwire::new(
            "tw_a",
            "pf_release",
            TripwireType::Custom,
            "task_contains_any(\"format\")",
            TripwireAction::Audit,
            "2026-05-03T20:00:00Z",
        );
        let third = Tripwire::new(
            "tw_c",
            "pf_other",
            TripwireType::Custom,
            "task_contains_any(\"other\")",
            TripwireAction::Warn,
            "2026-05-03T20:03:00Z",
        )
        .disarmed();

        let report = list_tripwires_from_records(
            &[first, second, third],
            &ListOptions {
                workspace: PathBuf::from("."),
                preflight_run_id: Some("pf_release".to_owned()),
                include_disarmed: true,
                limit: Some(2),
                ..Default::default()
            },
        );

        ensure(report.total_count, 2, "total count")?;
        ensure(report.armed_count, 1, "armed count")?;
        ensure(report.triggered_count, 1, "triggered count")?;
        ensure(report.tripwires[0].id.as_str(), "tw_a", "stable order")?;
        ensure(report.tripwires[1].id.as_str(), "tw_b", "stable order")
    }

    #[test]
    fn check_tripwire_record_triggers_from_explicit_payload_and_records_feedback() -> TestResult {
        let tripwire = Tripwire::new(
            "tw_release",
            "pf_release",
            TripwireType::Custom,
            "task_contains_any(\"deploy\")",
            TripwireAction::Halt,
            "2026-05-03T20:00:00Z",
        );
        let report = check_tripwire_record(
            &tripwire,
            &CheckOptions {
                workspace: PathBuf::from("."),
                tripwire_id: "tw_release".to_owned(),
                event_payload: TripwireEventPayload::default().with_task_input("deploy release"),
                task_outcome: Some(TaskOutcome::Success),
                dry_run: true,
                ..Default::default()
            },
        )
        .map_err(|error| error.message())?;

        ensure(report.result, CheckResult::Triggered, "result")?;
        ensure(report.should_halt, true, "halt decision")?;
        ensure(
            report
                .event_payload_hash
                .as_deref()
                .is_some_and(|hash| hash.starts_with("blake3:")),
            true,
            "payload hash",
        )?;
        ensure(
            report.feedback.is_some(),
            true,
            "task outcome records feedback projection",
        )
    }

    #[test]
    fn check_tripwire_record_reports_unsupported_condition() -> TestResult {
        let tripwire = Tripwire::new(
            "tw_unsupported",
            "pf_release",
            TripwireType::Custom,
            "error_count < 3",
            TripwireAction::Warn,
            "2026-05-03T20:00:00Z",
        );
        let report = check_tripwire_record(
            &tripwire,
            &CheckOptions {
                workspace: PathBuf::from("."),
                tripwire_id: "tw_unsupported".to_owned(),
                event_payload: TripwireEventPayload::default().with_task_input("deploy"),
                dry_run: true,
                ..Default::default()
            },
        )
        .map_err(|error| error.message())?;

        ensure(report.result, CheckResult::Error, "result")?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "unsupported_condition"),
            true,
            "unsupported degradation",
        )
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
    fn glob_match_matches_full_text() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.sh", "deploy.sh"));
        assert!(glob_match("rm*", "rm -rf /"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("a?c", "abbc"));
        assert!(!glob_match("*.sh", "deploy.txt"));
        assert!(glob_match("*deploy*", "preprod-deploy-job"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
    }

    #[test]
    fn event_match_condition_satisfied_with_glob() -> TestResult {
        let payload = TripwireEventPayload::default().with_event_data(serde_json::json!({
            "command": { "path": "scripts/deploy.sh", "argv": ["./deploy.sh", "prod"] },
            "tool": { "name": "Bash" },
        }));

        let evaluation = evaluate_tripwire_condition("event:command.path=*.sh", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::Satisfied,
            "satisfied",
        )?;
        ensure(
            evaluation.source_key,
            Some("command.path".to_owned()),
            "source key",
        )?;
        ensure(
            evaluation
                .matched_terms
                .iter()
                .any(|term| term.contains("scripts/deploy.sh")),
            true,
            "citation includes value",
        )
    }

    #[test]
    fn event_match_condition_unsatisfied_when_value_differs() -> TestResult {
        let payload = TripwireEventPayload::default()
            .with_event_data(serde_json::json!({"tool": {"name": "Read"}}));

        let evaluation = evaluate_tripwire_condition("event:tool.name=\"Bash\"", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::Unsatisfied,
            "unsatisfied",
        )
    }

    #[test]
    fn event_match_condition_unsatisfied_when_path_missing() -> TestResult {
        let payload = TripwireEventPayload::default()
            .with_event_data(serde_json::json!({"tool": {"name": "Bash"}}));

        let evaluation = evaluate_tripwire_condition("event:command.path=*.sh", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::Unsatisfied,
            "missing path is unsatisfied",
        )
    }

    #[test]
    fn event_match_condition_missing_input_without_event_data() -> TestResult {
        let payload = TripwireEventPayload::default();

        let evaluation = evaluate_tripwire_condition("event:command.path=*.sh", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::MissingInput,
            "missing input",
        )
    }

    #[test]
    fn event_match_condition_rejects_malformed_path() -> TestResult {
        let payload = TripwireEventPayload::default().with_event_data(serde_json::json!({"a": 1}));

        let evaluation = evaluate_tripwire_condition("event:..=foo", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::UnsupportedCondition,
            "malformed path is unsupported",
        )
    }

    #[test]
    fn event_match_condition_supports_array_index() -> TestResult {
        let payload = TripwireEventPayload::default().with_event_data(serde_json::json!({
            "command": {"argv": ["bash", "-c", "rm -rf /tmp/x"]}
        }));

        let evaluation = evaluate_tripwire_condition("event:command.argv.2=rm*", &payload);

        ensure(
            evaluation.result,
            ConditionEvaluationResult::Satisfied,
            "array index resolves",
        )
    }

    #[test]
    fn check_tripwire_record_triggers_from_event_payload_glob() -> TestResult {
        let tripwire = Tripwire::new(
            "tw_event_bash",
            "pf_evt",
            TripwireType::Custom,
            "event:command.path=*.sh",
            TripwireAction::Halt,
            "2026-05-06T00:00:00Z",
        );
        let report = check_tripwire_record(
            &tripwire,
            &CheckOptions {
                workspace: PathBuf::from("."),
                tripwire_id: "tw_event_bash".to_owned(),
                event_payload: TripwireEventPayload::default()
                    .with_event_data(serde_json::json!({"command": {"path": "deploy.sh"}})),
                dry_run: true,
                ..Default::default()
            },
        )
        .map_err(|error| error.message())?;

        ensure(report.result, CheckResult::Triggered, "triggered")?;
        ensure(
            report
                .condition_evaluation
                .and_then(|e| e.source_key)
                .as_deref(),
            Some("command.path"),
            "citation key",
        )
    }

    #[test]
    fn harm_feedback_promotion_below_threshold_does_not_promote() {
        let outcome = propose_tripwire_from_harmful_feedback(&HarmFeedbackPromotionOptions {
            workspace_id: "ws_test".to_owned(),
            memory_id: "mem_001".to_owned(),
            harm_count: 1,
            threshold: 3,
            memory_summary: Some("never run rm -rf /".to_owned()),
            window_seconds: 7 * 24 * 3600,
            suggested_condition: None,
        });
        match outcome {
            HarmFeedbackPromotionOutcome::BelowThreshold {
                harm_count,
                threshold,
            } => {
                assert_eq!(harm_count, 1);
                assert_eq!(threshold, 3);
            }
            other => panic!("expected BelowThreshold, got {other:?}"),
        }
    }

    #[test]
    fn harm_feedback_promotion_at_threshold_returns_proposal() {
        let outcome = propose_tripwire_from_harmful_feedback(&HarmFeedbackPromotionOptions {
            workspace_id: "ws_test".to_owned(),
            memory_id: "mem_42".to_owned(),
            harm_count: 4,
            threshold: 3,
            memory_summary: Some("destructive command guidance".to_owned()),
            window_seconds: 7 * 24 * 3600,
            suggested_condition: Some("event:command.path=*rm*".to_owned()),
        });

        let HarmFeedbackPromotionOutcome::Promoted(proposal) = outcome else {
            panic!("expected Promoted variant");
        };

        assert!(proposal.candidate_id.starts_with("curate_"));
        assert_eq!(proposal.candidate_id.len(), 33);
        assert_eq!(proposal.input.candidate_type, "rule");
        assert_eq!(proposal.input.target_memory_id, "mem_42");
        assert_eq!(proposal.input.source_type, HARM_PROMOTION_SOURCE_TYPE);
        assert_eq!(
            proposal.input.source_id.as_deref(),
            Some("harm_feedback:mem_42")
        );
        assert_eq!(proposal.condition, "event:command.path=*rm*");
        assert!(
            proposal.input.reason.contains("4 harmful feedback events"),
            "reason explains harm count: {}",
            proposal.input.reason
        );
        assert!((proposal.input.confidence - 0.55).abs() < 1e-6);

        // Stable: same inputs → same id.
        let again = propose_tripwire_from_harmful_feedback(&HarmFeedbackPromotionOptions {
            workspace_id: "ws_test".to_owned(),
            memory_id: "mem_42".to_owned(),
            harm_count: 4,
            threshold: 3,
            memory_summary: Some("destructive command guidance".to_owned()),
            window_seconds: 7 * 24 * 3600,
            suggested_condition: Some("event:command.path=*rm*".to_owned()),
        });
        let HarmFeedbackPromotionOutcome::Promoted(again_proposal) = again else {
            panic!("expected Promoted variant on replay");
        };
        assert_eq!(proposal.candidate_id, again_proposal.candidate_id);
    }

    #[test]
    fn harm_feedback_promotion_uses_default_condition_when_none_supplied() {
        let outcome = propose_tripwire_from_harmful_feedback(&HarmFeedbackPromotionOptions {
            workspace_id: "ws_test".to_owned(),
            memory_id: "mem_xyz".to_owned(),
            harm_count: 3,
            threshold: 3,
            memory_summary: None,
            window_seconds: 86_400,
            suggested_condition: None,
        });
        let HarmFeedbackPromotionOutcome::Promoted(proposal) = outcome else {
            panic!("expected Promoted variant");
        };
        assert_eq!(proposal.condition, "source:memory:mem_xyz remains relevant");
        let json = proposal.to_json();
        assert_eq!(
            json["schema"].as_str(),
            Some(TRIPWIRE_HARM_PROMOTION_SCHEMA_V1)
        );
        assert_eq!(json["memoryId"].as_str(), Some("mem_xyz"));
        assert_eq!(json["harmCount"].as_i64(), Some(3));
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
