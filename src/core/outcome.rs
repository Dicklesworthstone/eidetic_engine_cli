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
use chrono::{Duration, Utc};
use serde::Serialize;

use crate::db::{
    AuditedFeedbackEventInput, CreateAuditInput, CreateFeedbackEventInput,
    CreateFeedbackQuarantineInput, DbConnection, FeedbackCounts, StoredFeedbackEvent,
    StoredFeedbackQuarantine, audit_actions, feedback_scoring, generate_audit_id,
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
const HARMFUL_SIGNALS: &[&str] = &["negative", "contradiction", "harmful", "inaccurate"];

/// Default harmful-feedback burst ceiling per source.
pub const DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR: u32 = 5;
/// Default harmful-feedback burst window in seconds.
pub const DEFAULT_HARMFUL_BURST_WINDOW_SECONDS: u32 = 3600;

/// Stable schema for `ee outcome quarantine list` response data.
pub const OUTCOME_QUARANTINE_LIST_SCHEMA_V1: &str = "ee.outcome.quarantine.list.v1";
/// Stable schema for `ee outcome quarantine release/reject` response data.
pub const OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1: &str = "ee.outcome.quarantine.review.v1";

/// Status returned by the `ee outcome` feedback recording use case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutcomeRecordStatus {
    /// The feedback event was persisted and audited.
    Recorded,
    /// The command validated inputs but did not mutate storage.
    DryRun,
    /// A caller-supplied event ID already existed with matching content.
    AlreadyRecorded,
    /// The event was preserved in quarantine and did not affect live scoring.
    Quarantined,
}

impl OutcomeRecordStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::DryRun => "dry_run",
            Self::AlreadyRecorded => "already_recorded",
            Self::Quarantined => "feedback_quarantined",
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
    pub harmful_per_source_per_hour: u32,
    pub harmful_burst_window_seconds: u32,
}

/// Options for listing quarantined feedback events.
#[derive(Clone, Debug)]
pub struct OutcomeQuarantineListOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub status: Option<&'a str>,
}

/// Options for releasing or rejecting one quarantined feedback event.
#[derive(Clone, Debug)]
pub struct OutcomeQuarantineReviewOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub quarantine_id: &'a str,
    pub reject: bool,
    pub actor: Option<&'a str>,
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

/// Quarantine metadata exposed by outcome commands.
#[derive(Clone, Debug, PartialEq)]
pub struct OutcomeQuarantineSummary {
    pub id: Option<String>,
    pub status: String,
    pub source_id: Option<String>,
    pub limit: u32,
    pub window_seconds: u32,
    pub observed_count: u32,
    pub reason: String,
    pub raw_event_hash: Option<String>,
}

impl OutcomeQuarantineSummary {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": &self.id,
            "status": &self.status,
            "sourceId": &self.source_id,
            "limit": self.limit,
            "windowSeconds": self.window_seconds,
            "observedCount": self.observed_count,
            "reason": &self.reason,
            "rawEventHash": &self.raw_event_hash,
        })
    }
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

/// Stable quarantine row exposed by `ee outcome quarantine list`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeQuarantineRecord {
    pub id: String,
    pub workspace_id: String,
    pub source_id: String,
    pub target_type: String,
    pub target_id: String,
    pub signal: String,
    pub event_weight: f32,
    pub event_source_type: String,
    pub proposed_event_id: Option<String>,
    pub recorded_at: String,
    pub reason: String,
    pub event_reason_present: bool,
    pub event_evidence_json_present: bool,
    pub event_session_id: Option<String>,
    pub raw_event_hash: String,
    pub status: String,
    pub reviewed_at: Option<String>,
    pub reviewed_by: Option<String>,
    pub released_feedback_event_id: Option<String>,
}

/// Result of listing quarantined feedback.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeQuarantineListReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub status_filter: Option<String>,
    pub queue_depth: usize,
    pub records: Vec<OutcomeQuarantineRecord>,
}

impl OutcomeQuarantineListReport {
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"outcome quarantine list","status":"serialization_failed"}}"#,
                OUTCOME_QUARANTINE_LIST_SCHEMA_V1
            )
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Feedback quarantine ({} records)\n", self.queue_depth);
        for record in &self.records {
            output.push_str(&format!(
                "  {} [{}] {} {} from {}\n",
                record.id, record.status, record.target_type, record.target_id, record.source_id
            ));
        }
        output
    }
}

/// Result of releasing or rejecting quarantined feedback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeQuarantineReviewReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub status: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub quarantine_id: String,
    pub action: String,
    pub changed: bool,
    pub dry_run: bool,
    pub feedback_event_id: Option<String>,
    pub audit_id: Option<String>,
}

impl OutcomeQuarantineReviewReport {
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"schema":"{}","command":"outcome quarantine review","status":"serialization_failed"}}"#,
                OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1
            )
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "Feedback quarantine {}\n  ID: {}\n  Changed: {}\n  Audit: {}\n",
            self.action,
            self.quarantine_id,
            self.changed,
            self.audit_id.as_deref().unwrap_or("none")
        )
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
    pub quarantine: Option<OutcomeQuarantineSummary>,
    pub feedback: OutcomeFeedbackSummary,
}

impl OutcomeRecordReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let action = match self.status {
            OutcomeRecordStatus::Recorded => "Recorded outcome feedback",
            OutcomeRecordStatus::DryRun => "DRY RUN: Would record outcome feedback",
            OutcomeRecordStatus::AlreadyRecorded => "Outcome feedback already recorded",
            OutcomeRecordStatus::Quarantined => {
                "Outcome feedback quarantined; live scoring was not changed"
            }
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
        if let Some(ref quarantine) = self.quarantine
            && let Some(ref quarantine_id) = quarantine.id
        {
            output.push_str(&format!("  Quarantine: {quarantine_id}\n"));
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
            "quarantine": self.quarantine.as_ref().map(OutcomeQuarantineSummary::data_json),
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
    let mut source_id = normalize_optional_text("source id", options.source_id.as_deref())?;
    let reason = normalize_optional_text("reason", options.reason.as_deref())?;
    let evidence_json = normalize_evidence_json(options.evidence_json.as_deref())?;
    let session_id = normalize_optional_text("session id", options.session_id.as_deref())?;
    validate_harmful_feedback_policy(
        options.harmful_per_source_per_hour,
        options.harmful_burst_window_seconds,
    )?;
    if source_id.is_none() && is_harmful_signal(&signal) {
        source_id = Some(fallback_source_id(
            &source_type,
            session_id.as_deref(),
            options.actor.as_deref(),
        ));
    }
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
        let quarantine = harmful_quarantine_preview(
            &connection,
            &target.workspace_id,
            &signal,
            source_id.as_deref(),
            options.harmful_per_source_per_hour,
            options.harmful_burst_window_seconds,
        )?;
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
            quarantine,
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
                quarantine: None,
                feedback,
            });
        }

        return Err(DomainError::Usage {
            message: format!("feedback event id already exists with different content: {event_id}"),
            repair: Some("ee outcome --event-id <new-feedback-id>".to_string()),
        });
    }

    if let Some(quarantine) = harmful_quarantine_preview(
        &connection,
        &target.workspace_id,
        &signal,
        source_id.as_deref(),
        options.harmful_per_source_per_hour,
        options.harmful_burst_window_seconds,
    )? {
        let quarantine_id = generate_feedback_quarantine_id();
        let raw_event_hash = raw_feedback_event_hash(&event_id, &feedback_input)?;
        let reason = quarantine.reason.clone();
        let audit_id = insert_feedback_quarantine_audited(
            &connection,
            &quarantine_id,
            &CreateFeedbackQuarantineInput {
                workspace_id: target.workspace_id.clone(),
                source_id: source_id.clone().unwrap_or_else(|| "unknown".to_owned()),
                target_type: target_type.clone(),
                target_id: target_id.clone(),
                signal: signal.clone(),
                weight,
                source_type: source_type.clone(),
                proposed_event_id: Some(event_id.clone()),
                recorded_at: Utc::now().to_rfc3339(),
                reason,
                event_reason: feedback_input.reason.clone(),
                evidence_json: feedback_input.evidence_json.clone(),
                session_id: feedback_input.session_id.clone(),
                raw_event_hash: raw_event_hash.clone(),
            },
            options.actor.as_deref(),
        )?;
        let feedback = current_feedback_summary(&connection, &target_type, &target_id)?;
        return Ok(OutcomeRecordReport {
            version: env!("CARGO_PKG_VERSION"),
            status: OutcomeRecordStatus::Quarantined,
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
            quarantine: Some(OutcomeQuarantineSummary {
                id: Some(quarantine_id),
                raw_event_hash: Some(raw_event_hash),
                ..quarantine
            }),
            feedback,
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
        quarantine: None,
        feedback,
    })
}

/// List quarantined feedback events for a workspace.
pub fn list_feedback_quarantine(
    options: &OutcomeQuarantineListOptions<'_>,
) -> Result<OutcomeQuarantineListReport, DomainError> {
    let prepared = prepare_quarantine_workspace(options.workspace_path, options.database_path)?;
    let status = normalize_quarantine_status(options.status)?;
    let connection = open_existing_database(&prepared.database_path)?;
    let rows = connection
        .list_feedback_quarantine(&prepared.workspace_id, status.as_deref())
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list feedback quarantine: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let records = rows
        .into_iter()
        .map(outcome_quarantine_record_from_row)
        .collect::<Vec<_>>();
    Ok(OutcomeQuarantineListReport {
        schema: OUTCOME_QUARANTINE_LIST_SCHEMA_V1,
        command: "outcome quarantine list",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        status_filter: status,
        queue_depth: records.len(),
        records,
    })
}

/// Release or reject one quarantined feedback event without deleting evidence.
pub fn review_feedback_quarantine(
    options: &OutcomeQuarantineReviewOptions<'_>,
) -> Result<OutcomeQuarantineReviewReport, DomainError> {
    let prepared = prepare_quarantine_workspace(options.workspace_path, options.database_path)?;
    let quarantine_id = validate_feedback_quarantine_id(options.quarantine_id)?;
    let connection = open_existing_database(&prepared.database_path)?;
    let Some(row) = connection
        .get_feedback_quarantine(&quarantine_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query feedback quarantine: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
    else {
        return Err(feedback_quarantine_not_found(&quarantine_id));
    };
    if row.workspace_id != prepared.workspace_id {
        return Err(feedback_quarantine_not_found(&quarantine_id));
    }

    let action = if options.reject { "reject" } else { "release" };
    if row.status != "pending" {
        return Ok(outcome_quarantine_review_report(
            &prepared,
            &quarantine_id,
            QuarantineReviewSummary {
                action,
                status: "already_reviewed",
                changed: false,
                dry_run: options.dry_run,
                feedback_event_id: row.released_feedback_event_id,
                audit_id: None,
            },
        ));
    }
    if options.dry_run {
        return Ok(outcome_quarantine_review_report(
            &prepared,
            &quarantine_id,
            QuarantineReviewSummary {
                action,
                status: "dry_run",
                changed: true,
                dry_run: true,
                feedback_event_id: row.proposed_event_id,
                audit_id: None,
            },
        ));
    }

    if options.reject {
        let audit_id = update_feedback_quarantine_review_audited(
            &connection,
            &row,
            "rejected",
            options.actor,
            None,
        )?;
        return Ok(outcome_quarantine_review_report(
            &prepared,
            &quarantine_id,
            QuarantineReviewSummary {
                action,
                status: "rejected",
                changed: true,
                dry_run: false,
                feedback_event_id: None,
                audit_id: Some(audit_id),
            },
        ));
    }

    let event_id = row
        .proposed_event_id
        .clone()
        .unwrap_or_else(generate_feedback_event_id);
    let feedback_input = CreateFeedbackEventInput {
        workspace_id: row.workspace_id.clone(),
        target_type: row.target_type.clone(),
        target_id: row.target_id.clone(),
        signal: row.signal.clone(),
        weight: row.weight,
        source_type: row.source_type.clone(),
        source_id: Some(row.source_id.clone()),
        reason: row.event_reason.clone(),
        evidence_json: row.evidence_json.clone(),
        session_id: row.session_id.clone(),
    };
    let expected_hash = raw_feedback_event_hash(&event_id, &feedback_input)?;
    if expected_hash != row.raw_event_hash {
        return Err(DomainError::PolicyDenied {
            message: format!("quarantined feedback payload hash mismatch for {}", row.id),
            repair: Some(format!("ee outcome quarantine release {} --reject", row.id)),
        });
    }
    let audit_id = release_feedback_quarantine_audited(
        &connection,
        &row,
        &event_id,
        &feedback_input,
        options.actor,
    )?;
    Ok(outcome_quarantine_review_report(
        &prepared,
        &quarantine_id,
        QuarantineReviewSummary {
            action,
            status: "released",
            changed: true,
            dry_run: false,
            feedback_event_id: Some(event_id),
            audit_id: Some(audit_id),
        },
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TargetResolution {
    workspace_id: String,
    verified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedQuarantineWorkspace {
    workspace_id: String,
    workspace_path: std::path::PathBuf,
    database_path: std::path::PathBuf,
}

fn prepare_quarantine_workspace(
    workspace_path: &Path,
    database_path: Option<&Path>,
) -> Result<PreparedQuarantineWorkspace, DomainError> {
    let workspace_path = resolve_workspace_path(workspace_path)?;
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    Ok(PreparedQuarantineWorkspace {
        workspace_id: super::curate::stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
    })
}

fn resolve_workspace_path(path: &Path) -> Result<std::path::PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(path)
    };
    absolute
        .canonicalize()
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        })
}

fn open_existing_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_owned()),
        });
    }
    DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
        message: format!("Failed to open database: {error}"),
        repair: Some("ee doctor".to_owned()),
    })
}

fn normalize_quarantine_status(raw: Option<&str>) -> Result<Option<String>, DomainError> {
    let Some(raw) = raw else {
        return Ok(Some("pending".to_owned()));
    };
    let value = raw.trim();
    if value.is_empty() {
        return Ok(Some("pending".to_owned()));
    }
    if matches!(value, "pending" | "released" | "rejected" | "all") {
        Ok((value != "all").then(|| value.to_owned()))
    } else {
        Err(DomainError::Usage {
            message: format!("invalid quarantine status '{value}'"),
            repair: Some("ee outcome quarantine list --status pending".to_owned()),
        })
    }
}

fn outcome_quarantine_record_from_row(row: StoredFeedbackQuarantine) -> OutcomeQuarantineRecord {
    OutcomeQuarantineRecord {
        id: row.id,
        workspace_id: row.workspace_id,
        source_id: row.source_id,
        target_type: row.target_type,
        target_id: row.target_id,
        signal: row.signal,
        event_weight: row.weight,
        event_source_type: row.source_type,
        proposed_event_id: row.proposed_event_id,
        recorded_at: row.recorded_at,
        reason: row.reason,
        event_reason_present: row.event_reason.is_some(),
        event_evidence_json_present: row.evidence_json.is_some(),
        event_session_id: row.session_id,
        raw_event_hash: row.raw_event_hash,
        status: row.status,
        reviewed_at: row.reviewed_at,
        reviewed_by: row.reviewed_by,
        released_feedback_event_id: row.released_feedback_event_id,
    }
}

#[derive(Clone, Debug)]
struct QuarantineReviewSummary<'a> {
    action: &'a str,
    status: &'a str,
    changed: bool,
    dry_run: bool,
    feedback_event_id: Option<String>,
    audit_id: Option<String>,
}

fn outcome_quarantine_review_report(
    prepared: &PreparedQuarantineWorkspace,
    quarantine_id: &str,
    summary: QuarantineReviewSummary<'_>,
) -> OutcomeQuarantineReviewReport {
    OutcomeQuarantineReviewReport {
        schema: OUTCOME_QUARANTINE_REVIEW_SCHEMA_V1,
        command: "outcome quarantine review",
        version: env!("CARGO_PKG_VERSION"),
        status: summary.status.to_owned(),
        workspace_id: prepared.workspace_id.clone(),
        workspace_path: prepared.workspace_path.display().to_string(),
        database_path: prepared.database_path.display().to_string(),
        quarantine_id: quarantine_id.to_owned(),
        action: summary.action.to_owned(),
        changed: summary.changed,
        dry_run: summary.dry_run,
        feedback_event_id: summary.feedback_event_id,
        audit_id: summary.audit_id,
    }
}

fn validate_feedback_quarantine_id(raw: &str) -> Result<String, DomainError> {
    let value = require_nonempty(
        "feedback quarantine id",
        raw,
        "ee outcome quarantine release fq_...",
    )?;
    let payload = value
        .strip_prefix("fq_")
        .ok_or_else(|| DomainError::Usage {
            message: "feedback quarantine id must start with 'fq_'".to_owned(),
            repair: Some("ee outcome quarantine list --json".to_owned()),
        })?;
    if value.len() == 29 && payload.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        Ok(value)
    } else {
        Err(DomainError::Usage {
            message:
                "feedback quarantine id must be 'fq_' followed by 26 ASCII alphanumeric characters"
                    .to_owned(),
            repair: Some("ee outcome quarantine list --json".to_owned()),
        })
    }
}

fn feedback_quarantine_not_found(quarantine_id: &str) -> DomainError {
    DomainError::NotFound {
        resource: "feedback quarantine".to_owned(),
        id: quarantine_id.to_owned(),
        repair: Some("ee outcome quarantine list --json".to_owned()),
    }
}

fn update_feedback_quarantine_review_audited(
    connection: &DbConnection,
    row: &StoredFeedbackQuarantine,
    status: &str,
    actor: Option<&str>,
    released_feedback_event_id: Option<&str>,
) -> Result<String, DomainError> {
    let audit_id = generate_audit_id();
    let details = feedback_quarantine_review_audit_details(row, status, released_feedback_event_id);
    connection
        .with_transaction(|| {
            connection.update_feedback_quarantine_status(
                &row.id,
                status,
                actor,
                released_feedback_event_id,
            )?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(row.workspace_id.clone()),
                    actor: actor
                        .map(str::to_owned)
                        .or_else(|| Some("ee outcome quarantine".to_owned())),
                    action: if status == "released" {
                        audit_actions::FEEDBACK_QUARANTINE_RELEASE.to_owned()
                    } else {
                        audit_actions::FEEDBACK_QUARANTINE_REJECT.to_owned()
                    },
                    target_type: Some("feedback_quarantine".to_owned()),
                    target_id: Some(row.id.clone()),
                    details: Some(details.clone()),
                },
            )
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to review feedback quarantine: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn release_feedback_quarantine_audited(
    connection: &DbConnection,
    row: &StoredFeedbackQuarantine,
    event_id: &str,
    feedback_input: &CreateFeedbackEventInput,
    actor: Option<&str>,
) -> Result<String, DomainError> {
    let audit_id = generate_audit_id();
    let details = feedback_quarantine_review_audit_details(row, "released", Some(event_id));
    connection
        .with_transaction(|| {
            connection.insert_feedback_event(event_id, feedback_input)?;
            connection.update_feedback_quarantine_status(
                &row.id,
                "released",
                actor,
                Some(event_id),
            )?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(row.workspace_id.clone()),
                    actor: actor
                        .map(str::to_owned)
                        .or_else(|| Some("ee outcome quarantine".to_owned())),
                    action: audit_actions::FEEDBACK_QUARANTINE_RELEASE.to_owned(),
                    target_type: Some("feedback_quarantine".to_owned()),
                    target_id: Some(row.id.clone()),
                    details: Some(details.clone()),
                },
            )
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to release feedback quarantine: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn feedback_quarantine_review_audit_details(
    row: &StoredFeedbackQuarantine,
    status: &str,
    released_feedback_event_id: Option<&str>,
) -> String {
    serde_json::json!({
        "feedbackQuarantineId": &row.id,
        "status": status,
        "targetType": &row.target_type,
        "targetId": &row.target_id,
        "sourceId": &row.source_id,
        "eventWeight": score_json_value(row.weight),
        "eventSourceType": &row.source_type,
        "eventReasonPresent": row.event_reason.is_some(),
        "eventEvidenceJsonPresent": row.evidence_json.is_some(),
        "eventSessionId": &row.session_id,
        "rawEventHash": &row.raw_event_hash,
        "releasedFeedbackEventId": released_feedback_event_id,
    })
    .to_string()
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

fn harmful_quarantine_preview(
    connection: &DbConnection,
    workspace_id: &str,
    signal: &str,
    source_id: Option<&str>,
    limit: u32,
    window_seconds: u32,
) -> Result<Option<OutcomeQuarantineSummary>, DomainError> {
    if !is_harmful_signal(signal) {
        return Ok(None);
    }
    let Some(source_id) = source_id else {
        return Ok(None);
    };
    let since = Utc::now()
        .checked_sub_signed(Duration::seconds(i64::from(window_seconds)))
        .unwrap_or_else(Utc::now)
        .to_rfc3339();
    let live_count = connection
        .count_harmful_feedback_for_source_since(workspace_id, source_id, &since)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to inspect harmful feedback rate state: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let pending_count = connection
        .count_pending_quarantine_for_source_since(workspace_id, source_id, &since)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to inspect feedback quarantine queue: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let existing_count = live_count.saturating_add(pending_count);
    if existing_count < limit {
        return Ok(None);
    }
    let observed_count = existing_count.saturating_add(1);
    Ok(Some(OutcomeQuarantineSummary {
        id: None,
        status: "pending".to_owned(),
        source_id: Some(source_id.to_owned()),
        limit,
        window_seconds,
        observed_count,
        reason: format!(
            "harmful feedback rate limit exceeded: source {source_id} observed {observed_count} harmful events in {window_seconds}s (limit {limit})"
        ),
        raw_event_hash: None,
    }))
}

fn insert_feedback_quarantine_audited(
    connection: &DbConnection,
    quarantine_id: &str,
    input: &CreateFeedbackQuarantineInput,
    actor: Option<&str>,
) -> Result<String, DomainError> {
    let audit_id = generate_audit_id();
    let details = feedback_quarantine_audit_details(quarantine_id, input);
    connection
        .with_transaction(|| {
            connection.insert_feedback_quarantine(quarantine_id, input)?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(input.workspace_id.clone()),
                    actor: actor
                        .map(str::to_owned)
                        .or_else(|| Some("ee outcome".to_owned())),
                    action: audit_actions::FEEDBACK_QUARANTINE.to_owned(),
                    target_type: Some(input.target_type.clone()),
                    target_id: Some(input.target_id.clone()),
                    details: Some(details.clone()),
                },
            )
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to quarantine feedback event: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    Ok(audit_id)
}

fn validate_harmful_feedback_policy(limit: u32, window_seconds: u32) -> Result<(), DomainError> {
    if limit == 0 {
        return Err(DomainError::Usage {
            message: "harmful feedback rate limit must be greater than zero".to_owned(),
            repair: Some("ee outcome <target-id> --harmful-per-source-per-hour 5".to_owned()),
        });
    }
    if window_seconds == 0 {
        return Err(DomainError::Usage {
            message: "harmful feedback burst window must be greater than zero seconds".to_owned(),
            repair: Some("ee outcome <target-id> --harmful-burst-window-seconds 3600".to_owned()),
        });
    }
    Ok(())
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

fn is_harmful_signal(signal: &str) -> bool {
    HARMFUL_SIGNALS.contains(&signal)
}

fn fallback_source_id(source_type: &str, session_id: Option<&str>, actor: Option<&str>) -> String {
    if let Some(session_id) = session_id {
        return format!("session:{session_id}");
    }
    let actor = actor.map(str::trim).filter(|value| !value.is_empty());
    if let Some(actor) = actor {
        return format!("actor:{}", stable_short_hash(actor));
    }
    format!("source-type:{source_type}")
}

fn generate_feedback_event_id() -> String {
    let mut payload = uuid::Uuid::now_v7().simple().to_string();
    payload.truncate(26);
    format!("fb_{payload}")
}

fn generate_feedback_quarantine_id() -> String {
    let mut payload = uuid::Uuid::now_v7().simple().to_string();
    payload.truncate(26);
    format!("fq_{payload}")
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

fn feedback_quarantine_audit_details(
    quarantine_id: &str,
    input: &CreateFeedbackQuarantineInput,
) -> String {
    serde_json::json!({
        "feedbackQuarantineId": quarantine_id,
        "proposedFeedbackEventId": &input.proposed_event_id,
        "targetType": &input.target_type,
        "targetId": &input.target_id,
        "signal": &input.signal,
        "sourceId": &input.source_id,
        "eventWeight": score_json_value(input.weight),
        "eventSourceType": &input.source_type,
        "eventReasonPresent": input.event_reason.is_some(),
        "eventEvidenceJsonPresent": input.evidence_json.is_some(),
        "eventSessionId": &input.session_id,
        "recordedAt": &input.recorded_at,
        "reason": &input.reason,
        "rawEventHash": &input.raw_event_hash,
    })
    .to_string()
}

fn raw_feedback_event_hash(
    event_id: &str,
    input: &CreateFeedbackEventInput,
) -> Result<String, DomainError> {
    let payload = serde_json::json!({
        "eventId": event_id,
        "workspaceId": &input.workspace_id,
        "targetType": &input.target_type,
        "targetId": &input.target_id,
        "signal": &input.signal,
        "weight": score_json_value(input.weight),
        "sourceType": &input.source_type,
        "sourceId": &input.source_id,
        "reason": &input.reason,
        "evidenceJson": &input.evidence_json,
        "sessionId": &input.session_id,
    });
    serde_json::to_string(&payload)
        .map(|canonical| format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex()))
        .map_err(|error| DomainError::Usage {
            message: format!(
                "failed to canonicalize feedback event for quarantine hashing: {error}"
            ),
            repair: Some("ee outcome <target-id> --signal harmful".to_owned()),
        })
}

fn stable_short_hash(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..16].to_owned()
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
        CliCancelReason, CliOutcomeClass, CliOutcomeSummary, DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR, EXIT_CANCELLED, EXIT_PANICKED, OutcomeRecordOptions,
        OutcomeRecordStatus, default_feedback_weight, generate_feedback_event_id, outcome_class,
        outcome_exit_code, record_outcome, validate_feedback_event_id,
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

    fn ensure(condition: bool, context: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(context.to_string())
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
        seed_outcome_database_with_workspace_id(prefix, Some(OUTCOME_TEST_WORKSPACE_ID.to_string()))
    }

    fn seed_outcome_database_with_workspace_id(
        prefix: &str,
        workspace_id: Option<String>,
    ) -> Result<(tempfile::TempDir, std::path::PathBuf), String> {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| error.to_string())?;
        let workspace_path = dir
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace_id = workspace_id
            .unwrap_or_else(|| crate::core::curate::stable_workspace_id(&workspace_path));
        let database = dir.path().join("ee.db");
        if let Some(parent) = database.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("outcome-test".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                OUTCOME_TEST_MEMORY_ID,
                &CreateMemoryInput {
                    workspace_id,
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
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
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
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
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
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
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
    fn harmful_feedback_over_source_rate_limit_is_quarantined() -> TestResult {
        let (_dir, database) = seed_outcome_database("ee-outcome-rate-limit")?;
        for index in 0..DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR {
            let report = record_outcome(&OutcomeRecordOptions {
                database_path: &database,
                target_type: "memory".to_string(),
                target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
                workspace_id: None,
                signal: "harmful".to_string(),
                weight: None,
                source_type: "outcome_observed".to_string(),
                source_id: Some("spam-source".to_string()),
                reason: Some("Observed a harmful outcome.".to_string()),
                evidence_json: None,
                session_id: None,
                event_id: Some(format!("fb_{:026}", 300 + index)),
                actor: Some("test".to_string()),
                dry_run: false,
                harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
                harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
            })
            .map_err(|error| error.message())?;
            ensure_equal(
                &report.status,
                &OutcomeRecordStatus::Recorded,
                "within limit records",
            )?;
        }

        let over_limit = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "harmful".to_string(),
            weight: None,
            source_type: "outcome_observed".to_string(),
            source_id: Some("spam-source".to_string()),
            reason: Some("Burst event should be reviewed.".to_string()),
            evidence_json: None,
            session_id: None,
            event_id: Some("fb_00000000000000000000000999".to_string()),
            actor: Some("test".to_string()),
            dry_run: false,
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        })
        .map_err(|error| error.message())?;

        ensure_equal(
            &over_limit.status,
            &OutcomeRecordStatus::Quarantined,
            "sixth harmful event quarantined",
        )?;
        ensure_equal(
            &over_limit.feedback.total_count,
            &DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            "quarantined event does not affect feedback count",
        )?;
        ensure_equal(
            &over_limit.quarantine.is_some(),
            &true,
            "quarantine summary present",
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let events = connection
            .list_feedback_events_for_target("memory", OUTCOME_TEST_MEMORY_ID)
            .map_err(|error| error.to_string())?;
        ensure_equal(
            &events.len(),
            &(DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR as usize),
            "only live events are counted",
        )?;
        let quarantined = connection
            .list_feedback_quarantine(OUTCOME_TEST_WORKSPACE_ID, Some("pending"))
            .map_err(|error| error.to_string())?;
        ensure_equal(&quarantined.len(), &1_usize, "one quarantine row")?;
        ensure_equal(
            &quarantined[0].raw_event_hash.starts_with("blake3:"),
            &true,
            "raw event hash is stored",
        )
    }

    #[test]
    fn releasing_quarantined_feedback_preserves_original_payload() -> TestResult {
        let (dir, database) =
            seed_outcome_database_with_workspace_id("ee-outcome-quarantine-release", None)?;

        let first = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "harmful".to_string(),
            weight: None,
            source_type: "automated_check".to_string(),
            source_id: Some("preserved-source".to_string()),
            reason: Some("First harmful signal establishes the source count.".to_string()),
            evidence_json: None,
            session_id: None,
            event_id: Some("fb_00000000000000000000000997".to_string()),
            actor: Some("test".to_string()),
            dry_run: false,
            harmful_per_source_per_hour: 1,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            &first.status,
            &OutcomeRecordStatus::Recorded,
            "first status",
        )?;

        let quarantined = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "harmful".to_string(),
            weight: Some(7.25),
            source_type: "automated_check".to_string(),
            source_id: Some("preserved-source".to_string()),
            reason: Some("Original release reason must be preserved.".to_string()),
            evidence_json: Some(r#"{"kind":"fixture","ok":true}"#.to_string()),
            session_id: None,
            event_id: Some("fb_00000000000000000000000998".to_string()),
            actor: Some("test".to_string()),
            dry_run: false,
            harmful_per_source_per_hour: 1,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            &quarantined.status,
            &OutcomeRecordStatus::Quarantined,
            "second status",
        )?;

        let quarantine = quarantined
            .quarantine
            .as_ref()
            .ok_or_else(|| "quarantine summary missing".to_string())?;
        let quarantine_id = quarantine
            .id
            .as_ref()
            .ok_or_else(|| "quarantine id missing".to_string())?
            .clone();
        let review = super::review_feedback_quarantine(&super::OutcomeQuarantineReviewOptions {
            workspace_path: dir.path(),
            database_path: Some(&database),
            quarantine_id: &quarantine_id,
            reject: false,
            actor: Some("reviewer"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(&review.status.as_str(), &"released", "review status")?;
        ensure_equal(
            &review.feedback_event_id,
            &Some("fb_00000000000000000000000998".to_string()),
            "released event id",
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let event = connection
            .get_feedback_event("fb_00000000000000000000000998")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "released feedback event missing".to_string())?;
        ensure_equal(
            &event.source_type.as_str(),
            &"automated_check",
            "source type",
        )?;
        ensure(
            (event.weight - 7.25).abs() < 0.001,
            "weight must preserve quarantined value",
        )?;
        ensure_equal(
            &event.reason,
            &Some("Original release reason must be preserved.".to_string()),
            "event reason",
        )?;
        ensure_equal(
            &event.evidence_json,
            &Some(r#"{"kind":"fixture","ok":true}"#.to_string()),
            "event evidence json",
        )?;
        ensure_equal(&event.session_id, &None, "event session id")
    }

    #[test]
    fn rejecting_quarantined_feedback_preserves_evidence_without_live_event() -> TestResult {
        let (dir, database) =
            seed_outcome_database_with_workspace_id("ee-outcome-quarantine-reject", None)?;

        let first = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "harmful".to_string(),
            weight: None,
            source_type: "automated_check".to_string(),
            source_id: Some("reject-source".to_string()),
            reason: Some("First harmful signal establishes the rate bucket.".to_string()),
            evidence_json: None,
            session_id: None,
            event_id: Some("fb_00000000000000000000000995".to_string()),
            actor: Some("test".to_string()),
            dry_run: false,
            harmful_per_source_per_hour: 1,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            &first.status,
            &OutcomeRecordStatus::Recorded,
            "first status",
        )?;

        let proposed_event_id = "fb_00000000000000000000000996".to_string();
        let quarantined = record_outcome(&OutcomeRecordOptions {
            database_path: &database,
            target_type: "memory".to_string(),
            target_id: OUTCOME_TEST_MEMORY_ID.to_string(),
            workspace_id: None,
            signal: "harmful".to_string(),
            weight: Some(3.5),
            source_type: "automated_check".to_string(),
            source_id: Some("reject-source".to_string()),
            reason: Some("Rejected payload must remain inspectable.".to_string()),
            evidence_json: Some(r#"{"kind":"reject-fixture"}"#.to_string()),
            session_id: Some("sess_00000000000000000000000996".to_string()),
            event_id: Some(proposed_event_id.clone()),
            actor: Some("test".to_string()),
            dry_run: false,
            harmful_per_source_per_hour: 1,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            &quarantined.status,
            &OutcomeRecordStatus::Quarantined,
            "second status",
        )?;

        let quarantine_id = quarantined
            .quarantine
            .as_ref()
            .and_then(|quarantine| quarantine.id.clone())
            .ok_or_else(|| "quarantine id missing".to_string())?;
        let review = super::review_feedback_quarantine(&super::OutcomeQuarantineReviewOptions {
            workspace_path: dir.path(),
            database_path: Some(&database),
            quarantine_id: &quarantine_id,
            reject: true,
            actor: Some("reviewer"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(&review.status.as_str(), &"rejected", "review status")?;
        ensure_equal(&review.changed, &true, "review changed")?;
        ensure_equal(&review.feedback_event_id, &None, "no released event id")?;
        ensure_equal(&review.audit_id.is_some(), &true, "audit id present")?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let live_events = connection
            .list_feedback_events_for_target("memory", OUTCOME_TEST_MEMORY_ID)
            .map_err(|error| error.to_string())?;
        ensure_equal(&live_events.len(), &1_usize, "only original live event")?;
        ensure_equal(
            &connection
                .get_feedback_event(&proposed_event_id)
                .map_err(|error| error.to_string())?
                .is_none(),
            &true,
            "rejected event not inserted",
        )?;

        let rejected_rows = connection
            .list_feedback_quarantine(
                &crate::core::curate::stable_workspace_id(dir.path()),
                Some("rejected"),
            )
            .map_err(|error| error.to_string())?;
        ensure_equal(&rejected_rows.len(), &1_usize, "rejected row retained")?;
        let rejected_row = rejected_rows
            .first()
            .ok_or_else(|| "rejected row missing after length check".to_string())?;
        ensure_equal(&rejected_row.id, &quarantine_id, "retained row id")?;
        ensure_equal(
            &rejected_row.status.as_str(),
            &"rejected",
            "retained row status",
        )?;
        ensure_equal(
            &rejected_row.proposed_event_id,
            &Some(proposed_event_id),
            "retained proposed event id",
        )?;
        ensure_equal(
            &rejected_row.raw_event_hash.starts_with("blake3:"),
            &true,
            "retained raw event hash",
        )?;
        ensure_equal(
            &rejected_row.released_feedback_event_id,
            &None,
            "no released feedback event",
        )
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
            harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
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
