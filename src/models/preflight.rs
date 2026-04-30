//! Preflight schemas for task risk assessment (EE-390).
//!
//! Preflight is the system for assessing task risk before execution.
//! It checks for potential issues, generates risk briefs, and sets
//! tripwires that can halt execution if conditions are violated.
//!
//! Core concepts:
//!
//! * **Preflight run**: A risk assessment pass before task execution
//! * **Risk brief**: Summary of identified risks and recommendations
//! * **Tripwire**: A condition that must remain true during execution
//! * **Tripwire event**: A record of tripwire state changes

use std::fmt;
use std::str::FromStr;

/// Schema version for preflight run.
pub const PREFLIGHT_RUN_SCHEMA_V1: &str = "ee.preflight_run.v1";

/// Schema version for risk brief.
pub const RISK_BRIEF_SCHEMA_V1: &str = "ee.risk_brief.v1";

/// Schema version for tripwire.
pub const TRIPWIRE_SCHEMA_V1: &str = "ee.tripwire.v1";

/// Schema version for tripwire event.
pub const TRIPWIRE_EVENT_SCHEMA_V1: &str = "ee.tripwire_event.v1";

/// ID prefix for preflight runs.
pub const PREFLIGHT_RUN_ID_PREFIX: &str = "pf_";

/// ID prefix for risk briefs.
pub const RISK_BRIEF_ID_PREFIX: &str = "rb_";

/// ID prefix for tripwires.
pub const TRIPWIRE_ID_PREFIX: &str = "tw_";

/// ID prefix for tripwire events.
pub const TRIPWIRE_EVENT_ID_PREFIX: &str = "twe_";

/// A preflight risk assessment run.
///
/// Preflight runs evaluate a task before execution to identify risks,
/// required permissions, and potential issues.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreflightRun {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique run ID.
    pub id: String,
    /// Workspace where preflight was run.
    pub workspace_id: Option<String>,
    /// Task input being assessed.
    pub task_input: String,
    /// Status of the preflight run.
    pub status: PreflightStatus,
    /// Risk brief produced by this run.
    pub risk_brief_id: Option<String>,
    /// Tripwires set for this task.
    pub tripwire_ids: Vec<String>,
    /// Overall risk level determined.
    pub risk_level: RiskLevel,
    /// Whether the task is cleared for execution.
    pub cleared: bool,
    /// Reason if not cleared.
    pub block_reason: Option<String>,
    /// Timestamp when preflight started (RFC 3339).
    pub started_at: String,
    /// Timestamp when preflight completed (RFC 3339).
    pub completed_at: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
}

impl PreflightRun {
    /// Create a new preflight run.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        task_input: impl Into<String>,
        started_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: PREFLIGHT_RUN_SCHEMA_V1,
            id: id.into(),
            task_input: task_input.into(),
            started_at: started_at.into(),
            status: PreflightStatus::Running,
            risk_level: RiskLevel::Unknown,
            cleared: false,
            ..Default::default()
        }
    }

    /// Set the workspace ID.
    #[must_use]
    pub fn with_workspace_id(mut self, id: impl Into<String>) -> Self {
        self.workspace_id = Some(id.into());
        self
    }

    /// Set the status.
    #[must_use]
    pub fn with_status(mut self, status: PreflightStatus) -> Self {
        self.status = status;
        self
    }

    /// Set the risk brief ID.
    #[must_use]
    pub fn with_risk_brief_id(mut self, id: impl Into<String>) -> Self {
        self.risk_brief_id = Some(id.into());
        self
    }

    /// Add a tripwire ID.
    pub fn add_tripwire(&mut self, id: impl Into<String>) {
        self.tripwire_ids.push(id.into());
    }

    /// Set the risk level.
    #[must_use]
    pub fn with_risk_level(mut self, level: RiskLevel) -> Self {
        self.risk_level = level;
        self
    }

    /// Mark as cleared for execution.
    #[must_use]
    pub fn cleared(mut self) -> Self {
        self.cleared = true;
        self
    }

    /// Mark as blocked with reason.
    #[must_use]
    pub fn blocked(mut self, reason: impl Into<String>) -> Self {
        self.cleared = false;
        self.block_reason = Some(reason.into());
        self
    }

    /// Set completion timestamp.
    #[must_use]
    pub fn with_completed_at(mut self, ts: impl Into<String>) -> Self {
        self.completed_at = Some(ts.into());
        self
    }

    /// Set duration.
    #[must_use]
    pub fn with_duration_ms(mut self, ms: u64) -> Self {
        self.duration_ms = Some(ms);
        self
    }
}

/// Status of a preflight run.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PreflightStatus {
    /// Preflight is currently running.
    #[default]
    Running,
    /// Preflight completed successfully.
    Completed,
    /// Preflight failed to complete.
    Failed,
    /// Preflight was cancelled.
    Cancelled,
    /// Preflight timed out.
    Timeout,
}

impl PreflightStatus {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
        }
    }
}

impl fmt::Display for PreflightStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PreflightStatus {
    type Err = ParsePreflightStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "timeout" => Ok(Self::Timeout),
            _ => Err(ParsePreflightStatusError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a preflight status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsePreflightStatusError {
    input: String,
}

impl fmt::Display for ParsePreflightStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown preflight status `{}`; expected running, completed, failed, cancelled, or timeout",
            self.input
        )
    }
}

impl std::error::Error for ParsePreflightStatusError {}

/// Risk level assessment.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RiskLevel {
    /// No identified risks.
    None,
    /// Low risk - minor concerns.
    Low,
    /// Medium risk - notable concerns.
    Medium,
    /// High risk - significant concerns.
    High,
    /// Critical risk - should not proceed.
    Critical,
    /// Risk level unknown or not assessed.
    #[default]
    Unknown,
}

impl RiskLevel {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this risk level should block execution.
    #[must_use]
    pub const fn should_block(self) -> bool {
        matches!(self, Self::Critical)
    }

    /// Whether this risk level requires confirmation.
    #[must_use]
    pub const fn requires_confirmation(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RiskLevel {
    type Err = ParseRiskLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            "unknown" => Ok(Self::Unknown),
            _ => Err(ParseRiskLevelError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a risk level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRiskLevelError {
    input: String,
}

impl fmt::Display for ParseRiskLevelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown risk level `{}`; expected none, low, medium, high, critical, or unknown",
            self.input
        )
    }
}

impl std::error::Error for ParseRiskLevelError {}

/// A risk brief summarizing identified risks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RiskBrief {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique brief ID.
    pub id: String,
    /// Preflight run that produced this brief.
    pub preflight_run_id: String,
    /// Overall risk level.
    pub risk_level: RiskLevel,
    /// Individual risk items identified.
    pub risks: Vec<RiskItem>,
    /// Recommendations for risk mitigation.
    pub recommendations: Vec<String>,
    /// Required permissions for the task.
    pub required_permissions: Vec<String>,
    /// Potential side effects.
    pub side_effects: Vec<String>,
    /// Human-readable summary.
    pub summary: Option<String>,
    /// Timestamp when brief was generated (RFC 3339).
    pub generated_at: String,
}

impl RiskBrief {
    /// Create a new risk brief.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        preflight_run_id: impl Into<String>,
        risk_level: RiskLevel,
        generated_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: RISK_BRIEF_SCHEMA_V1,
            id: id.into(),
            preflight_run_id: preflight_run_id.into(),
            risk_level,
            generated_at: generated_at.into(),
            ..Default::default()
        }
    }

    /// Add a risk item.
    pub fn add_risk(&mut self, risk: RiskItem) {
        self.risks.push(risk);
    }

    /// Add a recommendation.
    pub fn add_recommendation(&mut self, rec: impl Into<String>) {
        self.recommendations.push(rec.into());
    }

    /// Add a required permission.
    pub fn add_required_permission(&mut self, perm: impl Into<String>) {
        self.required_permissions.push(perm.into());
    }

    /// Add a side effect.
    pub fn add_side_effect(&mut self, effect: impl Into<String>) {
        self.side_effects.push(effect.into());
    }

    /// Set summary.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }
}

/// An individual risk item identified during preflight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskItem {
    /// Category of risk.
    pub category: RiskCategory,
    /// Risk level for this specific item.
    pub level: RiskLevel,
    /// Description of the risk.
    pub description: String,
    /// Mitigation if available.
    pub mitigation: Option<String>,
    /// Source that identified this risk.
    pub source: Option<String>,
}

impl RiskItem {
    /// Create a new risk item.
    #[must_use]
    pub fn new(category: RiskCategory, level: RiskLevel, description: impl Into<String>) -> Self {
        Self {
            category,
            level,
            description: description.into(),
            mitigation: None,
            source: None,
        }
    }

    /// Set mitigation.
    #[must_use]
    pub fn with_mitigation(mut self, mitigation: impl Into<String>) -> Self {
        self.mitigation = Some(mitigation.into());
        self
    }

    /// Set source.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

/// Category of risk.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum RiskCategory {
    /// Data loss or corruption risk.
    DataLoss,
    /// Security vulnerability risk.
    Security,
    /// System stability risk.
    Stability,
    /// Performance degradation risk.
    Performance,
    /// External service disruption risk.
    ExternalService,
    /// Compliance or policy violation risk.
    Compliance,
    /// Reversibility concern (hard to undo).
    Reversibility,
    /// Resource exhaustion risk.
    ResourceExhaustion,
    /// Other/uncategorized risk.
    #[default]
    Other,
}

impl RiskCategory {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DataLoss => "data_loss",
            Self::Security => "security",
            Self::Stability => "stability",
            Self::Performance => "performance",
            Self::ExternalService => "external_service",
            Self::Compliance => "compliance",
            Self::Reversibility => "reversibility",
            Self::ResourceExhaustion => "resource_exhaustion",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for RiskCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RiskCategory {
    type Err = ParseRiskCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "data_loss" => Ok(Self::DataLoss),
            "security" => Ok(Self::Security),
            "stability" => Ok(Self::Stability),
            "performance" => Ok(Self::Performance),
            "external_service" => Ok(Self::ExternalService),
            "compliance" => Ok(Self::Compliance),
            "reversibility" => Ok(Self::Reversibility),
            "resource_exhaustion" => Ok(Self::ResourceExhaustion),
            "other" => Ok(Self::Other),
            _ => Err(ParseRiskCategoryError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a risk category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRiskCategoryError {
    input: String,
}

impl fmt::Display for ParseRiskCategoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown risk category `{}`; expected data_loss, security, stability, performance, external_service, compliance, reversibility, resource_exhaustion, or other",
            self.input
        )
    }
}

impl std::error::Error for ParseRiskCategoryError {}

/// A tripwire that monitors conditions during execution.
///
/// Tripwires are set during preflight and checked periodically
/// during task execution. If a tripwire is triggered, execution
/// may be halted or an alert raised.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tripwire {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique tripwire ID.
    pub id: String,
    /// Preflight run that created this tripwire.
    pub preflight_run_id: String,
    /// Type of tripwire.
    pub tripwire_type: TripwireType,
    /// Condition expression (human-readable or DSL).
    pub condition: String,
    /// Action to take when triggered.
    pub action: TripwireAction,
    /// Current state of the tripwire.
    pub state: TripwireState,
    /// Message to show when triggered.
    pub message: Option<String>,
    /// Timestamp when tripwire was created (RFC 3339).
    pub created_at: String,
    /// Timestamp of last check (RFC 3339).
    pub last_checked_at: Option<String>,
    /// Timestamp when triggered, if applicable (RFC 3339).
    pub triggered_at: Option<String>,
}

impl Tripwire {
    /// Create a new tripwire.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        preflight_run_id: impl Into<String>,
        tripwire_type: TripwireType,
        condition: impl Into<String>,
        action: TripwireAction,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: TRIPWIRE_SCHEMA_V1,
            id: id.into(),
            preflight_run_id: preflight_run_id.into(),
            tripwire_type,
            condition: condition.into(),
            action,
            state: TripwireState::Armed,
            message: None,
            created_at: created_at.into(),
            last_checked_at: None,
            triggered_at: None,
        }
    }

    /// Set the message.
    #[must_use]
    pub fn with_message(mut self, msg: impl Into<String>) -> Self {
        self.message = Some(msg.into());
        self
    }

    /// Mark as triggered.
    #[must_use]
    pub fn triggered(mut self, at: impl Into<String>) -> Self {
        self.state = TripwireState::Triggered;
        self.triggered_at = Some(at.into());
        self
    }

    /// Update last checked timestamp.
    #[must_use]
    pub fn checked(mut self, at: impl Into<String>) -> Self {
        self.last_checked_at = Some(at.into());
        self
    }

    /// Disarm the tripwire.
    #[must_use]
    pub fn disarmed(mut self) -> Self {
        self.state = TripwireState::Disarmed;
        self
    }
}

/// Type of tripwire.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TripwireType {
    /// File system change detection.
    FileChange,
    /// Resource usage threshold.
    ResourceThreshold,
    /// Time limit exceeded.
    TimeLimit,
    /// Error count threshold.
    ErrorThreshold,
    /// External service health.
    ServiceHealth,
    /// Custom condition.
    #[default]
    Custom,
}

impl TripwireType {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileChange => "file_change",
            Self::ResourceThreshold => "resource_threshold",
            Self::TimeLimit => "time_limit",
            Self::ErrorThreshold => "error_threshold",
            Self::ServiceHealth => "service_health",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for TripwireType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TripwireType {
    type Err = ParseTripwireTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "file_change" => Ok(Self::FileChange),
            "resource_threshold" => Ok(Self::ResourceThreshold),
            "time_limit" => Ok(Self::TimeLimit),
            "error_threshold" => Ok(Self::ErrorThreshold),
            "service_health" => Ok(Self::ServiceHealth),
            "custom" => Ok(Self::Custom),
            _ => Err(ParseTripwireTypeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a tripwire type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTripwireTypeError {
    input: String,
}

impl fmt::Display for ParseTripwireTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown tripwire type `{}`; expected file_change, resource_threshold, time_limit, error_threshold, service_health, or custom",
            self.input
        )
    }
}

impl std::error::Error for ParseTripwireTypeError {}

/// Action to take when a tripwire is triggered.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TripwireAction {
    /// Halt execution immediately.
    Halt,
    /// Pause and wait for confirmation.
    Pause,
    /// Log a warning but continue.
    #[default]
    Warn,
    /// Log for audit but take no action.
    Audit,
}

impl TripwireAction {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::Pause => "pause",
            Self::Warn => "warn",
            Self::Audit => "audit",
        }
    }

    /// Whether this action stops execution.
    #[must_use]
    pub const fn stops_execution(self) -> bool {
        matches!(self, Self::Halt | Self::Pause)
    }
}

impl fmt::Display for TripwireAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TripwireAction {
    type Err = ParseTripwireActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "halt" => Ok(Self::Halt),
            "pause" => Ok(Self::Pause),
            "warn" => Ok(Self::Warn),
            "audit" => Ok(Self::Audit),
            _ => Err(ParseTripwireActionError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a tripwire action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTripwireActionError {
    input: String,
}

impl fmt::Display for ParseTripwireActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown tripwire action `{}`; expected halt, pause, warn, or audit",
            self.input
        )
    }
}

impl std::error::Error for ParseTripwireActionError {}

/// State of a tripwire.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TripwireState {
    /// Tripwire is active and monitoring.
    #[default]
    Armed,
    /// Tripwire has been triggered.
    Triggered,
    /// Tripwire has been disarmed.
    Disarmed,
    /// Tripwire check failed.
    Error,
}

impl TripwireState {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Triggered => "triggered",
            Self::Disarmed => "disarmed",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for TripwireState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TripwireState {
    type Err = ParseTripwireStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "armed" => Ok(Self::Armed),
            "triggered" => Ok(Self::Triggered),
            "disarmed" => Ok(Self::Disarmed),
            "error" => Ok(Self::Error),
            _ => Err(ParseTripwireStateError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a tripwire state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTripwireStateError {
    input: String,
}

impl fmt::Display for ParseTripwireStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown tripwire state `{}`; expected armed, triggered, disarmed, or error",
            self.input
        )
    }
}

impl std::error::Error for ParseTripwireStateError {}

/// An event recording tripwire state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TripwireEvent {
    /// Schema identifier.
    pub schema: &'static str,
    /// Unique event ID.
    pub id: String,
    /// Tripwire that generated this event.
    pub tripwire_id: String,
    /// Type of event.
    pub event_type: TripwireEventType,
    /// State before the event.
    pub previous_state: TripwireState,
    /// State after the event.
    pub new_state: TripwireState,
    /// Event details or context.
    pub details: Option<String>,
    /// Timestamp (RFC 3339).
    pub timestamp: String,
}

impl TripwireEvent {
    /// Create a new tripwire event.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        tripwire_id: impl Into<String>,
        event_type: TripwireEventType,
        previous_state: TripwireState,
        new_state: TripwireState,
        timestamp: impl Into<String>,
    ) -> Self {
        Self {
            schema: TRIPWIRE_EVENT_SCHEMA_V1,
            id: id.into(),
            tripwire_id: tripwire_id.into(),
            event_type,
            previous_state,
            new_state,
            details: None,
            timestamp: timestamp.into(),
        }
    }

    /// Set event details.
    #[must_use]
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
}

/// Type of tripwire event.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TripwireEventType {
    /// Tripwire was armed.
    Armed,
    /// Tripwire was checked.
    Checked,
    /// Tripwire was triggered.
    #[default]
    Triggered,
    /// Tripwire was disarmed.
    Disarmed,
    /// Tripwire check encountered an error.
    Error,
    /// Tripwire was reset.
    Reset,
}

impl TripwireEventType {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Checked => "checked",
            Self::Triggered => "triggered",
            Self::Disarmed => "disarmed",
            Self::Error => "error",
            Self::Reset => "reset",
        }
    }
}

impl fmt::Display for TripwireEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TripwireEventType {
    type Err = ParseTripwireEventTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "armed" => Ok(Self::Armed),
            "checked" => Ok(Self::Checked),
            "triggered" => Ok(Self::Triggered),
            "disarmed" => Ok(Self::Disarmed),
            "error" => Ok(Self::Error),
            "reset" => Ok(Self::Reset),
            _ => Err(ParseTripwireEventTypeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a tripwire event type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTripwireEventTypeError {
    input: String,
}

impl fmt::Display for ParseTripwireEventTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown tripwire event type `{}`; expected armed, checked, triggered, disarmed, error, or reset",
            self.input
        )
    }
}

impl std::error::Error for ParseTripwireEventTypeError {}

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
    fn preflight_schema_versions_are_stable() -> TestResult {
        ensure(PREFLIGHT_RUN_SCHEMA_V1, "ee.preflight_run.v1", "run")?;
        ensure(RISK_BRIEF_SCHEMA_V1, "ee.risk_brief.v1", "brief")?;
        ensure(TRIPWIRE_SCHEMA_V1, "ee.tripwire.v1", "tripwire")?;
        ensure(TRIPWIRE_EVENT_SCHEMA_V1, "ee.tripwire_event.v1", "event")
    }

    #[test]
    fn preflight_run_builder() -> TestResult {
        let mut run = PreflightRun::new("pf_001", "Deploy to production", "2026-04-30T12:00:00Z")
            .with_workspace_id("ws_001")
            .with_status(PreflightStatus::Completed)
            .with_risk_brief_id("rb_001")
            .with_risk_level(RiskLevel::High)
            .cleared()
            .with_completed_at("2026-04-30T12:01:00Z")
            .with_duration_ms(60000);

        run.add_tripwire("tw_001");

        ensure(run.schema, PREFLIGHT_RUN_SCHEMA_V1, "schema")?;
        ensure(run.status, PreflightStatus::Completed, "status")?;
        ensure(run.risk_level, RiskLevel::High, "risk")?;
        ensure(run.cleared, true, "cleared")?;
        ensure(run.tripwire_ids.len(), 1, "tripwires")
    }

    #[test]
    fn preflight_run_blocked() -> TestResult {
        let run = PreflightRun::new("pf_002", "Delete database", "2026-04-30T12:00:00Z")
            .with_risk_level(RiskLevel::Critical)
            .blocked("Critical risk: data loss");

        ensure(run.cleared, false, "cleared")?;
        ensure(
            run.block_reason,
            Some("Critical risk: data loss".to_string()),
            "reason",
        )
    }

    #[test]
    fn preflight_status_strings_are_stable() -> TestResult {
        ensure(PreflightStatus::Running.as_str(), "running", "running")?;
        ensure(
            PreflightStatus::Completed.as_str(),
            "completed",
            "completed",
        )?;
        ensure(PreflightStatus::Failed.as_str(), "failed", "failed")?;
        ensure(
            PreflightStatus::Cancelled.as_str(),
            "cancelled",
            "cancelled",
        )?;
        ensure(PreflightStatus::Timeout.as_str(), "timeout", "timeout")
    }

    #[test]
    fn preflight_status_round_trip() -> TestResult {
        for s in [
            PreflightStatus::Running,
            PreflightStatus::Completed,
            PreflightStatus::Failed,
            PreflightStatus::Cancelled,
            PreflightStatus::Timeout,
        ] {
            let parsed = PreflightStatus::from_str(s.as_str());
            ensure(parsed, Ok(s), s.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn risk_level_strings_are_stable() -> TestResult {
        ensure(RiskLevel::None.as_str(), "none", "none")?;
        ensure(RiskLevel::Low.as_str(), "low", "low")?;
        ensure(RiskLevel::Medium.as_str(), "medium", "medium")?;
        ensure(RiskLevel::High.as_str(), "high", "high")?;
        ensure(RiskLevel::Critical.as_str(), "critical", "critical")?;
        ensure(RiskLevel::Unknown.as_str(), "unknown", "unknown")
    }

    #[test]
    fn risk_level_round_trip() -> TestResult {
        for r in [
            RiskLevel::None,
            RiskLevel::Low,
            RiskLevel::Medium,
            RiskLevel::High,
            RiskLevel::Critical,
            RiskLevel::Unknown,
        ] {
            let parsed = RiskLevel::from_str(r.as_str());
            ensure(parsed, Ok(r), r.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn risk_level_blocking_behavior() -> TestResult {
        ensure(RiskLevel::None.should_block(), false, "none")?;
        ensure(RiskLevel::Low.should_block(), false, "low")?;
        ensure(RiskLevel::Medium.should_block(), false, "medium")?;
        ensure(RiskLevel::High.should_block(), false, "high")?;
        ensure(RiskLevel::Critical.should_block(), true, "critical")
    }

    #[test]
    fn risk_level_confirmation_behavior() -> TestResult {
        ensure(RiskLevel::None.requires_confirmation(), false, "none")?;
        ensure(RiskLevel::Low.requires_confirmation(), false, "low")?;
        ensure(RiskLevel::Medium.requires_confirmation(), false, "medium")?;
        ensure(RiskLevel::High.requires_confirmation(), true, "high")?;
        ensure(
            RiskLevel::Critical.requires_confirmation(),
            true,
            "critical",
        )
    }

    #[test]
    fn risk_brief_builder() -> TestResult {
        let mut brief = RiskBrief::new(
            "rb_001",
            "pf_001",
            RiskLevel::Medium,
            "2026-04-30T12:00:00Z",
        )
        .with_summary("Moderate deployment risk");

        brief.add_risk(RiskItem::new(
            RiskCategory::DataLoss,
            RiskLevel::Low,
            "Potential data loss in migration",
        ));
        brief.add_recommendation("Run backup first");
        brief.add_required_permission("write:database");
        brief.add_side_effect("Database downtime");

        ensure(brief.schema, RISK_BRIEF_SCHEMA_V1, "schema")?;
        ensure(brief.risks.len(), 1, "risks")?;
        ensure(brief.recommendations.len(), 1, "recommendations")?;
        ensure(brief.required_permissions.len(), 1, "permissions")?;
        ensure(brief.side_effects.len(), 1, "effects")
    }

    #[test]
    fn risk_item_builder() -> TestResult {
        let item = RiskItem::new(
            RiskCategory::Security,
            RiskLevel::High,
            "SQL injection risk",
        )
        .with_mitigation("Use parameterized queries")
        .with_source("static analysis");

        ensure(item.category, RiskCategory::Security, "category")?;
        ensure(item.level, RiskLevel::High, "level")?;
        ensure(
            item.mitigation,
            Some("Use parameterized queries".to_string()),
            "mitigation",
        )
    }

    #[test]
    fn risk_category_strings_are_stable() -> TestResult {
        ensure(RiskCategory::DataLoss.as_str(), "data_loss", "data_loss")?;
        ensure(RiskCategory::Security.as_str(), "security", "security")?;
        ensure(RiskCategory::Stability.as_str(), "stability", "stability")?;
        ensure(
            RiskCategory::Performance.as_str(),
            "performance",
            "performance",
        )?;
        ensure(
            RiskCategory::ExternalService.as_str(),
            "external_service",
            "external",
        )?;
        ensure(
            RiskCategory::Compliance.as_str(),
            "compliance",
            "compliance",
        )?;
        ensure(
            RiskCategory::Reversibility.as_str(),
            "reversibility",
            "reversibility",
        )?;
        ensure(
            RiskCategory::ResourceExhaustion.as_str(),
            "resource_exhaustion",
            "resource",
        )?;
        ensure(RiskCategory::Other.as_str(), "other", "other")
    }

    #[test]
    fn risk_category_round_trip() -> TestResult {
        for c in [
            RiskCategory::DataLoss,
            RiskCategory::Security,
            RiskCategory::Stability,
            RiskCategory::Performance,
            RiskCategory::ExternalService,
            RiskCategory::Compliance,
            RiskCategory::Reversibility,
            RiskCategory::ResourceExhaustion,
            RiskCategory::Other,
        ] {
            let parsed = RiskCategory::from_str(c.as_str());
            ensure(parsed, Ok(c), c.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn tripwire_builder() -> TestResult {
        let tw = Tripwire::new(
            "tw_001",
            "pf_001",
            TripwireType::ResourceThreshold,
            "memory_mb < 1000",
            TripwireAction::Halt,
            "2026-04-30T12:00:00Z",
        )
        .with_message("Memory threshold exceeded")
        .checked("2026-04-30T12:05:00Z");

        ensure(tw.schema, TRIPWIRE_SCHEMA_V1, "schema")?;
        ensure(tw.tripwire_type, TripwireType::ResourceThreshold, "type")?;
        ensure(tw.action, TripwireAction::Halt, "action")?;
        ensure(tw.state, TripwireState::Armed, "state")
    }

    #[test]
    fn tripwire_triggered() -> TestResult {
        let tw = Tripwire::new(
            "tw_002",
            "pf_001",
            TripwireType::TimeLimit,
            "elapsed_ms < 60000",
            TripwireAction::Warn,
            "2026-04-30T12:00:00Z",
        )
        .triggered("2026-04-30T12:01:00Z");

        ensure(tw.state, TripwireState::Triggered, "state")?;
        ensure(
            tw.triggered_at,
            Some("2026-04-30T12:01:00Z".to_string()),
            "triggered_at",
        )
    }

    #[test]
    fn tripwire_type_strings_are_stable() -> TestResult {
        ensure(TripwireType::FileChange.as_str(), "file_change", "file")?;
        ensure(
            TripwireType::ResourceThreshold.as_str(),
            "resource_threshold",
            "resource",
        )?;
        ensure(TripwireType::TimeLimit.as_str(), "time_limit", "time")?;
        ensure(
            TripwireType::ErrorThreshold.as_str(),
            "error_threshold",
            "error",
        )?;
        ensure(
            TripwireType::ServiceHealth.as_str(),
            "service_health",
            "service",
        )?;
        ensure(TripwireType::Custom.as_str(), "custom", "custom")
    }

    #[test]
    fn tripwire_type_round_trip() -> TestResult {
        for t in [
            TripwireType::FileChange,
            TripwireType::ResourceThreshold,
            TripwireType::TimeLimit,
            TripwireType::ErrorThreshold,
            TripwireType::ServiceHealth,
            TripwireType::Custom,
        ] {
            let parsed = TripwireType::from_str(t.as_str());
            ensure(parsed, Ok(t), t.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn tripwire_action_strings_are_stable() -> TestResult {
        ensure(TripwireAction::Halt.as_str(), "halt", "halt")?;
        ensure(TripwireAction::Pause.as_str(), "pause", "pause")?;
        ensure(TripwireAction::Warn.as_str(), "warn", "warn")?;
        ensure(TripwireAction::Audit.as_str(), "audit", "audit")
    }

    #[test]
    fn tripwire_action_round_trip() -> TestResult {
        for a in [
            TripwireAction::Halt,
            TripwireAction::Pause,
            TripwireAction::Warn,
            TripwireAction::Audit,
        ] {
            let parsed = TripwireAction::from_str(a.as_str());
            ensure(parsed, Ok(a), a.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn tripwire_action_stops_execution() -> TestResult {
        ensure(TripwireAction::Halt.stops_execution(), true, "halt")?;
        ensure(TripwireAction::Pause.stops_execution(), true, "pause")?;
        ensure(TripwireAction::Warn.stops_execution(), false, "warn")?;
        ensure(TripwireAction::Audit.stops_execution(), false, "audit")
    }

    #[test]
    fn tripwire_state_strings_are_stable() -> TestResult {
        ensure(TripwireState::Armed.as_str(), "armed", "armed")?;
        ensure(TripwireState::Triggered.as_str(), "triggered", "triggered")?;
        ensure(TripwireState::Disarmed.as_str(), "disarmed", "disarmed")?;
        ensure(TripwireState::Error.as_str(), "error", "error")
    }

    #[test]
    fn tripwire_state_round_trip() -> TestResult {
        for s in [
            TripwireState::Armed,
            TripwireState::Triggered,
            TripwireState::Disarmed,
            TripwireState::Error,
        ] {
            let parsed = TripwireState::from_str(s.as_str());
            ensure(parsed, Ok(s), s.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn tripwire_event_builder() -> TestResult {
        let event = TripwireEvent::new(
            "twe_001",
            "tw_001",
            TripwireEventType::Triggered,
            TripwireState::Armed,
            TripwireState::Triggered,
            "2026-04-30T12:01:00Z",
        )
        .with_details("Memory usage exceeded 1GB");

        ensure(event.schema, TRIPWIRE_EVENT_SCHEMA_V1, "schema")?;
        ensure(event.event_type, TripwireEventType::Triggered, "type")?;
        ensure(event.previous_state, TripwireState::Armed, "prev")?;
        ensure(event.new_state, TripwireState::Triggered, "new")
    }

    #[test]
    fn tripwire_event_type_strings_are_stable() -> TestResult {
        ensure(TripwireEventType::Armed.as_str(), "armed", "armed")?;
        ensure(TripwireEventType::Checked.as_str(), "checked", "checked")?;
        ensure(
            TripwireEventType::Triggered.as_str(),
            "triggered",
            "triggered",
        )?;
        ensure(TripwireEventType::Disarmed.as_str(), "disarmed", "disarmed")?;
        ensure(TripwireEventType::Error.as_str(), "error", "error")?;
        ensure(TripwireEventType::Reset.as_str(), "reset", "reset")
    }

    #[test]
    fn tripwire_event_type_round_trip() -> TestResult {
        for e in [
            TripwireEventType::Armed,
            TripwireEventType::Checked,
            TripwireEventType::Triggered,
            TripwireEventType::Disarmed,
            TripwireEventType::Error,
            TripwireEventType::Reset,
        ] {
            let parsed = TripwireEventType::from_str(e.as_str());
            ensure(parsed, Ok(e), e.as_str())?;
        }
        Ok(())
    }
}
