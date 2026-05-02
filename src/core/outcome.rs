//! Outcome to CLI boundary mapping (EE-009).
//!
//! Maps Asupersync's `Outcome<T, E>` to CLI exit codes and error responses.
//!
//! # Outcome Severity Lattice
//!
//! Asupersync defines a severity lattice where worse outcomes dominate:
//! `Ok < Err < Cancelled < Panicked`
//!
//! This module maps that lattice to CLI exit codes:
//! - `Ok(T)` → exit 0 (success)
//! - `Err(DomainError)` → exit 1-8 (domain-specific errors)
//! - `Cancelled` → exit 130 (SIGINT convention)
//! - `Panicked` → exit 101 (Rust panic convention)
//!
//! # Usage
//!
//! ```ignore
//! use ee::core::outcome::{CliOutcome, outcome_exit_code};
//! use asupersync::Outcome;
//!
//! let outcome: Outcome<(), DomainError> = Outcome::ok(());
//! let exit_code = outcome_exit_code(&outcome);
//! ```

use std::path::Path;

use asupersync::Outcome;
use asupersync::types::{CancelKind, CancelReason, PanicPayload};

use crate::db::{
    AuditedFeedbackEventInput, CreateFeedbackEventInput, DbConnection, FeedbackCounts,
    StoredFeedbackEvent, feedback_scoring,
};
use crate::models::{DomainError, ProcessExitCode};

/// Exit code for cancelled operations (SIGINT convention).
pub const EXIT_CANCELLED: u8 = 130;

/// Exit code for panicked operations (Rust panic convention).
pub const EXIT_PANICKED: u8 = 101;

/// CLI outcome classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliOutcomeClass {
    /// Operation succeeded.
    Success,
    /// Domain-level error (usage, config, storage, etc.).
    DomainError,
    /// Operation was cancelled (budget exhausted, timeout, signal).
    Cancelled,
    /// Operation panicked.
    Panicked,
}

impl CliOutcomeClass {
    /// Stable string form for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::DomainError => "domain_error",
            Self::Cancelled => "cancelled",
            Self::Panicked => "panicked",
        }
    }

    /// Whether this outcome class is terminal (no further progress possible).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Success)
    }
}

/// Cancel reason classification for CLI output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliCancelReason {
    /// Budget exhausted (time, polls, cost).
    BudgetExhausted,
    /// Explicit cancellation requested.
    UserRequested,
    /// Timeout or deadline exceeded.
    Timeout,
    /// Parent scope was cancelled.
    ParentCancelled,
    /// Shutdown requested.
    Shutdown,
    /// Other cancellation reason.
    Other,
}

impl CliCancelReason {
    /// Stable string form for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExhausted => "budget_exhausted",
            Self::UserRequested => "user_requested",
            Self::Timeout => "timeout",
            Self::ParentCancelled => "parent_cancelled",
            Self::Shutdown => "shutdown",
            Self::Other => "other",
        }
    }
}

impl From<&CancelReason> for CliCancelReason {
    fn from(reason: &CancelReason) -> Self {
        match reason.kind {
            CancelKind::PollQuota | CancelKind::CostBudget | CancelKind::Deadline => {
                Self::BudgetExhausted
            }
            CancelKind::User => Self::UserRequested,
            CancelKind::Timeout => Self::Timeout,
            CancelKind::ParentCancelled => Self::ParentCancelled,
            CancelKind::Shutdown => Self::Shutdown,
            CancelKind::FailFast
            | CancelKind::RaceLost
            | CancelKind::ResourceUnavailable
            | CancelKind::LinkedExit => Self::Other,
        }
    }
}

/// Get the exit code for an Outcome.
///
/// Maps the Outcome severity lattice to Unix exit codes:
/// - `Ok` → 0
/// - `Err(DomainError)` → domain-specific exit code (1-8)
/// - `Cancelled` → 130 (SIGINT convention)
/// - `Panicked` → 101 (Rust panic convention)
#[must_use]
pub fn outcome_exit_code<T>(outcome: &Outcome<T, DomainError>) -> u8 {
    match outcome {
        Outcome::Ok(_) => ProcessExitCode::Success as u8,
        Outcome::Err(e) => e.exit_code() as u8,
        Outcome::Cancelled(_) => EXIT_CANCELLED,
        Outcome::Panicked(_) => EXIT_PANICKED,
    }
}

/// Get the outcome class for an Outcome.
#[must_use]
pub fn outcome_class<T, E>(outcome: &Outcome<T, E>) -> CliOutcomeClass {
    match outcome {
        Outcome::Ok(_) => CliOutcomeClass::Success,
        Outcome::Err(_) => CliOutcomeClass::DomainError,
        Outcome::Cancelled(_) => CliOutcomeClass::Cancelled,
        Outcome::Panicked(_) => CliOutcomeClass::Panicked,
    }
}

/// Extract a human-readable message from a cancelled outcome.
#[must_use]
pub fn cancel_message(reason: &CancelReason) -> String {
    if let Some(msg) = &reason.message {
        return msg.clone();
    }
    match reason.kind {
        CancelKind::User => "Cancellation requested.".to_string(),
        CancelKind::Timeout => "Operation timed out.".to_string(),
        CancelKind::Deadline => "Deadline exceeded.".to_string(),
        CancelKind::PollQuota => "Poll budget exhausted.".to_string(),
        CancelKind::CostBudget => "Cost budget exhausted.".to_string(),
        CancelKind::FailFast => "Sibling operation failed.".to_string(),
        CancelKind::RaceLost => "Lost race to another operation.".to_string(),
        CancelKind::ParentCancelled => "Parent operation was cancelled.".to_string(),
        CancelKind::ResourceUnavailable => "Resource unavailable.".to_string(),
        CancelKind::Shutdown => "Runtime shutdown.".to_string(),
        CancelKind::LinkedExit => "Linked task exited.".to_string(),
    }
}

/// Extract a human-readable message from a panicked outcome.
#[must_use]
pub fn panic_message(payload: &PanicPayload) -> String {
    payload.message().to_string()
}

/// A CLI-ready outcome summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliOutcomeSummary {
    pub class: CliOutcomeClass,
    pub exit_code: u8,
    pub message: Option<String>,
    pub cancel_reason: Option<CliCancelReason>,
}

impl CliOutcomeSummary {
    /// Create a summary from an Outcome.
    #[must_use]
    pub fn from_outcome<T>(outcome: &Outcome<T, DomainError>) -> Self {
        match outcome {
            Outcome::Ok(_) => Self {
                class: CliOutcomeClass::Success,
                exit_code: 0,
                message: None,
                cancel_reason: None,
            },
            Outcome::Err(e) => Self {
                class: CliOutcomeClass::DomainError,
                exit_code: e.exit_code() as u8,
                message: Some(e.message().to_string()),
                cancel_reason: None,
            },
            Outcome::Cancelled(reason) => Self {
                class: CliOutcomeClass::Cancelled,
                exit_code: EXIT_CANCELLED,
                message: Some(cancel_message(reason)),
                cancel_reason: Some(CliCancelReason::from(reason)),
            },
            Outcome::Panicked(payload) => Self {
                class: CliOutcomeClass::Panicked,
                exit_code: EXIT_PANICKED,
                message: Some(panic_message(payload)),
                cancel_reason: None,
            },
        }
    }

    /// Whether this outcome represents success.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self.class, CliOutcomeClass::Success)
    }
}

const ALLOWED_TARGET_TYPES: &[&str] = &["memory", "rule", "session", "source", "pack", "candidate"];
const ALLOWED_SIGNALS: &[&str] = &[
    "positive",
    "negative",
    "neutral",
    "contradiction",
    "confirmation",
    "harmful",
    "helpful",
    "stale",
    "inaccurate",
    "outdated",
];
const ALLOWED_SOURCE_TYPES: &[&str] = &[
    "human_explicit",
    "agent_inference",
    "automated_check",
    "outcome_observed",
    "contradiction_detected",
    "usage_pattern",
    "decay_trigger",
];

/// Status returned by the `ee outcome` feedback recording use case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutcomeRecordStatus {
    /// The feedback event was persisted and audited.
    Recorded,
    /// The command validated inputs but did not mutate storage.
    DryRun,
    /// A caller-supplied event ID already existed with matching content.
    AlreadyRecorded,
}

impl OutcomeRecordStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::DryRun => "dry_run",
            Self::AlreadyRecorded => "already_recorded",
        }
    }
}

/// Options for recording observed outcome feedback.
#[derive(Clone, Debug)]
pub struct OutcomeRecordOptions<'a> {
    pub database_path: &'a Path,
    pub target_type: String,
    pub target_id: String,
    pub workspace_id: Option<String>,
    pub signal: String,
    pub weight: Option<f32>,
    pub source_type: String,
    pub source_id: Option<String>,
    pub reason: Option<String>,
    pub evidence_json: Option<String>,
    pub session_id: Option<String>,
    pub event_id: Option<String>,
    pub actor: Option<String>,
    pub dry_run: bool,
}

/// Aggregated feedback summary exposed by `ee outcome`.
#[derive(Clone, Debug, PartialEq)]
pub struct OutcomeFeedbackSummary {
    pub positive_weight: f32,
    pub positive_count: u32,
    pub negative_weight: f32,
    pub negative_count: u32,
    pub neutral_weight: f32,
    pub neutral_count: u32,
    pub decay_weight: f32,
    pub decay_count: u32,
    pub total_count: u32,
    pub net_score: f32,
    pub trust_score: f32,
}

impl OutcomeFeedbackSummary {
    #[must_use]
    pub fn from_counts(counts: &FeedbackCounts) -> Self {
        Self {
            positive_weight: counts.positive_weight,
            positive_count: counts.positive_count,
            negative_weight: counts.negative_weight,
            negative_count: counts.negative_count,
            neutral_weight: counts.neutral_weight,
            neutral_count: counts.neutral_count,
            decay_weight: counts.decay_weight,
            decay_count: counts.decay_count,
            total_count: counts.total_count(),
            net_score: counts.net_score(),
            trust_score: counts.trust_score(),
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "positiveWeight": score_json_value(self.positive_weight),
            "positiveCount": self.positive_count,
            "negativeWeight": score_json_value(self.negative_weight),
            "negativeCount": self.negative_count,
            "neutralWeight": score_json_value(self.neutral_weight),
            "neutralCount": self.neutral_count,
            "decayWeight": score_json_value(self.decay_weight),
            "decayCount": self.decay_count,
            "totalCount": self.total_count,
            "netScore": score_json_value(self.net_score),
            "trustScore": score_json_value(self.trust_score),
        })
    }
}

/// Result of recording outcome feedback.
#[derive(Clone, Debug, PartialEq)]
pub struct OutcomeRecordReport {
    pub version: &'static str,
    pub status: OutcomeRecordStatus,
    pub dry_run: bool,
    pub event_id: Option<String>,
    pub audit_id: Option<String>,
    pub target_type: String,
    pub target_id: String,
    pub workspace_id: String,
    pub target_verified: bool,
    pub signal: String,
    pub weight: f32,
    pub source_type: String,
    pub source_id: Option<String>,
    pub reason_present: bool,
    pub evidence_json_present: bool,
    pub session_id: Option<String>,
    pub feedback: OutcomeFeedbackSummary,
}

impl OutcomeRecordReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let action = match self.status {
            OutcomeRecordStatus::Recorded => "Recorded outcome feedback",
            OutcomeRecordStatus::DryRun => "DRY RUN: Would record outcome feedback",
            OutcomeRecordStatus::AlreadyRecorded => "Outcome feedback already recorded",
        };

        let mut output = String::new();
        output.push_str(action);
        output.push_str("\n\n");
        output.push_str(&format!(
            "  Target: {} {}\n",
            self.target_type, self.target_id
        ));
        output.push_str(&format!("  Signal: {}\n", self.signal));
        output.push_str(&format!("  Weight: {:.4}\n", self.weight));
        output.push_str(&format!("  Source: {}\n", self.source_type));
        if let Some(ref event_id) = self.event_id {
            output.push_str(&format!("  Event: {event_id}\n"));
        }
        if let Some(ref audit_id) = self.audit_id {
            output.push_str(&format!("  Audit: {audit_id}\n"));
        }
        output.push_str(&format!(
            "  Feedback total: {}\n",
            self.feedback.total_count
        ));
        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "outcome",
            "version": self.version,
            "status": self.status.as_str(),
            "dryRun": self.dry_run,
            "target": {
                "type": &self.target_type,
                "id": &self.target_id,
                "workspaceId": &self.workspace_id,
                "verified": self.target_verified,
            },
            "event": {
                "id": &self.event_id,
                "auditId": &self.audit_id,
                "signal": &self.signal,
                "weight": score_json_value(self.weight),
                "sourceType": &self.source_type,
                "sourceId": &self.source_id,
                "reasonPresent": self.reason_present,
                "evidenceJsonPresent": self.evidence_json_present,
                "sessionId": &self.session_id,
            },
            "feedback": self.feedback.data_json(),
        })
    }
}

/// Record observed feedback about a memory or related target.
///
/// The command verifies memory targets, validates machine-facing fields,
/// supports dry-run, and writes the feedback event with an audit log entry.
pub fn record_outcome(
    options: &OutcomeRecordOptions<'_>,
) -> Result<OutcomeRecordReport, DomainError> {
    let target_type = require_allowed(
        "target type",
        &options.target_type,
        ALLOWED_TARGET_TYPES,
        "ee outcome <target-id> --target-type memory",
    )?;
    let target_id = require_nonempty("target id", &options.target_id, "ee outcome <target-id>")?;
    let signal = require_allowed(
        "signal",
        &options.signal,
        ALLOWED_SIGNALS,
        "ee outcome <target-id> --signal helpful",
    )?;
    let source_type = require_allowed(
        "source type",
        &options.source_type,
        ALLOWED_SOURCE_TYPES,
        "ee outcome <target-id> --source-type outcome_observed",
    )?;
    let source_id = normalize_optional_text("source id", options.source_id.as_deref())?;
    let reason = normalize_optional_text("reason", options.reason.as_deref())?;
    let evidence_json = normalize_evidence_json(options.evidence_json.as_deref())?;
    let session_id = normalize_optional_text("session id", options.session_id.as_deref())?;
    let event_id = match options.event_id.as_deref() {
        Some(raw) => Some(validate_feedback_event_id(raw)?),
        None if options.dry_run => None,
        None => Some(generate_feedback_event_id()),
    };
    let weight = options.weight.map_or_else(
        || Ok(default_feedback_weight(&source_type, &signal)),
        validate_weight,
    )?;

    if !options.database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", options.database_path.display()),
            repair: Some("ee init --workspace .".to_string()),
        });
    }

    let connection =
        DbConnection::open_file(options.database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let target = resolve_target_workspace(
        &connection,
        &target_type,
        &target_id,
        options.workspace_id.as_deref(),
    )?;

    let feedback_input = CreateFeedbackEventInput {
        workspace_id: target.workspace_id.clone(),
        target_type: target_type.clone(),
        target_id: target_id.clone(),
        signal: signal.clone(),
        weight,
        source_type: source_type.clone(),
        source_id: source_id.clone(),
        reason,
        evidence_json: evidence_json.clone(),
        session_id: session_id.clone(),
    };

    if options.dry_run {
        let feedback = current_feedback_summary(&connection, &target_type, &target_id)?;
        return Ok(OutcomeRecordReport {
            version: env!("CARGO_PKG_VERSION"),
            status: OutcomeRecordStatus::DryRun,
            dry_run: true,
            event_id,
            audit_id: None,
            target_type,
            target_id,
            workspace_id: target.workspace_id,
            target_verified: target.verified,
            signal,
            weight,
            source_type,
            source_id,
            reason_present: feedback_input.reason.is_some(),
            evidence_json_present: evidence_json.is_some(),
            session_id,
            feedback,
        });
    }

    let Some(event_id) = event_id else {
        return Err(DomainError::Usage {
            message: "event id was not generated for outcome write".to_string(),
            repair: Some("ee outcome <target-id> --signal helpful".to_string()),
        });
    };
    if let Some(existing) = get_existing_event(&connection, &event_id)? {
        if feedback_event_matches(&existing, &feedback_input) {
            let feedback = current_feedback_summary(&connection, &target_type, &target_id)?;
            return Ok(OutcomeRecordReport {
                version: env!("CARGO_PKG_VERSION"),
                status: OutcomeRecordStatus::AlreadyRecorded,
                dry_run: false,
                event_id: Some(event_id),
                audit_id: None,
                target_type,
                target_id,
                workspace_id: target.workspace_id,
                target_verified: target.verified,
                signal,
                weight,
                source_type,
                source_id,
                reason_present: feedback_input.reason.is_some(),
                evidence_json_present: evidence_json.is_some(),
                session_id,
                feedback,
            });
        }

        return Err(DomainError::Usage {
            message: format!("feedback event id already exists with different content: {event_id}"),
            repair: Some("ee outcome --event-id <new-feedback-id>".to_string()),
        });
    }

    let audit_id = connection
        .insert_feedback_event_audited(
            &event_id,
            &AuditedFeedbackEventInput {
                event: feedback_input.clone(),
                actor: options.actor.clone(),
                details: Some(outcome_audit_details(&event_id, &feedback_input)),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to record feedback event: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let feedback = current_feedback_summary(&connection, &target_type, &target_id)?;

    Ok(OutcomeRecordReport {
        version: env!("CARGO_PKG_VERSION"),
        status: OutcomeRecordStatus::Recorded,
        dry_run: false,
        event_id: Some(event_id),
        audit_id: Some(audit_id),
        target_type,
        target_id,
        workspace_id: target.workspace_id,
        target_verified: target.verified,
        signal,
        weight,
        source_type,
        source_id,
        reason_present: feedback_input.reason.is_some(),
        evidence_json_present: evidence_json.is_some(),
        session_id,
        feedback,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TargetResolution {
    workspace_id: String,
    verified: bool,
}

fn resolve_target_workspace(
    connection: &DbConnection,
    target_type: &str,
    target_id: &str,
    workspace_id: Option<&str>,
) -> Result<TargetResolution, DomainError> {
    if target_type == "memory" {
        let memory = connection
            .get_memory(target_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query memory target: {error}"),
                repair: Some("ee doctor".to_string()),
            })?
            .ok_or_else(|| DomainError::NotFound {
                resource: "memory".to_string(),
                id: target_id.to_string(),
                repair: Some("ee memory list".to_string()),
            })?;
        return Ok(TargetResolution {
            workspace_id: memory.workspace_id,
            verified: true,
        });
    }

    let workspace_id = require_nonempty(
        "workspace id",
        workspace_id.unwrap_or_default(),
        "ee outcome <target-id> --workspace-id <workspace-id>",
    )?;
    let workspace =
        connection
            .get_workspace(&workspace_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query workspace: {error}"),
                repair: Some("ee doctor".to_string()),
            })?;
    if workspace.is_none() {
        return Err(DomainError::NotFound {
            resource: "workspace".to_string(),
            id: workspace_id,
            repair: Some("ee status --json".to_string()),
        });
    }

    Ok(TargetResolution {
        workspace_id,
        verified: false,
    })
}

fn current_feedback_summary(
    connection: &DbConnection,
    target_type: &str,
    target_id: &str,
) -> Result<OutcomeFeedbackSummary, DomainError> {
    connection
        .count_feedback_by_signal(target_type, target_id)
        .map(|counts| OutcomeFeedbackSummary::from_counts(&counts))
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to summarize feedback: {error}"),
            repair: Some("ee doctor".to_string()),
        })
}

fn get_existing_event(
    connection: &DbConnection,
    event_id: &str,
) -> Result<Option<StoredFeedbackEvent>, DomainError> {
    connection
        .get_feedback_event(event_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query feedback event: {error}"),
            repair: Some("ee doctor".to_string()),
        })
}

fn require_allowed(
    field: &str,
    raw: &str,
    allowed: &[&str],
    repair: &str,
) -> Result<String, DomainError> {
    let value = require_nonempty(field, raw, repair)?;
    if allowed.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(DomainError::Usage {
            message: format!(
                "invalid {field} '{value}'. Expected one of: {}",
                allowed.join(", ")
            ),
            repair: Some(repair.to_string()),
        })
    }
}

fn require_nonempty(field: &str, raw: &str, repair: &str) -> Result<String, DomainError> {
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

fn normalize_optional_text(field: &str, raw: Option<&str>) -> Result<Option<String>, DomainError> {
    raw.map(|value| require_nonempty(field, value, "ee outcome --help"))
        .transpose()
}

fn normalize_evidence_json(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    let Some(value) = raw else {
        return Ok(None);
    };
    let value = require_nonempty("evidence json", value, "ee outcome --evidence-json '{...}'")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&value).map_err(|error| DomainError::Usage {
            message: format!("evidence json must be valid JSON: {error}"),
            repair: Some(
                "ee outcome <target-id> --evidence-json '{\"outcome\":\"success\"}'".to_string(),
            ),
        })?;
    serde_json::to_string(&parsed)
        .map(Some)
        .map_err(|error| DomainError::Usage {
            message: format!("failed to canonicalize evidence json: {error}"),
            repair: Some(
                "ee outcome <target-id> --evidence-json '{\"outcome\":\"success\"}'".to_string(),
            ),
        })
}

fn default_feedback_weight(source_type: &str, signal: &str) -> f32 {
    (feedback_scoring::source_weight(source_type) * feedback_scoring::signal_multiplier(signal))
        .clamp(0.0, 10.0)
}

fn validate_weight(weight: f32) -> Result<f32, DomainError> {
    if weight.is_finite() && (0.0..=10.0).contains(&weight) {
        Ok(weight)
    } else {
        Err(DomainError::Usage {
            message: "weight must be a finite number between 0.0 and 10.0".to_string(),
            repair: Some("ee outcome <target-id> --weight 1.0".to_string()),
        })
    }
}

fn generate_feedback_event_id() -> String {
    let mut payload = uuid::Uuid::now_v7().simple().to_string();
    payload.truncate(26);
    format!("fb_{payload}")
}

fn validate_feedback_event_id(raw: &str) -> Result<String, DomainError> {
    let value = require_nonempty("event id", raw, "ee outcome --event-id fb_...")?;
    let payload = value
        .strip_prefix("fb_")
        .ok_or_else(|| DomainError::Usage {
            message: "event id must start with 'fb_'".to_string(),
            repair: Some("ee outcome --event-id fb_01234567890123456789012345".to_string()),
        })?;
    if value.len() == 29 && payload.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        Ok(value)
    } else {
        Err(DomainError::Usage {
            message: "event id must be 'fb_' followed by 26 ASCII alphanumeric characters"
                .to_string(),
            repair: Some("ee outcome --event-id fb_01234567890123456789012345".to_string()),
        })
    }
}

fn feedback_event_matches(
    existing: &StoredFeedbackEvent,
    input: &CreateFeedbackEventInput,
) -> bool {
    existing.workspace_id == input.workspace_id
        && existing.target_type == input.target_type
        && existing.target_id == input.target_id
        && existing.signal == input.signal
        && (existing.weight - input.weight).abs() <= f32::EPSILON
        && existing.source_type == input.source_type
        && existing.source_id == input.source_id
        && existing.reason == input.reason
        && existing.evidence_json == input.evidence_json
        && existing.session_id == input.session_id
}

fn outcome_audit_details(event_id: &str, input: &CreateFeedbackEventInput) -> String {
    serde_json::json!({
        "feedbackEventId": event_id,
        "targetType": &input.target_type,
        "targetId": &input.target_id,
        "signal": &input.signal,
        "weight": score_json_value(input.weight),
        "sourceType": &input.source_type,
        "sourceId": &input.source_id,
        "reasonPresent": input.reason.is_some(),
        "evidenceJsonPresent": input.evidence_json.is_some(),
        "sessionId": &input.session_id,
    })
    .to_string()
}

fn score_json_value(value: f32) -> serde_json::Value {
    let rounded = (f64::from(value) * 10_000.0).round() / 10_000.0;
    serde_json::Number::from_f64(rounded).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use asupersync::Outcome;
    use asupersync::types::{CancelKind, CancelReason, PanicPayload, RegionId, Time};

    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection, feedback_scoring};

    use super::{
        CliCancelReason, CliOutcomeClass, CliOutcomeSummary, EXIT_CANCELLED, EXIT_PANICKED,
        OutcomeRecordOptions, OutcomeRecordStatus, default_feedback_weight,
        generate_feedback_event_id, outcome_class, outcome_exit_code, record_outcome,
        validate_feedback_event_id,
    };
    use crate::models::{DomainError, ProcessExitCode};

    type TestResult = Result<(), String>;

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn test_cancel_reason(kind: CancelKind) -> CancelReason {
        CancelReason::with_origin(kind, RegionId::testing_default(), Time::ZERO)
    }

    const OUTCOME_TEST_WORKSPACE_ID: &str = "wsp_00000000000000000000000001";
    const OUTCOME_TEST_MEMORY_ID: &str = "mem_00000000000000000000000002";

    fn seed_outcome_database(
        prefix: &str,
    ) -> Result<(tempfile::TempDir, std::path::PathBuf), String> {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| error.to_string())?;
        let database = dir.path().join("ee.db");
        if let Some(parent) = database.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                OUTCOME_TEST_WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: dir.path().to_string_lossy().into_owned(),
                    name: Some("outcome-test".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                OUTCOME_TEST_MEMORY_ID,
                &CreateMemoryInput {
                    workspace_id: OUTCOME_TEST_WORKSPACE_ID.to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Run cargo fmt --check before release.".to_string(),
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: Some("file://AGENTS.md".to_string()),
                    trust_class: "human_explicit".to_string(),
                    trust_subclass: Some("project-rule".to_string()),
                    tags: vec!["cargo".to_string()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
        Ok((dir, database))
    }

    #[test]
    fn exit_code_constants_follow_conventions() -> TestResult {
        ensure_equal(&EXIT_CANCELLED, &130, "SIGINT convention")?;
        ensure_equal(&EXIT_PANICKED, &101, "Rust panic convention")
    }

    #[test]
    fn outcome_class_strings_are_stable() -> TestResult {
        ensure_equal(&CliOutcomeClass::Success.as_str(), &"success", "success")?;
        ensure_equal(
            &CliOutcomeClass::DomainError.as_str(),
            &"domain_error",
            "domain_error",
        )?;
        ensure_equal(
            &CliOutcomeClass::Cancelled.as_str(),
            &"cancelled",
            "cancelled",
        )?;
        ensure_equal(&CliOutcomeClass::Panicked.as_str(), &"panicked", "panicked")
    }

    #[test]
    fn cancel_reason_strings_are_stable() -> TestResult {
        ensure_equal(
            &CliCancelReason::BudgetExhausted.as_str(),
            &"budget_exhausted",
            "budget",
        )?;
        ensure_equal(
            &CliCancelReason::UserRequested.as_str(),
            &"user_requested",
            "user",
        )?;
        ensure_equal(&CliCancelReason::Timeout.as_str(), &"timeout", "timeout")?;
        ensure_equal(
            &CliCancelReason::ParentCancelled.as_str(),
            &"parent_cancelled",
            "parent",
        )?;
        ensure_equal(&CliCancelReason::Shutdown.as_str(), &"shutdown", "shutdown")?;
        ensure_equal(&CliCancelReason::Other.as_str(), &"other", "other")
    }

    #[test]
    fn outcome_ok_maps_to_exit_zero() -> TestResult {
        let outcome: Outcome<(), DomainError> = Outcome::ok(());
        ensure_equal(&outcome_exit_code(&outcome), &0, "ok exit code")?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::Success,
            "ok class",
        )
    }

    #[test]
    fn outcome_err_maps_to_domain_exit_code() -> TestResult {
        let error = DomainError::Usage {
            message: "test".to_string(),
            repair: None,
        };
        let outcome: Outcome<(), DomainError> = Outcome::err(error);
        ensure_equal(
            &outcome_exit_code(&outcome),
            &(ProcessExitCode::Usage as u8),
            "usage exit code",
        )?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::DomainError,
            "err class",
        )
    }

    #[test]
    fn outcome_cancelled_maps_to_130() -> TestResult {
        let reason = test_cancel_reason(CancelKind::User);
        let outcome: Outcome<(), DomainError> = Outcome::Cancelled(reason);
        ensure_equal(&outcome_exit_code(&outcome), &130, "cancelled exit code")?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::Cancelled,
            "cancelled class",
        )
    }

    #[test]
    fn outcome_panicked_maps_to_101() -> TestResult {
        let payload = PanicPayload::new("test panic");
        let outcome: Outcome<(), DomainError> = Outcome::Panicked(payload);
        ensure_equal(&outcome_exit_code(&outcome), &101, "panicked exit code")?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::Panicked,
            "panicked class",
        )
    }

    #[test]
    fn cli_outcome_summary_from_ok() -> TestResult {
        let outcome: Outcome<i32, DomainError> = Outcome::ok(42);
        let summary = CliOutcomeSummary::from_outcome(&outcome);
        ensure_equal(&summary.class, &CliOutcomeClass::Success, "class")?;
        ensure_equal(&summary.exit_code, &0, "exit code")?;
        ensure_equal(&summary.is_success(), &true, "is_success")
    }

    #[test]
    fn cli_outcome_summary_from_err() -> TestResult {
        let error = DomainError::Storage {
            message: "Database locked".to_string(),
            repair: Some("ee db unlock".to_string()),
        };
        let outcome: Outcome<(), DomainError> = Outcome::err(error);
        let summary = CliOutcomeSummary::from_outcome(&outcome);
        ensure_equal(&summary.class, &CliOutcomeClass::DomainError, "class")?;
        ensure_equal(
            &summary.exit_code,
            &(ProcessExitCode::Storage as u8),
            "exit code",
        )?;
        ensure_equal(
            &summary.message,
            &Some("Database locked".to_string()),
            "message",
        )?;
        ensure_equal(&summary.is_success(), &false, "is_success")
    }

    #[test]
    fn cli_outcome_summary_from_cancelled() -> TestResult {
        let reason = test_cancel_reason(CancelKind::PollQuota);
        let outcome: Outcome<(), DomainError> = Outcome::Cancelled(reason);
        let summary = CliOutcomeSummary::from_outcome(&outcome);
        ensure_equal(&summary.class, &CliOutcomeClass::Cancelled, "class")?;
        ensure_equal(&summary.exit_code, &130, "exit code")?;
        ensure_equal(
            &summary.cancel_reason,
            &Some(CliCancelReason::BudgetExhausted),
            "cancel reason",
        )?;
        ensure_equal(&summary.is_success(), &false, "is_success")
    }

    #[test]
    fn outcome_class_is_terminal_classification() -> TestResult {
        ensure_equal(&CliOutcomeClass::Success.is_terminal(), &false, "success")?;
        ensure_equal(
            &CliOutcomeClass::DomainError.is_terminal(),
            &true,
            "domain error",
        )?;
        ensure_equal(
            &CliOutcomeClass::Cancelled.is_terminal(),
            &true,
            "cancelled",
        )?;
        ensure_equal(&CliOutcomeClass::Panicked.is_terminal(), &true, "panicked")
    }

    #[test]
    fn cancel_kind_to_cli_reason_mapping() -> TestResult {
        let cases = [
            (CancelKind::PollQuota, CliCancelReason::BudgetExhausted),
            (CancelKind::CostBudget, CliCancelReason::BudgetExhausted),
            (CancelKind::Deadline, CliCancelReason::BudgetExhausted),
            (CancelKind::User, CliCancelReason::UserRequested),
            (CancelKind::Timeout, CliCancelReason::Timeout),
            (
                CancelKind::ParentCancelled,
                CliCancelReason::ParentCancelled,
            ),
            (CancelKind::Shutdown, CliCancelReason::Shutdown),
            (CancelKind::FailFast, CliCancelReason::Other),
            (CancelKind::RaceLost, CliCancelReason::Other),
        ];

        for (kind, expected) in cases {
            let reason = test_cancel_reason(kind);
            let cli_reason = CliCancelReason::from(&reason);
            ensure_equal(&cli_reason, &expected, &format!("{kind:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn feedback_event_id_generation_matches_storage_contract() -> TestResult {
        let id = generate_feedback_event_id();
        ensure_equal(&id.len(), &29, "feedback id length")?;
        ensure_equal(&id.starts_with("fb_"), &true, "feedback id prefix")?;
        ensure_equal(
            &validate_feedback_event_id(&id).map_err(|error| error.message())?,
            &id,
            "feedback id validates",
        )
    }

    #[test]
    fn default_feedback_weight_uses_source_and_signal_scoring() -> TestResult {
        ensure_equal(
            &default_feedback_weight("outcome_observed", "helpful"),
            &1.2_f32,
            "outcome helpful weight",
        )?;
        ensure_equal(
            &default_feedback_weight("outcome_observed", "harmful"),
            &(feedback_scoring::WEIGHT_OUTCOME_OBSERVED * feedback_scoring::HARMFUL_MULTIPLIER),
            "outcome harmful weight",
        )
    }

    #[test]
    fn record_outcome_dry_run_does_not_mutate_feedback_events() -> TestResult {
        let (_dir, database) = seed_outcome_database("ee-outcome-dry-run")?;
        let report = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "helpful".to_string(),
            weight: None,
            source_type: "outcome_observed".to_string(),
            source_id: Some("test-run".to_string()),
            reason: Some("Task succeeded after using this rule.".to_string()),
            evidence_json: Some(r#"{"outcome":"success"}"#.to_string()),
            session_id: None,
            event_id: Some("fb_01234567890123456789012345".to_string()),
            actor: Some("test".to_string()),
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        ensure_equal(
            &report.status,
            &OutcomeRecordStatus::DryRun,
            "dry run status",
        )?;
        ensure_equal(&report.feedback.total_count, &0, "no feedback recorded")?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let events = connection
            .list_feedback_events_for_target("memory", OUTCOME_TEST_MEMORY_ID)
            .map_err(|error| error.to_string())?;
        ensure_equal(&events.len(), &0_usize, "event table remains empty")
    }

    #[test]
    fn record_outcome_persists_feedback_and_audit() -> TestResult {
        let (_dir, database) = seed_outcome_database("ee-outcome-record")?;
        let report = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "helpful".to_string(),
            weight: Some(2.0),
            source_type: "human_explicit".to_string(),
            source_id: Some("operator-note".to_string()),
            reason: Some("The memory directly avoided a release mistake.".to_string()),
            evidence_json: Some(r#"{"outcome":"success","redacted":true}"#.to_string()),
            session_id: Some("sess_00000000000000000000000001".to_string()),
            event_id: Some("fb_11234567890123456789012345".to_string()),
            actor: Some("test".to_string()),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(
            &report.status,
            &OutcomeRecordStatus::Recorded,
            "recorded status",
        )?;
        ensure_equal(&report.feedback.total_count, &1, "feedback count")?;
        ensure_equal(
            &report.evidence_json_present,
            &true,
            "evidence presence only",
        )?;
        ensure_equal(&report.audit_id.is_some(), &true, "audit id present")?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let audit = connection
            .list_audit_by_target("memory", OUTCOME_TEST_MEMORY_ID, None)
            .map_err(|error| error.to_string())?;
        ensure_equal(&audit.len(), &1_usize, "audit row count")?;
        ensure_equal(
            &audit[0].action,
            &crate::db::audit_actions::FEEDBACK_RECORD.to_string(),
            "audit action",
        )
    }

    #[test]
    fn record_outcome_event_id_is_idempotent_for_same_content() -> TestResult {
        let (_dir, database) = seed_outcome_database("ee-outcome-idempotent")?;
        let options = OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "helpful".to_string(),
            weight: Some(1.0),
            source_type: "outcome_observed".to_string(),
            source_id: Some("run-1".to_string()),
            reason: Some("Succeeded.".to_string()),
            evidence_json: Some(r#"{"outcome":"success"}"#.to_string()),
            session_id: None,
            event_id: Some("fb_21234567890123456789012345".to_string()),
            actor: Some("test".to_string()),
            dry_run: false,
        };

        let first = record_outcome(&options).map_err(|error| error.message())?;
        let second = record_outcome(&options).map_err(|error| error.message())?;

        ensure_equal(
            &first.status,
            &OutcomeRecordStatus::Recorded,
            "first status",
        )?;
        ensure_equal(
            &second.status,
            &OutcomeRecordStatus::AlreadyRecorded,
            "second status",
        )?;
        ensure_equal(&second.feedback.total_count, &1, "deduped count")
    }

    #[test]
    fn record_outcome_rejects_invalid_evidence_json() -> TestResult {
        let (_dir, database) = seed_outcome_database("ee-outcome-invalid-json")?;
        let result = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "helpful".to_string(),
            weight: None,
            source_type: "outcome_observed".to_string(),
            source_id: None,
            reason: None,
            evidence_json: Some("{invalid".to_string()),
            session_id: None,
            event_id: None,
            actor: None,
            dry_run: false,
        });

        match result {
            Err(DomainError::Usage { message, .. }) => ensure_equal(
                &message.starts_with("evidence json must be valid JSON"),
                &true,
                "usage error message",
            ),
            other => Err(format!("expected usage error, got {other:?}")),
        }
    }
}
