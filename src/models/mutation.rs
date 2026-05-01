//! Universal dry-run and idempotency response contracts (EE-319).
//!
//! Provides standard contracts for mutating commands to report:
//! - What would happen in dry-run mode
//! - Whether the operation is idempotent
//! - What actions would be taken vs skipped

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Schema identifier for mutation response.
pub const MUTATION_RESPONSE_SCHEMA_V1: &str = "ee.mutation.v1";

/// Schema identifier for dry-run previews.
pub const DRY_RUN_PREVIEW_SCHEMA_V1: &str = "ee.dry_run_preview.v1";

/// Status of a planned mutation action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationActionStatus {
    /// Action would be performed.
    Pending,
    /// Action would be skipped (already done or not needed).
    Skipped,
    /// Action was performed successfully.
    Completed,
    /// Action failed.
    Failed,
    /// Action was blocked by a policy or constraint.
    Blocked,
}

impl MutationActionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Skipped => "skipped",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }

    #[must_use]
    pub const fn is_actionable(self) -> bool {
        matches!(self, Self::Pending)
    }
}

impl fmt::Display for MutationActionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseMutationActionStatusError {
    pub invalid: String,
}

impl fmt::Display for ParseMutationActionStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid mutation action status '{}'; expected one of: pending, skipped, completed, failed, blocked",
            self.invalid
        )
    }
}

impl std::error::Error for ParseMutationActionStatusError {}

impl FromStr for MutationActionStatus {
    type Err = ParseMutationActionStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "skipped" => Ok(Self::Skipped),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "blocked" => Ok(Self::Blocked),
            _ => Err(ParseMutationActionStatusError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// Type of mutation action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationActionType {
    /// Create a new resource.
    Create,
    /// Update an existing resource.
    Update,
    /// Delete a resource.
    Delete,
    /// Archive a resource (soft delete).
    Archive,
    /// Restore a previously archived resource.
    Restore,
    /// Initialize a new workspace or structure.
    Initialize,
    /// Migrate data or schema.
    Migrate,
    /// Rebuild an index or derived structure.
    Rebuild,
    /// Import data from external source.
    Import,
    /// Export data to external destination.
    Export,
}

impl MutationActionType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Archive => "archive",
            Self::Restore => "restore",
            Self::Initialize => "initialize",
            Self::Migrate => "migrate",
            Self::Rebuild => "rebuild",
            Self::Import => "import",
            Self::Export => "export",
        }
    }

    #[must_use]
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Delete | Self::Migrate)
    }

    #[must_use]
    pub const fn requires_confirmation(self) -> bool {
        matches!(self, Self::Delete | Self::Migrate | Self::Rebuild)
    }
}

impl fmt::Display for MutationActionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseMutationActionTypeError {
    pub invalid: String,
}

impl fmt::Display for ParseMutationActionTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid mutation action type '{}'; expected one of: create, update, delete, archive, restore, initialize, migrate, rebuild, import, export",
            self.invalid
        )
    }
}

impl std::error::Error for ParseMutationActionTypeError {}

impl FromStr for MutationActionType {
    type Err = ParseMutationActionTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "create" => Ok(Self::Create),
            "update" => Ok(Self::Update),
            "delete" => Ok(Self::Delete),
            "archive" => Ok(Self::Archive),
            "restore" => Ok(Self::Restore),
            "initialize" => Ok(Self::Initialize),
            "migrate" => Ok(Self::Migrate),
            "rebuild" => Ok(Self::Rebuild),
            "import" => Ok(Self::Import),
            "export" => Ok(Self::Export),
            _ => Err(ParseMutationActionTypeError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// A single planned mutation action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlannedAction {
    /// Type of action.
    pub action_type: MutationActionType,
    /// Status of the action.
    pub status: MutationActionStatus,
    /// Target resource (path, ID, or description).
    pub target: String,
    /// Human-readable description of what would happen.
    pub description: String,
    /// Reason for skip/block if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Estimated size change in bytes (positive = growth, negative = reduction).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_delta_bytes: Option<i64>,
}

impl PlannedAction {
    #[must_use]
    pub fn new(
        action_type: MutationActionType,
        target: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            action_type,
            status: MutationActionStatus::Pending,
            target: target.into(),
            description: description.into(),
            reason: None,
            size_delta_bytes: None,
        }
    }

    #[must_use]
    pub fn skipped(mut self, reason: impl Into<String>) -> Self {
        self.status = MutationActionStatus::Skipped;
        self.reason = Some(reason.into());
        self
    }

    #[must_use]
    pub fn blocked(mut self, reason: impl Into<String>) -> Self {
        self.status = MutationActionStatus::Blocked;
        self.reason = Some(reason.into());
        self
    }

    #[must_use]
    pub fn with_size_delta(mut self, bytes: i64) -> Self {
        self.size_delta_bytes = Some(bytes);
        self
    }
}

/// Idempotency classification for a mutation operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyClass {
    /// Operation is fully idempotent (same result regardless of repetition).
    FullyIdempotent,
    /// Operation is conditionally idempotent (same result if preconditions match).
    ConditionallyIdempotent,
    /// Operation is not idempotent (repeated execution has cumulative effects).
    NotIdempotent,
}

impl IdempotencyClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FullyIdempotent => "fully_idempotent",
            Self::ConditionallyIdempotent => "conditionally_idempotent",
            Self::NotIdempotent => "not_idempotent",
        }
    }

    #[must_use]
    pub const fn safe_to_retry(self) -> bool {
        matches!(self, Self::FullyIdempotent | Self::ConditionallyIdempotent)
    }
}

impl fmt::Display for IdempotencyClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseIdempotencyClassError {
    pub invalid: String,
}

impl fmt::Display for ParseIdempotencyClassError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid idempotency class '{}'; expected one of: fully_idempotent, conditionally_idempotent, not_idempotent",
            self.invalid
        )
    }
}

impl std::error::Error for ParseIdempotencyClassError {}

impl FromStr for IdempotencyClass {
    type Err = ParseIdempotencyClassError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fully_idempotent" => Ok(Self::FullyIdempotent),
            "conditionally_idempotent" => Ok(Self::ConditionallyIdempotent),
            "not_idempotent" => Ok(Self::NotIdempotent),
            _ => Err(ParseIdempotencyClassError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// A dry-run preview showing what would happen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DryRunPreview {
    /// Schema identifier.
    pub schema: String,
    /// Command that was previewed.
    pub command: String,
    /// Whether the operation would succeed.
    pub would_succeed: bool,
    /// Idempotency classification.
    pub idempotency: IdempotencyClass,
    /// Planned actions.
    pub actions: Vec<PlannedAction>,
    /// Summary counts.
    pub summary: DryRunSummary,
    /// Warnings or notes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl DryRunPreview {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            schema: DRY_RUN_PREVIEW_SCHEMA_V1.to_owned(),
            command: command.into(),
            would_succeed: true,
            idempotency: IdempotencyClass::FullyIdempotent,
            actions: Vec::new(),
            summary: DryRunSummary::default(),
            warnings: Vec::new(),
        }
    }

    pub fn add_action(&mut self, action: PlannedAction) {
        match action.status {
            MutationActionStatus::Pending => self.summary.pending += 1,
            MutationActionStatus::Skipped => self.summary.skipped += 1,
            MutationActionStatus::Blocked => {
                self.summary.blocked += 1;
                self.would_succeed = false;
            }
            _ => {}
        }
        if let Some(delta) = action.size_delta_bytes {
            self.summary.total_size_delta_bytes += delta;
        }
        self.actions.push(action);
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    #[must_use]
    pub fn with_idempotency(mut self, class: IdempotencyClass) -> Self {
        self.idempotency = class;
        self
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

/// Summary counts for a dry-run preview.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DryRunSummary {
    /// Number of actions that would be executed.
    pub pending: u32,
    /// Number of actions that would be skipped.
    pub skipped: u32,
    /// Number of actions blocked by constraints.
    pub blocked: u32,
    /// Net size change in bytes.
    pub total_size_delta_bytes: i64,
}

impl DryRunSummary {
    #[must_use]
    pub fn has_work(&self) -> bool {
        self.pending > 0
    }

    #[must_use]
    pub fn would_succeed(&self) -> bool {
        self.blocked == 0
    }
}

/// Response for a mutation operation (whether executed or dry-run).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MutationResponse {
    /// Schema identifier.
    pub schema: String,
    /// Whether this was a dry-run.
    pub dry_run: bool,
    /// Command that was executed.
    pub command: String,
    /// Whether the operation succeeded.
    pub succeeded: bool,
    /// Idempotency classification.
    pub idempotency: IdempotencyClass,
    /// Actions taken (or planned if dry-run).
    pub actions: Vec<PlannedAction>,
    /// Summary.
    pub summary: MutationSummary,
    /// Warnings.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl MutationResponse {
    #[must_use]
    pub fn new(command: impl Into<String>, dry_run: bool) -> Self {
        Self {
            schema: MUTATION_RESPONSE_SCHEMA_V1.to_owned(),
            dry_run,
            command: command.into(),
            succeeded: true,
            idempotency: IdempotencyClass::FullyIdempotent,
            actions: Vec::new(),
            summary: MutationSummary::default(),
            warnings: Vec::new(),
            error: None,
        }
    }

    pub fn record_success(&mut self, action: &mut PlannedAction) {
        action.status = MutationActionStatus::Completed;
        self.summary.completed += 1;
    }

    pub fn record_failure(&mut self, action: &mut PlannedAction, error: impl Into<String>) {
        action.status = MutationActionStatus::Failed;
        action.reason = Some(error.into());
        self.summary.failed += 1;
        self.succeeded = false;
    }

    pub fn record_skip(&mut self, action: &mut PlannedAction, reason: impl Into<String>) {
        action.status = MutationActionStatus::Skipped;
        action.reason = Some(reason.into());
        self.summary.skipped += 1;
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Summary for a mutation operation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MutationSummary {
    /// Number of actions completed.
    pub completed: u32,
    /// Number of actions skipped.
    pub skipped: u32,
    /// Number of actions that failed.
    pub failed: u32,
}

impl MutationSummary {
    #[must_use]
    pub fn total(&self) -> u32 {
        self.completed + self.skipped + self.failed
    }
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
    fn mutation_action_status_roundtrip() -> TestResult {
        for status in [
            MutationActionStatus::Pending,
            MutationActionStatus::Skipped,
            MutationActionStatus::Completed,
            MutationActionStatus::Failed,
            MutationActionStatus::Blocked,
        ] {
            let s = status.as_str();
            let parsed: MutationActionStatus = s
                .parse()
                .map_err(|e: ParseMutationActionStatusError| e.to_string())?;
            ensure(parsed, status, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn mutation_action_type_roundtrip() -> TestResult {
        for action_type in [
            MutationActionType::Create,
            MutationActionType::Update,
            MutationActionType::Delete,
            MutationActionType::Archive,
            MutationActionType::Restore,
            MutationActionType::Initialize,
            MutationActionType::Migrate,
            MutationActionType::Rebuild,
            MutationActionType::Import,
            MutationActionType::Export,
        ] {
            let s = action_type.as_str();
            let parsed: MutationActionType = s
                .parse()
                .map_err(|e: ParseMutationActionTypeError| e.to_string())?;
            ensure(parsed, action_type, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn idempotency_class_roundtrip() -> TestResult {
        for class in [
            IdempotencyClass::FullyIdempotent,
            IdempotencyClass::ConditionallyIdempotent,
            IdempotencyClass::NotIdempotent,
        ] {
            let s = class.as_str();
            let parsed: IdempotencyClass = s
                .parse()
                .map_err(|e: ParseIdempotencyClassError| e.to_string())?;
            ensure(parsed, class, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn dry_run_preview_tracks_actions() {
        let mut preview = DryRunPreview::new("ee remember");

        preview.add_action(PlannedAction::new(
            MutationActionType::Create,
            "memory_123",
            "Create new memory",
        ));
        preview.add_action(
            PlannedAction::new(MutationActionType::Update, "index", "Update search index")
                .skipped("Index already up to date"),
        );

        assert!(preview.would_succeed);
        assert_eq!(preview.summary.pending, 1);
        assert_eq!(preview.summary.skipped, 1);
        assert_eq!(preview.summary.blocked, 0);
    }

    #[test]
    fn dry_run_preview_blocked_action_fails() {
        let mut preview = DryRunPreview::new("ee delete");

        preview.add_action(
            PlannedAction::new(MutationActionType::Delete, "memory_456", "Delete memory")
                .blocked("Policy denies deletion"),
        );

        assert!(!preview.would_succeed);
        assert_eq!(preview.summary.blocked, 1);
    }

    #[test]
    fn planned_action_serializes_correctly() -> TestResult {
        let action = PlannedAction::new(
            MutationActionType::Create,
            ".ee/config.toml",
            "Create config file",
        )
        .with_size_delta(1024);

        let json = serde_json::to_string(&action).map_err(|e| e.to_string())?;
        assert!(json.contains("\"action_type\":\"create\""));
        assert!(json.contains("\"status\":\"pending\""));
        assert!(json.contains("\"size_delta_bytes\":1024"));
        Ok(())
    }

    #[test]
    fn mutation_response_tracks_results() {
        let mut response = MutationResponse::new("ee init", false);

        let mut action = PlannedAction::new(
            MutationActionType::Initialize,
            ".ee",
            "Initialize workspace",
        );
        response.record_success(&mut action);

        assert!(response.succeeded);
        assert_eq!(response.summary.completed, 1);
    }

    #[test]
    fn idempotency_safe_to_retry() {
        assert!(IdempotencyClass::FullyIdempotent.safe_to_retry());
        assert!(IdempotencyClass::ConditionallyIdempotent.safe_to_retry());
        assert!(!IdempotencyClass::NotIdempotent.safe_to_retry());
    }

    #[test]
    fn action_type_properties() {
        assert!(MutationActionType::Delete.is_destructive());
        assert!(MutationActionType::Migrate.is_destructive());
        assert!(!MutationActionType::Create.is_destructive());

        assert!(MutationActionType::Delete.requires_confirmation());
        assert!(MutationActionType::Migrate.requires_confirmation());
        assert!(MutationActionType::Rebuild.requires_confirmation());
        assert!(!MutationActionType::Update.requires_confirmation());
    }
}
