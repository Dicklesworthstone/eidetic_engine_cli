//! Recorder subsystem for tracking agent recording sessions and events (EE-401).
//!
//! Provides append-only recording of agent activity for outcomes,
//! preflight feedback, replay, procedure distillation, and causal credit.

use std::collections::BTreeSet;
use std::str::FromStr;

use serde_json::{Value as JsonValue, json};

use crate::models::{
    ImportSourceType, RecorderEventChainStatus, RecorderEventType, RecorderRunStatus,
    RedactionStatus,
};

/// Schema for recorder start response.
pub const RECORDER_START_SCHEMA_V1: &str = "ee.recorder.start.v1";

/// Schema for recorder event response.
pub const RECORDER_EVENT_RESPONSE_SCHEMA_V1: &str = "ee.recorder.event_response.v1";

/// Schema for recorder finish response.
pub const RECORDER_FINISH_SCHEMA_V1: &str = "ee.recorder.finish.v1";

/// Schema for recorder tail response.
pub const RECORDER_TAIL_SCHEMA_V1: &str = "ee.recorder.tail.v1";

/// Schema for recorder tail follow event (JSONL).
pub const RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1: &str = "ee.recorder.tail_follow_event.v1";

/// Schema for recorder import dry-run plans.
pub const RECORDER_IMPORT_PLAN_SCHEMA_V1: &str = "ee.recorder.import_plan.v1";

/// Schema for recorder import execution results.
pub const RECORDER_IMPORT_RESULT_SCHEMA_V1: &str = "ee.recorder.import_result.v1";

/// Schema for recorder events list response.
pub const RECORDER_EVENTS_LIST_SCHEMA_V1: &str = "ee.recorder.events_list.v1";

/// Default maximum recorder event payload size accepted by the CLI.
pub const DEFAULT_MAX_RECORDER_PAYLOAD_BYTES: usize = 64 * 1024;

/// Default maximum number of source spans mapped into one recorder import plan.
pub const DEFAULT_RECORDER_IMPORT_LIMIT: usize = 100;

const DRY_RUN_TIMESTAMP: &str = "1970-01-01T00:00:00Z";

// ============================================================================
// Start Recording
// ============================================================================

/// Options for starting a recording session.
#[derive(Clone, Debug)]
pub struct RecorderStartOptions {
    /// Agent identifier.
    pub agent_id: String,
    /// Optional session identifier for correlation.
    pub session_id: Option<String>,
    /// Optional workspace identifier.
    pub workspace_id: Option<String>,
    /// Whether to perform a dry run.
    pub dry_run: bool,
}

/// Report from starting a recording session.
#[derive(Clone, Debug)]
pub struct RecorderStartReport {
    pub schema: &'static str,
    pub run_id: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    pub started_at: String,
    pub dry_run: bool,
}

impl RecorderStartReport {
    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut obj = json!({
            "schema": self.schema,
            "command": "recorder start",
            "runId": self.run_id,
            "agentId": self.agent_id,
            "startedAt": self.started_at,
            "dryRun": self.dry_run,
        });
        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(ref session_id) = self.session_id {
                obj_map.insert("sessionId".to_string(), json!(session_id));
            }
            if let Some(ref workspace_id) = self.workspace_id {
                obj_map.insert("workspaceId".to_string(), json!(workspace_id));
            }
        }
        obj
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(256);
        if self.dry_run {
            out.push_str("Recording Session [DRY RUN]\n");
        } else {
            out.push_str("Recording Session Started\n");
        }
        out.push_str("=========================\n\n");
        out.push_str(&format!("Run ID:   {}\n", self.run_id));
        out.push_str(&format!("Agent:    {}\n", self.agent_id));
        if let Some(ref session) = self.session_id {
            out.push_str(&format!("Session:  {session}\n"));
        }
        if let Some(ref workspace) = self.workspace_id {
            out.push_str(&format!("Workspace: {workspace}\n"));
        }
        out.push_str(&format!("Started:  {}\n", self.started_at));
        out.push_str("\nNext:\n  ee recorder event <run-id> --type tool_call\n");
        out
    }
}

/// Start a new recording session.
#[must_use]
pub fn start_recording(options: &RecorderStartOptions) -> RecorderStartReport {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let run_id = format!("run_{}", uuid::Uuid::now_v7());

    RecorderStartReport {
        schema: RECORDER_START_SCHEMA_V1,
        run_id,
        agent_id: options.agent_id.clone(),
        session_id: options.session_id.clone(),
        workspace_id: options.workspace_id.clone(),
        started_at: timestamp,
        dry_run: options.dry_run,
    }
}

// ============================================================================
// Record Event
// ============================================================================

/// Options for recording an event.
#[derive(Clone, Debug)]
pub struct RecorderEventOptions {
    /// Run ID to add event to.
    pub run_id: String,
    /// Type of event.
    pub event_type: RecorderEventType,
    /// Optional payload content.
    pub payload: Option<String>,
    /// Whether payload should be redacted.
    pub redact: bool,
    /// Optional previous event hash for append-only chain continuity.
    pub previous_event_hash: Option<String>,
    /// Maximum accepted payload size in bytes.
    pub max_payload_bytes: usize,
    /// Whether to perform a dry run.
    pub dry_run: bool,
}

/// Report from recording an event.
#[derive(Clone, Debug)]
pub struct RecorderEventReport {
    pub schema: &'static str,
    pub event_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event_type: RecorderEventType,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub payload_bytes: u64,
    pub payload_accepted: bool,
    pub redaction_status: RedactionStatus,
    pub redaction_classes: Vec<String>,
    pub placeholder_count: u64,
    pub redacted_bytes: u64,
    pub previous_event_hash: Option<String>,
    pub event_hash: String,
    pub chain_status: RecorderEventChainStatus,
    pub dry_run: bool,
}

impl RecorderEventReport {
    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut obj = json!({
            "schema": self.schema,
            "command": "recorder event",
            "eventId": self.event_id,
            "runId": self.run_id,
            "sequence": self.sequence,
            "eventType": self.event_type.as_str(),
            "timestamp": self.timestamp,
            "payloadBytes": self.payload_bytes,
            "payloadAccepted": self.payload_accepted,
            "redactionStatus": self.redaction_status.as_str(),
            "redactionClasses": self.redaction_classes,
            "placeholderCount": self.placeholder_count,
            "redactedBytes": self.redacted_bytes,
            "previousEventHash": self.previous_event_hash,
            "eventHash": self.event_hash,
            "chainStatus": self.chain_status.as_str(),
            "dryRun": self.dry_run,
        });
        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(ref hash) = self.payload_hash {
                obj_map.insert("payloadHash".to_string(), json!(hash));
            }
        }
        obj
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(256);
        if self.dry_run {
            out.push_str("Recorder Event [DRY RUN]\n");
        } else {
            out.push_str("Recorder Event Added\n");
        }
        out.push_str("=====================\n\n");
        out.push_str(&format!("Event ID: {}\n", self.event_id));
        out.push_str(&format!("Run ID:   {}\n", self.run_id));
        out.push_str(&format!("Sequence: {}\n", self.sequence));
        out.push_str(&format!("Type:     {}\n", self.event_type));
        out.push_str(&format!("Time:     {}\n", self.timestamp));
        if let Some(ref hash) = self.payload_hash {
            out.push_str(&format!("Payload:  {hash}\n"));
        }
        out.push_str(&format!("Bytes:    {}\n", self.payload_bytes));
        out.push_str(&format!("Redacted: {}\n", self.redaction_status));
        if !self.redaction_classes.is_empty() {
            out.push_str(&format!(
                "\nClasses:  {}",
                self.redaction_classes.join(", ")
            ));
        }
        out.push_str(&format!("\nEvent hash: {}\n", self.event_hash));
        out.push_str(&format!("Chain:    {}\n", self.chain_status));
        out
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderEventRejectionCode {
    PayloadTooLarge,
}

impl RecorderEventRejectionCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PayloadTooLarge => "recorder_payload_too_large",
        }
    }
}

/// Stable rejection details for recorder event validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderEventError {
    pub code: RecorderEventRejectionCode,
    pub message: String,
    pub repair: String,
    pub payload_bytes: usize,
    pub max_payload_bytes: usize,
}

impl RecorderEventError {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code.as_str(),
            "message": self.message,
            "severity": "medium",
            "repair": self.repair,
            "details": {
                "payloadBytes": self.payload_bytes,
                "maxPayloadBytes": self.max_payload_bytes,
            },
        })
    }
}

impl std::fmt::Display for RecorderEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecorderEventError {}

/// Record an event to a recording session.
pub fn record_event(
    options: &RecorderEventOptions,
    sequence: u64,
) -> Result<RecorderEventReport, RecorderEventError> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let event_id = format!("evt_{}", uuid::Uuid::now_v7());
    let payload = inspect_event_payload(
        options.payload.as_deref(),
        options.redact,
        options.max_payload_bytes,
    )?;

    let chain_status =
        RecorderEventChainStatus::for_event(sequence, options.previous_event_hash.as_deref());
    let event_hash = event_chain_hash(
        &options.run_id,
        sequence,
        options.event_type,
        &timestamp,
        payload.hash.as_deref(),
        payload.redaction_status,
        options.previous_event_hash.as_deref(),
    );

    Ok(RecorderEventReport {
        schema: RECORDER_EVENT_RESPONSE_SCHEMA_V1,
        event_id,
        run_id: options.run_id.clone(),
        sequence,
        event_type: options.event_type,
        timestamp,
        payload_hash: payload.hash,
        payload_bytes: usize_to_u64(payload.bytes),
        payload_accepted: options.payload.is_some(),
        redaction_status: payload.redaction_status,
        redaction_classes: payload.redaction_classes,
        placeholder_count: payload.placeholder_count,
        redacted_bytes: usize_to_u64(payload.redacted_bytes),
        previous_event_hash: options.previous_event_hash.clone(),
        event_hash,
        chain_status,
        dry_run: options.dry_run,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EventPayloadInspection {
    hash: Option<String>,
    bytes: usize,
    redaction_status: RedactionStatus,
    redaction_classes: Vec<String>,
    placeholder_count: u64,
    redacted_bytes: usize,
}

fn inspect_event_payload(
    payload: Option<&str>,
    force_redact: bool,
    max_payload_bytes: usize,
) -> Result<EventPayloadInspection, RecorderEventError> {
    let Some(payload) = payload else {
        return Ok(EventPayloadInspection {
            hash: None,
            bytes: 0,
            redaction_status: RedactionStatus::None,
            redaction_classes: Vec::new(),
            placeholder_count: 0,
            redacted_bytes: 0,
        });
    };

    let bytes = payload.len();
    if bytes > max_payload_bytes {
        return Err(RecorderEventError {
            code: RecorderEventRejectionCode::PayloadTooLarge,
            message: format!(
                "Recorder event payload is {bytes} bytes, exceeding the {max_payload_bytes} byte limit."
            ),
            repair: "Use a smaller payload, attach evidence by hash, or raise --max-payload-bytes intentionally.".to_string(),
            payload_bytes: bytes,
            max_payload_bytes,
        });
    }

    let redaction_classes = if force_redact {
        vec!["manual".to_string()]
    } else {
        detected_redaction_classes(payload)
    };
    let redacted = !redaction_classes.is_empty();
    let effective_payload = if redacted {
        format!("[REDACTED:{}:{} bytes]", redaction_classes.join(","), bytes)
    } else {
        payload.to_string()
    };

    Ok(EventPayloadInspection {
        hash: Some(blake3_hash(effective_payload.as_bytes())),
        bytes,
        redaction_status: if redacted {
            RedactionStatus::Full
        } else {
            RedactionStatus::None
        },
        placeholder_count: u64::try_from(redaction_classes.len()).unwrap_or(u64::MAX),
        redacted_bytes: if redacted { bytes } else { 0 },
        redaction_classes,
    })
}

fn detected_redaction_classes(payload: &str) -> Vec<String> {
    let lower = payload.to_ascii_lowercase();
    let mut classes = Vec::new();
    for (marker, class) in [
        ("api_key", "api_key"),
        ("apikey", "api_key"),
        ("password", "password"),
        ("passwd", "password"),
        ("private_key", "private_key"),
        ("ssh_key", "ssh_key"),
        ("secret", "secret"),
        ("token", "token"),
        // SRR6.46.1 / bd-36bbk.1.1 — tailscale identity material in any
        // shape (snake_case parser output, camelCase JSON renderer output).
        // The detector is substring-based and case-insensitive, so a
        // payload containing any of these markers gets tagged with the
        // `tailscale_metadata` class even when the volatile-field strip
        // pass has not yet been applied. Pairs with the
        // `tailscale_metadata` entry in `privacy.redaction_classes` and
        // with the field registrations in `src/obs/volatile_fields.rs`.
        ("selfnodekey", "tailscale_metadata"),
        ("selftailscaleip", "tailscale_metadata"),
        ("selfmagicdnsname", "tailscale_metadata"),
        ("tailnetid", "tailscale_metadata"),
        ("tailnetdisplayname", "tailscale_metadata"),
        ("selfadvertisedtags", "tailscale_metadata"),
        ("binaryversionraw", "tailscale_metadata"),
        ("binaryabsolutepath", "tailscale_metadata"),
    ] {
        if lower.contains(marker) && !classes.iter().any(|known| known == class) {
            classes.push(class.to_string());
        }
    }
    classes.sort();
    classes
}

fn event_chain_hash(
    run_id: &str,
    sequence: u64,
    event_type: RecorderEventType,
    timestamp: &str,
    payload_hash: Option<&str>,
    redaction_status: RedactionStatus,
    previous_event_hash: Option<&str>,
) -> String {
    let canonical = json!({
        "runId": run_id,
        "sequence": sequence,
        "eventType": event_type.as_str(),
        "timestamp": timestamp,
        "payloadHash": payload_hash,
        "redactionStatus": redaction_status.as_str(),
        "previousEventHash": previous_event_hash,
    });
    blake3_hash(canonical.to_string().as_bytes())
}

fn blake3_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

// ============================================================================
// Import Recording Plan
// ============================================================================

/// Options for planning a dry-run recorder import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderImportOptions {
    /// Source connector family.
    pub source_type: ImportSourceType,
    /// Stable external source identity.
    pub source_id: String,
    /// Optional source JSON payload, currently CASS `view --json`.
    pub input_json: Option<String>,
    /// Optional source path reported in output only.
    pub input_path: Option<String>,
    /// Agent identifier for the planned recorder run.
    pub agent_id: Option<String>,
    /// Session identifier for correlation.
    pub session_id: Option<String>,
    /// Workspace identifier for correlation.
    pub workspace_id: Option<String>,
    /// Maximum source events to map.
    pub max_events: usize,
    /// Force all mapped payloads through redaction.
    pub redact: bool,
    /// Maximum accepted payload size in bytes.
    pub max_payload_bytes: usize,
    /// Whether this is a read-only dry run.
    pub dry_run: bool,
}

/// A mapped source event in a recorder import plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderImportEventPlan {
    pub action: &'static str,
    pub source_span_id: String,
    pub source_line_start: u32,
    pub source_line_end: u32,
    pub event_id: String,
    pub sequence: u64,
    pub event_type: RecorderEventType,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub payload_bytes: u64,
    pub redaction_status: RedactionStatus,
    pub redaction_classes: Vec<String>,
    pub redacted_bytes: u64,
    pub previous_event_hash: Option<String>,
    pub event_hash: String,
    pub chain_status: RecorderEventChainStatus,
}

/// Summary returned by `ee recorder import --dry-run`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderImportPlanReport {
    pub schema: &'static str,
    pub source_type: ImportSourceType,
    pub source_id: String,
    pub input_path: Option<String>,
    pub connector: &'static str,
    pub run_id: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub dry_run: bool,
    pub events_discovered: u64,
    pub events_mapped: u64,
    pub events_rejected: u64,
    pub payload_bytes: u64,
    pub redacted_count: u64,
    pub redacted_bytes: u64,
    pub redaction_classes: Vec<String>,
    pub chain_complete: bool,
    pub events: Vec<RecorderImportEventPlan>,
    pub warnings: Vec<String>,
}

impl RecorderImportPlanReport {
    /// Render as stable JSON data payload.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "recorder import",
            "dryRun": self.dry_run,
            "source": {
                "type": self.source_type.as_str(),
                "sourceId": self.source_id,
                "inputPath": self.input_path,
                "connector": self.connector,
            },
            "run": {
                "runId": self.run_id,
                "agentId": self.agent_id,
                "sessionId": self.session_id,
                "workspaceId": self.workspace_id,
                "status": RecorderRunStatus::Imported.as_str(),
                "startedAt": self.started_at,
                "endedAt": self.ended_at,
                "eventCount": self.events_mapped,
                "redactedCount": self.redacted_count,
            },
            "summary": {
                "eventsDiscovered": self.events_discovered,
                "eventsMapped": self.events_mapped,
                "eventsRejected": self.events_rejected,
                "payloadBytes": self.payload_bytes,
                "redactedBytes": self.redacted_bytes,
                "redactionClasses": self.redaction_classes,
                "chainComplete": self.chain_complete,
            },
            "mutations": [
                {
                    "action": "would_create_run",
                    "count": 1,
                    "schema": "ee.recorder.run.v1",
                },
                {
                    "action": "would_create_event",
                    "count": self.events_mapped,
                    "schema": "ee.recorder.event.v1",
                },
            ],
            "events": self.events.iter().map(|event| json!({
                "action": event.action,
                "sourceSpanId": event.source_span_id,
                "sourceLineStart": event.source_line_start,
                "sourceLineEnd": event.source_line_end,
                "eventId": event.event_id,
                "sequence": event.sequence,
                "eventType": event.event_type.as_str(),
                "timestamp": event.timestamp,
                "payloadHash": event.payload_hash,
                "payloadBytes": event.payload_bytes,
                "redactionStatus": event.redaction_status.as_str(),
                "redactionClasses": event.redaction_classes,
                "redactedBytes": event.redacted_bytes,
                "previousEventHash": event.previous_event_hash,
                "eventHash": event.event_hash,
                "chainStatus": event.chain_status.as_str(),
            })).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::with_capacity(256);
        output.push_str("Recorder Import Plan [DRY RUN]\n");
        output.push_str("==============================\n\n");
        output.push_str(&format!(
            "Source:   {} {}\n",
            self.source_type, self.source_id
        ));
        output.push_str(&format!("Run ID:   {}\n", self.run_id));
        output.push_str(&format!("Agent:    {}\n", self.agent_id));
        output.push_str(&format!("Events:   {}\n", self.events_mapped));
        output.push_str(&format!("Redacted: {}\n", self.redacted_count));
        output.push_str("\nNo recorder records were written.\n");
        output
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderImportErrorCode {
    DryRunRequired,
    InvalidInputJson,
    InvalidSourceType,
    InvalidSourceShape,
    PayloadTooLarge,
    DatabaseError,
}

impl RecorderImportErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DryRunRequired => "recorder_import_dry_run_required",
            Self::InvalidInputJson => "recorder_import_invalid_json",
            Self::InvalidSourceType => "recorder_import_invalid_source_type",
            Self::InvalidSourceShape => "recorder_import_invalid_source_shape",
            Self::PayloadTooLarge => "recorder_import_payload_too_large",
            Self::DatabaseError => "recorder_import_database_error",
        }
    }
}

/// Stable recorder import planning error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderImportError {
    pub code: RecorderImportErrorCode,
    pub message: String,
    pub repair: String,
    pub details: Box<JsonValue>,
}

impl RecorderImportError {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code.as_str(),
            "message": self.message,
            "severity": "medium",
            "repair": self.repair,
            "details": self.details.as_ref(),
        })
    }
}

impl std::fmt::Display for RecorderImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecorderImportError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceEvent {
    span_id: String,
    line_start: u32,
    line_end: u32,
    event_type: RecorderEventType,
    payload: String,
}

/// Plan a read-only recorder import from connector JSON.
///
/// # Errors
///
/// Returns [`RecorderImportError`] for unsupported mutation mode, malformed
/// source JSON, unsupported source shapes, or payload validation failures.
pub fn plan_recorder_import(
    options: &RecorderImportOptions,
) -> Result<RecorderImportPlanReport, RecorderImportError> {
    let source_events = parse_import_source_events(options)?;
    let discovered = usize_to_u64(source_events.len());
    let max_events = options.max_events.max(1);
    let limited = source_events.into_iter().take(max_events);
    let run_id = stable_prefixed_id(
        "run",
        &format!("{}:{}", options.source_type, options.source_id),
    );
    let agent_id = options
        .agent_id
        .clone()
        .unwrap_or_else(|| default_agent_id(options.source_type).to_string());
    let mut events = Vec::new();
    let mut previous_event_hash = None;

    for (index, source_event) in limited.enumerate() {
        let sequence = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let payload = inspect_event_payload(
            Some(source_event.payload.as_str()),
            options.redact,
            options.max_payload_bytes,
        )
        .map_err(|error| {
            let details = Box::new(error.data_json()["details"].clone());
            RecorderImportError {
                code: RecorderImportErrorCode::PayloadTooLarge,
                message: error.message,
                repair: error.repair,
                details,
            }
        })?;
        let timestamp = DRY_RUN_TIMESTAMP.to_string();
        let chain_status =
            RecorderEventChainStatus::for_event(sequence, previous_event_hash.as_deref());
        let event_hash = event_chain_hash(
            &run_id,
            sequence,
            source_event.event_type,
            &timestamp,
            payload.hash.as_deref(),
            payload.redaction_status,
            previous_event_hash.as_deref(),
        );
        let event_id = stable_prefixed_id(
            "evt",
            &format!("{}:{}:{}", run_id, sequence, source_event.span_id),
        );
        events.push(RecorderImportEventPlan {
            action: "would_record",
            source_span_id: source_event.span_id,
            source_line_start: source_event.line_start,
            source_line_end: source_event.line_end,
            event_id,
            sequence,
            event_type: source_event.event_type,
            timestamp,
            payload_hash: payload.hash,
            payload_bytes: usize_to_u64(payload.bytes),
            redaction_status: payload.redaction_status,
            redaction_classes: payload.redaction_classes,
            redacted_bytes: usize_to_u64(payload.redacted_bytes),
            previous_event_hash: previous_event_hash.clone(),
            event_hash: event_hash.clone(),
            chain_status,
        });
        previous_event_hash = Some(event_hash);
    }

    let mut redaction_classes = Vec::new();
    for event in &events {
        for class in &event.redaction_classes {
            if !redaction_classes.iter().any(|known| known == class) {
                redaction_classes.push(class.clone());
            }
        }
    }
    redaction_classes.sort();

    let payload_bytes = events.iter().fold(0_u64, |total, event| {
        total.saturating_add(event.payload_bytes)
    });
    let redacted_bytes = events.iter().fold(0_u64, |total, event| {
        total.saturating_add(event.redacted_bytes)
    });
    let redacted_count = usize_to_u64(
        events
            .iter()
            .filter(|event| event.redaction_status.is_redacted())
            .count(),
    );
    let mapped = usize_to_u64(events.len());
    let mut warnings = Vec::new();
    if discovered > mapped {
        warnings.push(format!(
            "source contained {discovered} events but maxEvents limited mapping to {mapped}",
        ));
    }

    Ok(RecorderImportPlanReport {
        schema: RECORDER_IMPORT_PLAN_SCHEMA_V1,
        source_type: options.source_type,
        source_id: options.source_id.clone(),
        input_path: options.input_path.clone(),
        connector: connector_contract(options.source_type),
        run_id,
        agent_id,
        session_id: options.session_id.clone(),
        workspace_id: options.workspace_id.clone(),
        started_at: DRY_RUN_TIMESTAMP.to_string(),
        ended_at: None,
        dry_run: true,
        events_discovered: discovered,
        events_mapped: mapped,
        events_rejected: discovered.saturating_sub(mapped),
        payload_bytes,
        redacted_count,
        redacted_bytes,
        redaction_classes,
        chain_complete: events.iter().all(|event| {
            matches!(
                event.chain_status,
                RecorderEventChainStatus::Root | RecorderEventChainStatus::Linked
            )
        }),
        events,
        warnings,
    })
}

fn parse_import_source_events(
    options: &RecorderImportOptions,
) -> Result<Vec<SourceEvent>, RecorderImportError> {
    let Some(input) = options.input_json.as_deref() else {
        return Ok(Vec::new());
    };
    let value: JsonValue = serde_json::from_str(input).map_err(|error| RecorderImportError {
        code: RecorderImportErrorCode::InvalidInputJson,
        message: format!("Recorder import input is not valid JSON: {error}"),
        repair: "Provide CASS `view --json` output with a top-level lines array.".to_string(),
        details: Box::new(json!({"sourceId": options.source_id})),
    })?;

    match options.source_type {
        ImportSourceType::Cass => parse_cass_view_events(&value, &options.source_id),
        other => Err(RecorderImportError {
            code: RecorderImportErrorCode::InvalidSourceShape,
            message: format!(
                "Recorder import source '{}' does not have a supported input parser yet.",
                other.as_str()
            ),
            repair: "Use --source-type cass with CASS `view --json` output, or omit --input for an empty future-connector plan.".to_string(),
            details: Box::new(json!({"sourceType": other.as_str()})),
        }),
    }
}

fn parse_cass_view_events(
    value: &JsonValue,
    source_id: &str,
) -> Result<Vec<SourceEvent>, RecorderImportError> {
    let lines = value
        .get("lines")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| RecorderImportError {
            code: RecorderImportErrorCode::InvalidSourceShape,
            message: "Recorder import expected CASS view JSON with a lines array.".to_string(),
            repair:
                "Run cass view <session> -n <line> --json and pass the saved JSON through --input."
                    .to_string(),
            details: Box::new(json!({"missing": "lines"})),
        })?;
    let mut events = Vec::with_capacity(lines.len());
    for line in lines {
        let line_number = line
            .get("line")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| RecorderImportError {
                code: RecorderImportErrorCode::InvalidSourceShape,
                message: "Recorder import CASS line is missing numeric line.".to_string(),
                repair: "Ensure each CASS view line has a numeric line field.".to_string(),
                details: Box::new(json!({"sourceId": source_id})),
            })?;
        let content = line
            .get("content")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| RecorderImportError {
                code: RecorderImportErrorCode::InvalidSourceShape,
                message: "Recorder import CASS line is missing string content.".to_string(),
                repair: "Ensure each CASS view line has a string content field.".to_string(),
                details: Box::new(json!({"line": line_number})),
            })?;
        events.push(SourceEvent {
            span_id: format!("{source_id}:{line_number}"),
            line_start: line_number,
            line_end: line_number,
            event_type: classify_cass_line_event_type(content),
            payload: content.to_string(),
        });
    }
    Ok(events)
}

fn classify_cass_line_event_type(content: &str) -> RecorderEventType {
    let Ok(value) = serde_json::from_str::<JsonValue>(content) else {
        return RecorderEventType::UserMessage;
    };
    let line_type = value
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    match line_type {
        "tool_use" | "tool-call" | "tool_call" => return RecorderEventType::ToolCall,
        "tool_result" | "tool-result" => return RecorderEventType::ToolResult,
        "summary" | "file" | "file-history-snapshot" | "state_change" => {
            return RecorderEventType::StateChange;
        }
        _ => {}
    }

    let role = value
        .get("message")
        .and_then(|message| message.get("role"))
        .and_then(JsonValue::as_str)
        .or_else(|| value.get("role").and_then(JsonValue::as_str))
        .unwrap_or_default();
    match role {
        "assistant" | "model" => RecorderEventType::AssistantMessage,
        "system" => RecorderEventType::SystemMessage,
        "tool" | "function" => RecorderEventType::ToolResult,
        _ => RecorderEventType::UserMessage,
    }
}

fn default_agent_id(source_type: ImportSourceType) -> &'static str {
    match source_type {
        ImportSourceType::Cass => "cass",
        ImportSourceType::EideticLegacy => "eidetic-legacy",
        ImportSourceType::Recorder => "recorder",
        ImportSourceType::Manual => "manual",
    }
}

fn connector_contract(source_type: ImportSourceType) -> &'static str {
    match source_type {
        ImportSourceType::Cass => "cass_view_json",
        ImportSourceType::EideticLegacy => "future_connector",
        ImportSourceType::Recorder => "future_connector",
        ImportSourceType::Manual => "future_connector",
    }
}

fn stable_prefixed_id(prefix: &str, input: &str) -> String {
    let hash = blake3::hash(input.as_bytes()).to_hex().to_string();
    format!("{prefix}_{}", &hash[..26])
}

// ============================================================================
// Finish Recording
// ============================================================================

/// Options for finishing a recording session.
#[derive(Clone, Debug)]
pub struct RecorderFinishOptions {
    /// Run ID to finish.
    pub run_id: String,
    /// Final status.
    pub status: RecorderRunStatus,
    /// Whether to perform a dry run.
    pub dry_run: bool,
}

/// Report from finishing a recording session.
#[derive(Clone, Debug)]
pub struct RecorderFinishReport {
    pub schema: &'static str,
    pub run_id: String,
    pub status: RecorderRunStatus,
    pub ended_at: String,
    pub event_count: u64,
    pub dry_run: bool,
}

impl RecorderFinishReport {
    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "recorder finish",
            "runId": self.run_id,
            "status": self.status.as_str(),
            "endedAt": self.ended_at,
            "eventCount": self.event_count,
            "dryRun": self.dry_run,
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(256);
        if self.dry_run {
            out.push_str("Recording Session [DRY RUN]\n");
        } else {
            out.push_str("Recording Session Finished\n");
        }
        out.push_str("==========================\n\n");
        out.push_str(&format!("Run ID:  {}\n", self.run_id));
        out.push_str(&format!("Status:  {}\n", self.status));
        out.push_str(&format!("Ended:   {}\n", self.ended_at));
        out.push_str(&format!("Events:  {}\n", self.event_count));
        out
    }
}

/// Finish a recording session.
#[must_use]
pub fn finish_recording(options: &RecorderFinishOptions, event_count: u64) -> RecorderFinishReport {
    let timestamp = chrono::Utc::now().to_rfc3339();

    RecorderFinishReport {
        schema: RECORDER_FINISH_SCHEMA_V1,
        run_id: options.run_id.clone(),
        status: options.status,
        ended_at: timestamp,
        event_count,
        dry_run: options.dry_run,
    }
}

// ============================================================================
// Tail Recording
// ============================================================================

/// Options for tailing a recording session.
#[derive(Clone, Debug)]
pub struct RecorderTailOptions {
    /// Optional run ID to tail. When omitted, tail reads across all runs.
    pub run_id: Option<String>,
    /// Only include events at or after this RFC 3339 timestamp.
    pub since: Option<String>,
    /// Number of events to return.
    ///
    /// A zero value is a literal empty tail request: the public tail report
    /// returns no events, but can still set `has_more` when matching events
    /// exist. This intentionally differs from `RecorderEventsListOptions`,
    /// where zero means "unbounded".
    pub limit: u32,
    /// Return events with sequence strictly greater than this value.
    ///
    /// This is an exclusive cursor for follow-mode pagination: callers should
    /// pass the last sequence they have already observed.
    pub from_sequence: Option<u64>,
    /// Follow mode: continuously poll for new events.
    pub follow: bool,
    /// Optional simple `key=value AND key=value` filter.
    pub filter: Option<RecorderEventFilter>,
}

/// One normalized recorder event filter term.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderEventFilterTerm {
    pub key: String,
    pub value: String,
}

/// Parsed simple recorder event filter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecorderEventFilter {
    terms: Vec<RecorderEventFilterTerm>,
}

impl RecorderEventFilter {
    /// Parse `key=value AND key=value` expressions.
    ///
    /// # Errors
    ///
    /// Returns a usage error for unsupported fields or malformed terms.
    pub fn parse_expression(expression: &str) -> Result<Self, crate::models::DomainError> {
        let expression = expression.trim();
        if expression.is_empty() {
            return Ok(Self::default());
        }

        let mut terms = Vec::new();
        for raw_term in expression.split(" AND ") {
            let Some((raw_key, raw_value)) = raw_term.split_once('=') else {
                return Err(crate::models::DomainError::Usage {
                    message: format!(
                        "Invalid recorder filter term `{raw_term}`: expected key=value"
                    ),
                    repair: Some(
                        "Use filters such as `event_type=tool_call AND redacted=false`.".to_owned(),
                    ),
                });
            };
            let key = normalize_recorder_filter_key(raw_key).ok_or_else(|| {
                crate::models::DomainError::Usage {
                    message: format!("Unsupported recorder filter key `{}`", raw_key.trim()),
                    repair: Some(
                        "Use one of: run_id, event_id, event_type, redaction_status, redacted, chain_status, source."
                            .to_owned(),
                    ),
                }
            })?;
            let value = raw_value.trim();
            if value.is_empty() {
                return Err(crate::models::DomainError::Usage {
                    message: format!("Recorder filter key `{key}` has an empty value"),
                    repair: Some("Use filters such as `run_id=run_123`.".to_owned()),
                });
            }
            if key == "redacted" && !matches!(value, "true" | "false") {
                return Err(crate::models::DomainError::Usage {
                    message: format!(
                        "Recorder filter key `redacted` expects true or false, got `{value}`"
                    ),
                    repair: Some("Use `redacted=true` or `redacted=false`.".to_owned()),
                });
            }
            terms.push(RecorderEventFilterTerm {
                key,
                value: value.to_owned(),
            });
        }

        Ok(Self { terms })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    #[must_use]
    pub fn first_value(&self, key: &str) -> Option<&str> {
        self.terms
            .iter()
            .find(|term| term.key == key)
            .map(|term| term.value.as_str())
    }

    /// True if any term must be evaluated client-side because it isn't pushed
    /// down into `list_recorder_events_filtered` (which currently only applies
    /// `run_id` and `source_type` from the filter expression).
    #[must_use]
    pub fn has_client_only_terms(&self) -> bool {
        self.terms
            .iter()
            .any(|term| !matches!(term.key.as_str(), "run_id" | "source_type"))
    }

    #[must_use]
    pub fn matches_summary(&self, event: &RecorderEventSummary) -> bool {
        self.terms.iter().all(|term| match term.key.as_str() {
            "run_id" => event.run_id == term.value,
            "event_id" => event.event_id == term.value,
            "event_type" => event.event_type.as_str() == term.value,
            "redaction_status" => event.redaction_status == term.value,
            "redacted" => match term.value.as_str() {
                "true" => event.redacted,
                "false" => !event.redacted,
                _ => false,
            },
            "chain_status" => event.chain_status == term.value,
            "source_type" => true,
            _ => true,
        })
    }
}

/// A single follow event emitted in JSONL format.
#[derive(Clone, Debug)]
pub struct RecorderTailFollowEvent {
    pub schema: &'static str,
    pub run_id: String,
    pub event_id: String,
    pub sequence: u64,
    pub event_type: RecorderEventType,
    pub timestamp: String,
    pub redacted: bool,
    pub payload_preview: Option<String>,
}

impl RecorderTailFollowEvent {
    /// Render as a single JSONL line (no trailing newline).
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let mut obj = json!({
            "schema": self.schema,
            "runId": self.run_id,
            "eventId": self.event_id,
            "sequence": self.sequence,
            "eventType": self.event_type.as_str(),
            "timestamp": self.timestamp,
            "redacted": self.redacted,
        });
        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(ref preview) = self.payload_preview {
                obj_map.insert("payloadPreview".to_string(), json!(preview));
            }
        }
        obj.to_string()
    }
}

/// Result from follow mode iteration.
#[derive(Clone, Debug)]
pub enum TailFollowResult {
    /// New events available.
    Events(Vec<RecorderTailFollowEvent>),
    /// Run has completed, no more events expected.
    RunCompleted { final_sequence: u64 },
    /// Run not found.
    RunNotFound,
    /// Recorder store is not wired, so run state cannot be observed.
    StoreUnavailable { run_id: String },
    /// No new events, still active.
    Waiting { last_sequence: u64 },
}

/// Status of a caller-supplied recorder run snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderFollowRunStatus {
    Active,
    Completed,
}

/// Persisted recorder run snapshot used by follow-mode polling.
#[derive(Clone, Debug)]
pub struct RecorderFollowSnapshot {
    pub run_id: String,
    pub status: RecorderFollowRunStatus,
    pub events: Vec<RecorderEventSummary>,
}

/// Report from tailing a recording session.
#[derive(Clone, Debug)]
pub struct RecorderTailReport {
    pub schema: &'static str,
    pub run_id: Option<String>,
    pub events: Vec<RecorderEventSummary>,
    pub total_events: u64,
    pub has_more: bool,
}

/// Summary of a recorded event for tail output.
#[derive(Clone, Debug)]
pub struct RecorderEventSummary {
    pub event_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event_type: RecorderEventType,
    pub timestamp: String,
    pub redacted: bool,
    pub redaction_status: String,
    pub event_hash: String,
    pub chain_status: String,
}

impl RecorderTailReport {
    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "recorder tail",
            "runId": self.run_id,
            "events": self.events.iter().map(|e| json!({
                "eventId": e.event_id,
                "runId": e.run_id,
                "sequence": e.sequence,
                "eventType": e.event_type.as_str(),
                "timestamp": e.timestamp,
                "redacted": e.redacted,
                "redactionStatus": e.redaction_status,
                "eventHash": e.event_hash,
                "chainStatus": e.chain_status,
            })).collect::<Vec<_>>(),
            "totalEvents": self.total_events,
            "hasMore": self.has_more,
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        let scope = self.run_id.as_deref().unwrap_or("all runs");
        out.push_str(&format!("Recording Tail: {scope}\n"));
        out.push_str("==================\n\n");
        out.push_str(&format!("Total events: {}\n\n", self.total_events));

        if self.events.is_empty() {
            out.push_str("No events found.\n");
        } else {
            out.push_str("Recent events:\n");
            for event in &self.events {
                let redact_flag = if event.redacted { " [R]" } else { "" };
                out.push_str(&format!(
                    "  #{} {} {} {}{}\n",
                    event.sequence, event.event_id, event.event_type, event.timestamp, redact_flag
                ));
            }
        }

        if self.has_more {
            out.push_str("\n(more events available)\n");
        }
        out
    }
}

/// Tail events from a recording session when no recorder store is wired.
///
/// The CLI reports recorder tail as unavailable until a store-backed caller can
/// supply persisted events. This helper intentionally returns an empty snapshot
/// rather than fabricating events.
#[must_use]
pub fn tail_recording(options: &RecorderTailOptions) -> RecorderTailReport {
    tail_recording_from_events(options, &[])
}

/// Tail events from a caller-supplied persisted recorder event snapshot.
#[must_use]
pub fn tail_recording_from_events(
    options: &RecorderTailOptions,
    events: &[RecorderEventSummary],
) -> RecorderTailReport {
    let mut matching = events
        .iter()
        .filter(|event| {
            options
                .run_id
                .as_ref()
                .is_none_or(|run_id| &event.run_id == run_id)
        })
        .filter(|event| {
            options
                .since
                .as_ref()
                .is_none_or(|since| timestamp_is_at_or_after(&event.timestamp, since))
        })
        .filter(|event| {
            options
                .from_sequence
                .is_none_or(|from_sequence| event.sequence > from_sequence)
        })
        .filter(|event| {
            options
                .filter
                .as_ref()
                .is_none_or(|filter| filter.matches_summary(event))
        })
        .cloned()
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.sequence.cmp(&right.sequence))
            .then_with(|| left.event_id.cmp(&right.event_id))
    });

    let total_events = usize_to_u64(matching.len());
    let limit = usize::try_from(options.limit).unwrap_or(usize::MAX);
    let has_more = matching.len() > limit;
    if has_more {
        if options.from_sequence.is_some() || options.since.is_some() {
            matching.truncate(limit);
        } else {
            let start = matching.len().saturating_sub(limit);
            matching = matching.split_off(start);
        }
    }

    RecorderTailReport {
        schema: RECORDER_TAIL_SCHEMA_V1,
        run_id: options.run_id.clone(),
        events: matching,
        total_events,
        has_more,
    }
}

fn timestamp_is_at_or_after(timestamp: &str, since: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(timestamp),
        chrono::DateTime::parse_from_rfc3339(since),
    ) {
        (Ok(timestamp), Ok(since)) => timestamp >= since,
        _ => false,
    }
}

/// Headroom multiplier for the SQL `LIMIT` when extra client-side filtering
/// (terms beyond `run_id`/`source_type`, or `from_sequence`) needs slack to
/// still surface enough matching rows after the in-memory filter pass.
const TAIL_STORE_HEADROOM_MULTIPLIER: u32 = 4;

/// Absolute cap on the SQL `LIMIT` for the headroom branch.
const TAIL_STORE_HEADROOM_CAP: u32 = 10_000;

/// Decide how many rows to ask the SQL layer for when servicing
/// `tail_recording_from_store`. Always returns at least `options.limit + 1`
/// (the `+1` lets `tail_recording_from_events` set `has_more` correctly without
/// a separate `COUNT(*)` round-trip), and applies headroom when the filter
/// requires post-fetch evaluation. With `limit = 0`, this still fetches one row
/// so the public tail report can return zero events while distinguishing
/// "matching data exists" from "no matching data".
fn tail_recording_store_sql_limit(options: &RecorderTailOptions) -> u32 {
    let needs_extra_filtering = options.from_sequence.is_some()
        || options
            .filter
            .as_ref()
            .is_some_and(RecorderEventFilter::has_client_only_terms);

    let base = if needs_extra_filtering {
        options
            .limit
            .saturating_mul(TAIL_STORE_HEADROOM_MULTIPLIER)
            .min(TAIL_STORE_HEADROOM_CAP)
            .max(options.limit)
    } else {
        options.limit
    };

    base.saturating_add(1)
}

/// Tail events from the persisted recorder store.
///
/// # Errors
///
/// Returns a storage error when persisted rows cannot be read or contain an
/// invalid event type.
pub fn tail_recording_from_store(
    conn: &crate::db::DbConnection,
    options: &RecorderTailOptions,
) -> Result<RecorderTailReport, crate::models::DomainError> {
    let filter_run_id = options
        .filter
        .as_ref()
        .and_then(|filter| filter.first_value("run_id"));
    let query_run_id = options.run_id.as_deref().or(filter_run_id);
    let query_source = options
        .filter
        .as_ref()
        .and_then(|filter| filter.first_value("source_type"));
    let sql_limit = tail_recording_store_sql_limit(options);
    let stored_events = conn
        .list_recorder_events_filtered(
            query_run_id,
            options.since.as_deref(),
            query_source,
            sql_limit,
        )
        .map_err(|error| crate::models::DomainError::Storage {
            message: format!("Failed to read recorder events: {error}"),
            repair: Some("ee status --json".to_owned()),
        })?;
    let summaries = stored_events
        .into_iter()
        .map(recorder_event_summary_from_stored)
        .collect::<Result<Vec<_>, _>>()?;
    let effective_options = RecorderTailOptions {
        run_id: options
            .run_id
            .clone()
            .or_else(|| filter_run_id.map(str::to_owned)),
        since: options.since.clone(),
        limit: options.limit,
        from_sequence: options.from_sequence,
        follow: options.follow,
        filter: options.filter.clone(),
    };

    Ok(tail_recording_from_events(&effective_options, &summaries))
}

// ============================================================================
// Follow Mode (EE-RECORDER-FOLLOW-001)
// ============================================================================

/// Configuration for follow mode polling.
#[derive(Clone, Debug)]
pub struct FollowConfig {
    /// Minimum poll interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Maximum backoff interval in milliseconds.
    pub max_backoff_ms: u64,
    /// Current backoff multiplier.
    pub backoff_multiplier: f64,
}

impl Default for FollowConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 250,
            max_backoff_ms: 2000,
            backoff_multiplier: 1.5,
        }
    }
}

/// Poll for new events in follow mode when no recorder store is wired.
#[must_use]
pub fn poll_follow_events(run_id: &str, _from_sequence: u64, _limit: u32) -> TailFollowResult {
    TailFollowResult::StoreUnavailable {
        run_id: run_id.to_string(),
    }
}

/// Poll for new events from a caller-supplied persisted run snapshot.
#[must_use]
pub fn poll_follow_events_from_snapshot(
    snapshot: Option<&RecorderFollowSnapshot>,
    from_sequence: u64,
    limit: u32,
) -> TailFollowResult {
    let Some(snapshot) = snapshot else {
        return TailFollowResult::RunNotFound;
    };

    let tail = tail_recording_from_events(
        &RecorderTailOptions {
            run_id: Some(snapshot.run_id.clone()),
            since: None,
            limit,
            from_sequence: Some(from_sequence),
            follow: true,
            filter: None,
        },
        &snapshot.events,
    );

    if !tail.events.is_empty() {
        let events = tail
            .events
            .into_iter()
            .map(|event| RecorderTailFollowEvent {
                schema: RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
                run_id: event.run_id,
                event_id: event.event_id,
                sequence: event.sequence,
                event_type: event.event_type,
                timestamp: event.timestamp,
                redacted: event.redacted,
                payload_preview: None,
            })
            .collect();
        return TailFollowResult::Events(events);
    }

    let final_sequence = snapshot
        .events
        .iter()
        .map(|event| event.sequence)
        .max()
        .unwrap_or_else(|| from_sequence.saturating_sub(1));

    if snapshot.status == RecorderFollowRunStatus::Completed {
        return TailFollowResult::RunCompleted { final_sequence };
    }

    TailFollowResult::Waiting {
        last_sequence: final_sequence,
    }
}

/// Poll the persisted recorder store once for follow-mode output.
///
/// # Errors
///
/// Returns storage errors when the recorder store cannot be read.
pub fn poll_follow_events_from_store(
    conn: &crate::db::DbConnection,
    options: &RecorderTailOptions,
    seen_event_ids: &BTreeSet<String>,
) -> Result<TailFollowResult, crate::models::DomainError> {
    let report = tail_recording_from_store(conn, options)?;
    let follow_run_id = report.run_id.clone();
    let mut max_sequence = options.from_sequence.unwrap_or(0);
    let events = report
        .events
        .into_iter()
        .inspect(|event| {
            max_sequence = max_sequence.max(event.sequence);
        })
        .filter(|event| !seen_event_ids.contains(&event.event_id))
        .map(|event| RecorderTailFollowEvent {
            schema: RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
            run_id: event.run_id,
            event_id: event.event_id,
            sequence: event.sequence,
            event_type: event.event_type,
            timestamp: event.timestamp,
            redacted: event.redacted,
            payload_preview: None,
        })
        .collect::<Vec<_>>();

    if !events.is_empty() {
        return Ok(TailFollowResult::Events(events));
    }

    if let Some(run_id) = follow_run_id.as_deref() {
        let Some(run) =
            conn.get_recorder_run(run_id)
                .map_err(|error| crate::models::DomainError::Storage {
                    message: format!("Failed to read recorder run {run_id}: {error}"),
                    repair: Some("ee status --json".to_owned()),
                })?
        else {
            return Ok(TailFollowResult::RunNotFound);
        };
        let status = RecorderRunStatus::from_str(&run.status).map_err(|error| {
            crate::models::DomainError::Storage {
                message: format!("Recorder run {run_id} has invalid status: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            }
        })?;
        if status.is_terminal() {
            return Ok(TailFollowResult::RunCompleted {
                final_sequence: max_sequence,
            });
        }
    }

    Ok(TailFollowResult::Waiting {
        last_sequence: max_sequence,
    })
}

/// Generate a follow mode diagnostic message for stderr.
#[must_use]
pub fn follow_diagnostic(result: &TailFollowResult) -> Option<String> {
    match result {
        TailFollowResult::Events(events) => {
            if events.is_empty() {
                None
            } else {
                Some(format!("received {} event(s)", events.len()))
            }
        }
        TailFollowResult::RunCompleted { final_sequence } => {
            Some(format!("run completed at sequence {final_sequence}"))
        }
        TailFollowResult::RunNotFound => Some("run not found".to_string()),
        TailFollowResult::StoreUnavailable { run_id } => {
            Some(format!("recorder store unavailable for run {run_id}"))
        }
        TailFollowResult::Waiting { last_sequence } => {
            Some(format!("waiting (last seq: {last_sequence})"))
        }
    }
}

fn normalize_recorder_filter_key(raw: &str) -> Option<String> {
    let normalized = raw.trim().replace(['-', '.'], "_").to_lowercase();
    match normalized.as_str() {
        "run_id" | "runid" => Some("run_id".to_owned()),
        "event_id" | "eventid" => Some("event_id".to_owned()),
        "event_type" | "eventtype" | "type" => Some("event_type".to_owned()),
        "redaction_status" | "redactionstatus" => Some("redaction_status".to_owned()),
        "redacted" => Some("redacted".to_owned()),
        "chain_status" | "chainstatus" => Some("chain_status".to_owned()),
        "source" | "source_type" | "sourcetype" => Some("source_type".to_owned()),
        _ => None,
    }
}

fn recorder_event_summary_from_stored(
    event: crate::db::StoredRecorderEvent,
) -> Result<RecorderEventSummary, crate::models::DomainError> {
    let event_type = RecorderEventType::from_str(&event.event_type).map_err(|error| {
        crate::models::DomainError::Storage {
            message: format!(
                "Recorder event {} has invalid event type: {error}",
                event.event_id
            ),
            repair: Some("ee doctor --json".to_owned()),
        }
    })?;
    let redacted = event.redaction_status != "clean";
    Ok(RecorderEventSummary {
        event_id: event.event_id,
        run_id: event.run_id,
        sequence: event.sequence,
        event_type,
        timestamp: event.timestamp,
        redacted,
        redaction_status: event.redaction_status,
        event_hash: event.event_hash,
        chain_status: event.chain_status,
    })
}

// ============================================================================
// EE-403: Recorder Run Links
// ============================================================================

/// Schema for recorder links response.
pub const RECORDER_LINKS_SCHEMA_V1: &str = "ee.recorder.links.v1";

/// Type of artifact linked to a recorder run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderLinkType {
    ContextPack,
    PreflightRun,
    Outcome,
    Tripwire,
    TaskEpisode,
}

impl RecorderLinkType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContextPack => "context_pack",
            Self::PreflightRun => "preflight_run",
            Self::Outcome => "outcome",
            Self::Tripwire => "tripwire",
            Self::TaskEpisode => "task_episode",
        }
    }
}

impl std::fmt::Display for RecorderLinkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A link between a recorder run and an artifact.
#[derive(Clone, Debug)]
pub struct RecorderLink {
    pub link_id: String,
    pub run_id: String,
    pub link_type: RecorderLinkType,
    pub artifact_id: String,
    pub created_at: String,
    pub metadata: Option<String>,
}

impl RecorderLink {
    /// Render as JSON value.
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut obj = json!({
            "linkId": self.link_id,
            "runId": self.run_id,
            "linkType": self.link_type.as_str(),
            "artifactId": self.artifact_id,
            "createdAt": self.created_at,
        });
        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(ref meta) = self.metadata {
                obj_map.insert("metadata".to_string(), json!(meta));
            }
        }
        obj
    }
}

/// Options for adding a link.
#[derive(Clone, Debug)]
pub struct RecorderLinkAddOptions {
    pub run_id: String,
    pub link_type: RecorderLinkType,
    pub artifact_id: String,
    pub metadata: Option<String>,
    pub dry_run: bool,
}

/// Report from adding a link.
#[derive(Clone, Debug)]
pub struct RecorderLinkAddReport {
    pub schema: &'static str,
    pub link: RecorderLink,
    pub dry_run: bool,
}

impl RecorderLinkAddReport {
    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "recorder link add",
            "link": self.link.to_json(),
            "dryRun": self.dry_run,
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(256);
        if self.dry_run {
            out.push_str("Recorder Link [DRY RUN]\n");
        } else {
            out.push_str("Recorder Link Added\n");
        }
        out.push_str("====================\n\n");
        out.push_str(&format!("Link ID:    {}\n", self.link.link_id));
        out.push_str(&format!("Run ID:     {}\n", self.link.run_id));
        out.push_str(&format!("Type:       {}\n", self.link.link_type));
        out.push_str(&format!("Artifact:   {}\n", self.link.artifact_id));
        out.push_str(&format!("Created:    {}\n", self.link.created_at));
        out
    }
}

/// Plan a link between a recorder run and an artifact.
///
/// Durable recorder link writes are not wired yet, so this report is always
/// marked as dry-run even if the caller requests mutation.
#[must_use]
pub fn add_link(options: &RecorderLinkAddOptions) -> RecorderLinkAddReport {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let link_id = format!("link_{}", uuid::Uuid::now_v7());

    let link = RecorderLink {
        link_id,
        run_id: options.run_id.clone(),
        link_type: options.link_type,
        artifact_id: options.artifact_id.clone(),
        created_at: timestamp,
        metadata: options.metadata.clone(),
    };

    RecorderLinkAddReport {
        schema: RECORDER_LINKS_SCHEMA_V1,
        link,
        dry_run: true,
    }
}

/// Options for listing links.
#[derive(Clone, Debug, Default)]
pub struct RecorderLinksListOptions {
    pub run_id: Option<String>,
    pub link_type: Option<RecorderLinkType>,
    pub artifact_id: Option<String>,
    pub limit: u32,
}

/// Report from listing links.
#[derive(Clone, Debug)]
pub struct RecorderLinksListReport {
    pub schema: &'static str,
    pub links: Vec<RecorderLink>,
    pub total_count: u32,
}

impl RecorderLinksListReport {
    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "recorder links list",
            "links": self.links.iter().map(|l| l.to_json()).collect::<Vec<_>>(),
            "totalCount": self.total_count,
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("Recorder Links\n");
        out.push_str("==============\n\n");
        out.push_str(&format!("Total: {}\n\n", self.total_count));

        if self.links.is_empty() {
            out.push_str("No links found.\n");
        } else {
            for link in &self.links {
                out.push_str(&format!(
                    "  {} -> {} ({})\n",
                    link.run_id, link.artifact_id, link.link_type
                ));
            }
        }
        out
    }
}

/// List links for a recorder run or artifact when no recorder store is wired.
///
/// Until callers supply persisted records, this returns an empty result rather
/// than sample links.
#[must_use]
pub fn list_links(options: &RecorderLinksListOptions) -> RecorderLinksListReport {
    list_links_from_records(options, &[])
}

/// List links from caller-supplied persisted link records.
#[must_use]
pub fn list_links_from_records(
    options: &RecorderLinksListOptions,
    links: &[RecorderLink],
) -> RecorderLinksListReport {
    let mut filtered = links
        .iter()
        .filter(|l| {
            options
                .run_id
                .as_ref()
                .is_none_or(|r| &l.run_id == r) // ubs:ignore - public recorder run ID filter, not credential comparison.
                && options.link_type.is_none_or(|t| l.link_type == t) // ubs:ignore - public enum filter, not credential comparison.
                && options
                    .artifact_id
                    .as_ref()
                    .is_none_or(|a| &l.artifact_id == a) // ubs:ignore - public artifact ID filter, not credential comparison.
        })
        .cloned()
        .collect::<Vec<_>>();
    filtered.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.link_id.cmp(&right.link_id))
    });
    let total_count = u32::try_from(filtered.len()).unwrap_or(u32::MAX);
    let limit = usize::try_from(options.limit).unwrap_or(usize::MAX);
    filtered.truncate(limit);

    RecorderLinksListReport {
        schema: RECORDER_LINKS_SCHEMA_V1,
        total_count,
        links: filtered,
    }
}

// ============================================================================
// Recorder Import Execution (EE-400, eidetic_engine_cli-nmxc)
// ============================================================================

/// Result of executing a recorder import (non-dry-run).
#[derive(Clone, Debug, PartialEq)]
pub struct RecorderImportResult {
    pub schema: &'static str,
    pub source_type: ImportSourceType,
    pub source_id: String,
    pub run_id: String,
    pub agent_id: String,
    pub workspace_id: Option<String>,
    pub dry_run: bool,
    pub events_imported: u64,
    pub events_rejected: u64,
    pub payload_bytes: u64,
    pub redacted_count: u64,
    pub chain_complete: bool,
    pub started_at: String,
    pub ended_at: String,
    pub warnings: Vec<String>,
}

impl RecorderImportResult {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "recorder import",
            "sourceType": self.source_type.as_str(),
            "sourceId": self.source_id,
            "runId": self.run_id,
            "agentId": self.agent_id,
            "workspaceId": self.workspace_id,
            "dryRun": self.dry_run,
            "eventsImported": self.events_imported,
            "eventsRejected": self.events_rejected,
            "payloadBytes": self.payload_bytes,
            "redactedCount": self.redacted_count,
            "chainComplete": self.chain_complete,
            "startedAt": self.started_at,
            "endedAt": self.ended_at,
            "warnings": self.warnings,
        })
    }
}

/// Execute a recorder import, persisting events to the database.
///
/// This function plans the import then persists the run and events. It requires
/// a database connection and workspace ID for storage.
///
/// # Errors
///
/// Returns [`RecorderImportError`] for planning failures or database errors.
pub fn execute_recorder_import(
    options: &RecorderImportOptions,
    connection: &crate::db::DbConnection,
) -> Result<RecorderImportResult, RecorderImportError> {
    use crate::db::{CreateRecorderEventInput, CreateRecorderRunInput};
    use chrono::Utc;

    let plan_options = RecorderImportOptions {
        dry_run: true,
        ..options.clone()
    };
    let plan = plan_recorder_import(&plan_options)?;

    let started_at = Utc::now().to_rfc3339();

    let run_input = CreateRecorderRunInput {
        workspace_id: plan.workspace_id.clone(),
        agent_id: plan.agent_id.clone(),
        session_id: plan.session_id.clone(),
        source_type: plan.source_type.as_str().to_string(),
        source_id: Some(plan.source_id.clone()),
        status: "imported".to_string(),
        started_at: started_at.clone(),
        ended_at: None,
        event_count: plan.events_mapped,
        redacted_count: plan.redacted_count,
        payload_bytes: plan.payload_bytes,
        chain_complete: plan.chain_complete,
    };

    connection
        .insert_recorder_run(&plan.run_id, &run_input)
        .map_err(|error| RecorderImportError {
            code: RecorderImportErrorCode::DatabaseError,
            message: format!("Failed to insert recorder run: {error}"),
            repair: "Check database connectivity and workspace initialization.".to_string(),
            details: Box::new(json!({"runId": plan.run_id, "dbError": error.to_string()})),
        })?;

    for event in &plan.events {
        let event_input = CreateRecorderEventInput {
            run_id: plan.run_id.clone(),
            sequence: event.sequence,
            event_type: event.event_type.as_str().to_string(),
            timestamp: event.timestamp.clone(),
            payload_hash: event.payload_hash.clone(),
            payload_bytes: event.payload_bytes,
            redaction_status: event.redaction_status.as_db_str().to_string(),
            redacted_bytes: event.redacted_bytes,
            previous_event_hash: event.previous_event_hash.clone(),
            event_hash: event.event_hash.clone(),
            chain_status: event.chain_status.as_str().to_string(),
            source_span_id: Some(event.source_span_id.clone()),
            source_line_start: Some(event.source_line_start),
            source_line_end: Some(event.source_line_end),
        };

        connection
            .insert_recorder_event(&event.event_id, &event_input)
            .map_err(|error| RecorderImportError {
                code: RecorderImportErrorCode::DatabaseError,
                message: format!("Failed to insert recorder event: {error}"),
                repair: "Check database connectivity.".to_string(),
                details: Box::new(json!({
                    "eventId": event.event_id,
                    "sequence": event.sequence,
                    "dbError": error.to_string()
                })),
            })?;
    }

    let ended_at = Utc::now().to_rfc3339();

    Ok(RecorderImportResult {
        schema: RECORDER_IMPORT_RESULT_SCHEMA_V1,
        source_type: plan.source_type,
        source_id: plan.source_id,
        run_id: plan.run_id,
        agent_id: plan.agent_id,
        workspace_id: plan.workspace_id,
        dry_run: false,
        events_imported: plan.events_mapped,
        events_rejected: plan.events_rejected,
        payload_bytes: plan.payload_bytes,
        redacted_count: plan.redacted_count,
        chain_complete: plan.chain_complete,
        started_at,
        ended_at,
        warnings: plan.warnings,
    })
}

// ============================================================================
// Persistence helpers (EE-ibw4)
//
// These wrap the in-memory `start_recording` / `record_event` / `finish_recording`
// logic with calls to the persisted recorder store (V027 schema). They open a
// `&DbConnection` that the caller has already acquired and convert any DB error
// into a `DomainError::Storage`.
//
// The CLI handlers in `src/cli/mod.rs` call these helpers directly. We expose
// a stable surface here so the CLI side stays thin.
// ============================================================================

/// Persist a new recorder run row alongside the in-memory start report.
///
/// The caller has already opened a connection (typically against the workspace's
/// `.ee/ee.db`) and validated workspace existence. We do not persist anything
/// when `options.dry_run` is true.
pub fn start_and_persist_recording(
    conn: &crate::db::DbConnection,
    options: &RecorderStartOptions,
) -> Result<RecorderStartReport, crate::models::DomainError> {
    let report = start_recording(options);

    if !options.dry_run {
        let input = crate::db::CreateRecorderRunInput {
            workspace_id: options.workspace_id.clone(),
            agent_id: report.agent_id.clone(),
            session_id: options.session_id.clone(),
            // Live recordings use the DB CHECK alias 'live'; the ImportSourceType
            // enum models the import-side connectors (cass/eidetic_legacy/recorder/manual)
            // and does not carry a 'live' variant.
            source_type: "live".to_owned(),
            source_id: None,
            status: crate::models::RecorderRunStatus::Active.as_str().to_owned(),
            started_at: report.started_at.clone(),
            ended_at: None,
            event_count: 0,
            redacted_count: 0,
            payload_bytes: 0,
            chain_complete: true,
        };
        conn.insert_recorder_run(&report.run_id, &input)
            .map_err(|error| crate::models::DomainError::Storage {
                message: format!("Failed to persist recorder run: {error}"),
                repair: Some("ee status --json".to_owned()),
            })?;
    }

    Ok(report)
}

/// Persist a recorder event row, deriving sequence + previous-hash from the
/// already-persisted events for the same run.
pub fn record_and_persist_event(
    conn: &crate::db::DbConnection,
    options: &RecorderEventOptions,
) -> Result<RecorderEventReport, RecordPersistedEventError> {
    validate_run_id_token(&options.run_id).map_err(RecordPersistedEventError::InvalidRunId)?;

    let mut recorder_error = None;
    let report = conn
        .with_transaction(|| {
            if conn.get_recorder_run(&options.run_id)?.is_none() {
                recorder_error = Some(RecordPersistedEventError::RunNotFound(
                    options.run_id.clone(),
                ));
                return Err(crate::db::DbError::MalformedRow {
                    operation: crate::db::DbOperation::Query,
                    message: "recorder run not found".to_owned(),
                });
            }
            let existing = conn.list_recorder_events(&options.run_id)?;
            let sequence = u64::try_from(existing.len()).unwrap_or(u64::MAX) + 1;
            let previous_event_hash = existing.last().map(|event| event.event_hash.clone());

            // Honor any explicitly-provided previous-hash by validating it agrees with the
            // tail of the persisted chain. Mismatch is a serious error: surface it.
            if let (Some(provided), Some(actual)) = (
                options.previous_event_hash.as_deref(),
                previous_event_hash.as_deref(),
            ) {
                if provided != actual {
                    recorder_error = Some(RecordPersistedEventError::ChainMismatch {
                        expected: actual.to_owned(),
                        provided: provided.to_owned(),
                    });
                    return Err(crate::db::DbError::MalformedRow {
                        operation: crate::db::DbOperation::Query,
                        message: "recorder previous_event_hash mismatch".to_owned(),
                    });
                }
            }

            let opts_with_chain = RecorderEventOptions {
                previous_event_hash: previous_event_hash.clone(),
                ..options.clone()
            };
            let report = match record_event(&opts_with_chain, sequence) {
                Ok(report) => report,
                Err(error) => {
                    recorder_error = Some(RecordPersistedEventError::Validation(error));
                    return Err(crate::db::DbError::MalformedRow {
                        operation: crate::db::DbOperation::Query,
                        message: "recorder event validation failed".to_owned(),
                    });
                }
            };

            if !report.dry_run {
                let input = crate::db::CreateRecorderEventInput {
                    run_id: report.run_id.clone(),
                    sequence: report.sequence,
                    event_type: report.event_type.as_str().to_owned(),
                    timestamp: report.timestamp.clone(),
                    payload_hash: report.payload_hash.clone(),
                    payload_bytes: report.payload_bytes,
                    redaction_status: report.redaction_status.as_db_str().to_owned(),
                    redacted_bytes: report.redacted_bytes,
                    previous_event_hash: report.previous_event_hash.clone(),
                    event_hash: report.event_hash.clone(),
                    chain_status: report.chain_status.as_str().to_owned(),
                    source_span_id: None,
                    source_line_start: None,
                    source_line_end: None,
                };
                conn.insert_recorder_event(&report.event_id, &input)?;
            }

            Ok(report)
        })
        .map_err(|error| {
            recorder_error.unwrap_or_else(|| RecordPersistedEventError::Storage {
                message: format!("Failed to persist recorder event transaction: {error}"),
            })
        })?;

    Ok(report)
}

/// Mark a persisted recorder run as finished and stamp its end timestamp +
/// rolled-up event count. Returns the corresponding `RecorderFinishReport`.
pub fn finish_and_persist_recording(
    conn: &crate::db::DbConnection,
    options: &RecorderFinishOptions,
) -> Result<RecorderFinishReport, crate::models::DomainError> {
    validate_run_id_token(&options.run_id).map_err(|message| {
        crate::models::DomainError::Usage {
            message,
            repair: Some(
                "Pass a valid run id (e.g. run_<uuid>) returned by `ee recorder start`.".to_owned(),
            ),
        }
    })?;

    if conn
        .get_recorder_run(&options.run_id)
        .map_err(|error| crate::models::DomainError::Storage {
            message: format!("Failed to read recorder run {}: {error}", options.run_id),
            repair: Some("ee status --json".to_owned()),
        })?
        .is_none()
    {
        return Err(crate::models::DomainError::NotFound {
            resource: "recorder run".to_owned(),
            id: options.run_id.clone(),
            repair: Some(
                "Start a run with `ee recorder start --json` before finishing it.".to_owned(),
            ),
        });
    }

    let stored_events = conn
        .list_recorder_events(&options.run_id)
        .map_err(|error| crate::models::DomainError::Storage {
            message: format!(
                "Failed to read recorder events for run {}: {error}",
                options.run_id
            ),
            repair: Some("ee status --json".to_owned()),
        })?;
    let event_count = u64::try_from(stored_events.len()).unwrap_or(u64::MAX);
    let payload_bytes: u64 = stored_events.iter().map(|e| e.payload_bytes).sum();
    let redacted_count: u64 = stored_events
        .iter()
        .filter(|e| e.redaction_status != "clean")
        .count() as u64;
    let chain_complete = stored_events.iter().all(|e| e.chain_status != "broken");

    let report = finish_recording(options, event_count);

    if !options.dry_run {
        let sql = format!(
            "UPDATE recorder_runs SET status = '{status}', ended_at = '{ended}', event_count = {events}, payload_bytes = {payload}, redacted_count = {redacted}, chain_complete = {chain} WHERE run_id = '{run}'",
            status = options.status.as_str(),
            ended = report.ended_at,
            events = event_count,
            payload = payload_bytes,
            redacted = redacted_count,
            chain = i64::from(chain_complete),
            run = options.run_id,
        );
        conn.execute_raw(&sql)
            .map_err(|error| crate::models::DomainError::Storage {
                message: format!("Failed to mark recorder run finished: {error}"),
                repair: Some("ee status --json".to_owned()),
            })?;
    }

    Ok(report)
}

/// Validate that `run_id` matches the constraint enforced by the recorder_runs
/// CHECK clause: `GLOB 'run_*' AND length >= 8`, ASCII alphanumerics + `_-`. We
/// reject anything else so the inline-string SQL in `finish_and_persist_recording`
/// cannot be coaxed into injection.
fn validate_run_id_token(run_id: &str) -> Result<(), String> {
    if !run_id.starts_with("run_") {
        return Err(format!(
            "Invalid recorder run id `{run_id}`: must start with `run_`"
        ));
    }
    if run_id.len() < 8 || run_id.len() > 80 {
        return Err(format!(
            "Invalid recorder run id `{run_id}`: length out of range (8..=80)"
        ));
    }
    if !run_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid recorder run id `{run_id}`: only [A-Za-z0-9_-] allowed"
        ));
    }
    Ok(())
}

/// Errors returned by [`record_and_persist_event`].
#[derive(Debug)]
pub enum RecordPersistedEventError {
    /// The supplied run id did not match the recorder_runs CHECK constraint.
    InvalidRunId(String),
    /// The supplied run id was syntactically valid but absent from the store.
    RunNotFound(String),
    /// `record_event` rejected the inputs (usually payload too large).
    Validation(RecorderEventError),
    /// The caller asserted a previous-event-hash that disagrees with persisted chain.
    ChainMismatch { expected: String, provided: String },
    /// The underlying database operation failed.
    Storage { message: String },
}

impl std::fmt::Display for RecordPersistedEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRunId(message) | Self::Storage { message } => f.write_str(message),
            Self::RunNotFound(run_id) => write!(f, "recorder run not found: {run_id}"),
            Self::Validation(error) => write!(f, "{error}"),
            Self::ChainMismatch { expected, provided } => write!(
                f,
                "previous_event_hash mismatch: expected {expected}, got {provided}"
            ),
        }
    }
}

impl std::error::Error for RecordPersistedEventError {}

// ============================================================================
// Events List
// ============================================================================

/// Options for listing recorder events.
#[derive(Clone, Debug, Default)]
pub struct RecorderEventsListOptions {
    /// Filter events after this RFC 3339 timestamp.
    pub since: Option<String>,
    /// Filter events by source type.
    pub source: Option<String>,
    /// Filter events by run ID.
    pub run_id: Option<String>,
    /// Maximum number of events to return.
    ///
    /// A zero value means "unbounded" and returns every matching event. This
    /// intentionally differs from `RecorderTailOptions`, where zero means an
    /// explicitly empty tail snapshot with `has_more` still computed.
    pub limit: u32,
}

/// A recorder event entry for listing.
#[derive(Clone, Debug)]
pub struct RecorderEventEntry {
    pub event_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event_type: String,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub payload_bytes: u64,
    pub redaction_status: String,
    pub event_hash: String,
    pub chain_status: String,
    pub created_at: String,
}

/// Report from listing recorder events.
#[derive(Clone, Debug)]
pub struct RecorderEventsListReport {
    pub schema: &'static str,
    pub events: Vec<RecorderEventEntry>,
    pub filters: RecorderEventsListOptions,
}

impl RecorderEventsListReport {
    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "recorder events list",
            "count": self.events.len(),
            "totalCount": self.events.len(),
            "filters": {
                "since": self.filters.since,
                "source": self.filters.source,
                "runId": self.filters.run_id,
                "limit": self.filters.limit,
            },
            "events": self.events.iter().map(|e| json!({
                "eventId": e.event_id,
                "runId": e.run_id,
                "sequence": e.sequence,
                "eventType": e.event_type,
                "timestamp": e.timestamp,
                "payloadHash": e.payload_hash,
                "payloadBytes": e.payload_bytes,
                "redactionStatus": e.redaction_status,
                "eventHash": e.event_hash,
                "chainStatus": e.chain_status,
                "createdAt": e.created_at,
            })).collect::<Vec<_>>()
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("Recorder Events\n");
        out.push_str(&format!("  Count: {}\n", self.events.len()));
        if let Some(ref since) = self.filters.since {
            out.push_str(&format!("  Since: {since}\n"));
        }
        if let Some(ref source) = self.filters.source {
            out.push_str(&format!("  Source: {source}\n"));
        }
        if let Some(ref run_id) = self.filters.run_id {
            out.push_str(&format!("  Run ID: {run_id}\n"));
        }
        out.push('\n');
        for event in &self.events {
            out.push_str(&format!(
                "  [{:>4}] {} {} ({})\n",
                event.sequence, event.timestamp, event.event_type, event.event_id
            ));
        }
        out
    }
}

/// List recorder events with optional filters.
pub fn list_recorder_events(
    conn: &crate::db::DbConnection,
    options: &RecorderEventsListOptions,
) -> Result<Vec<RecorderEventEntry>, crate::models::DomainError> {
    let stored_events = conn
        .list_recorder_events_filtered(
            options.run_id.as_deref(),
            options.since.as_deref(),
            options.source.as_deref(),
            options.limit,
        )
        .map_err(|e| crate::models::DomainError::Storage {
            message: format!("Failed to list recorder events: {e}"),
            repair: Some("ee status --json".to_string()),
        })?;

    Ok(stored_events
        .into_iter()
        .map(|e| RecorderEventEntry {
            event_id: e.event_id,
            run_id: e.run_id,
            sequence: e.sequence,
            event_type: e.event_type,
            timestamp: e.timestamp,
            payload_hash: e.payload_hash,
            payload_bytes: e.payload_bytes,
            redaction_status: e.redaction_status,
            event_hash: e.event_hash,
            chain_status: e.chain_status,
            created_at: e.created_at,
        })
        .collect())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    // SRR6.46.1 / bd-36bbk.1.1 — tailscale_metadata redaction class detection.
    // The detector is purely substring-based + case-insensitive; these tests
    // lock the field-name vocabulary the redactor will trip on so future
    // schema renames don't silently drop the class label.

    #[test]
    fn detected_redaction_classes_tags_tailscale_metadata_on_self_node_key() {
        let payload = r#"{"selfNodeKey":"nodekey:abcdef","other":"x"}"#;
        assert!(detected_redaction_classes(payload).contains(&"tailscale_metadata".to_string()));
    }

    #[test]
    fn detected_redaction_classes_tags_tailscale_metadata_on_tailnet_id() {
        let payload = r#"{"tailnetId":"tn_example","other":"x"}"#;
        assert!(detected_redaction_classes(payload).contains(&"tailscale_metadata".to_string()));
    }

    #[test]
    fn detected_redaction_classes_tags_tailscale_metadata_on_self_tailscale_ip() {
        let payload = r#"{"selfTailscaleIp":"100.64.0.5"}"#;
        assert!(detected_redaction_classes(payload).contains(&"tailscale_metadata".to_string()));
    }

    #[test]
    fn detected_redaction_classes_tags_tailscale_metadata_on_self_magic_dns_name() {
        let payload = r#"{"selfMagicDnsName":"alpha.tailnet"}"#;
        assert!(detected_redaction_classes(payload).contains(&"tailscale_metadata".to_string()));
    }

    #[test]
    fn detected_redaction_classes_tags_tailscale_metadata_on_binary_absolute_path() {
        let payload = r#"{"binaryAbsolutePath":"/opt/homebrew/bin/tailscale"}"#;
        assert!(detected_redaction_classes(payload).contains(&"tailscale_metadata".to_string()));
    }

    #[test]
    fn detected_redaction_classes_does_not_double_count_tailscale_metadata() {
        let payload = r#"{"selfNodeKey":"x","tailnetId":"y","selfTailscaleIp":"100.64.0.1"}"#;
        let classes = detected_redaction_classes(payload);
        let count = classes.iter().filter(|c| **c == "tailscale_metadata").count();
        assert_eq!(count, 1, "tailscale_metadata should appear exactly once; got {classes:?}");
    }

    #[test]
    fn detected_redaction_classes_does_not_tag_tailscale_metadata_when_absent() {
        let payload = r#"{"workspace":"foo","level":"procedural"}"#;
        assert!(!detected_redaction_classes(payload).contains(&"tailscale_metadata".to_string()));
    }

    #[test]
    fn detected_redaction_classes_returns_sorted_with_tailscale_metadata() {
        let payload = r#"{"selfNodeKey":"x","api_key":"secret","password":"hunter2"}"#;
        let classes = detected_redaction_classes(payload);
        // sort order must be stable; tailscale_metadata sits after the
        // existing classes alphabetically.
        let mut expected: Vec<String> = vec![
            "api_key".to_string(),
            "password".to_string(),
            "secret".to_string(),
            "tailscale_metadata".to_string(),
            "token".to_string(),
        ];
        expected.retain(|c| classes.contains(c));
        assert_eq!(
            classes.iter().filter(|c| expected.contains(c)).collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>(),
            "expected sorted intersect to match the natural order; got {classes:?}"
        );
    }

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn event_summary(
        run_id: &str,
        event_id: &str,
        sequence: u64,
        event_type: RecorderEventType,
        timestamp: &str,
        redacted: bool,
    ) -> RecorderEventSummary {
        RecorderEventSummary {
            event_id: event_id.to_owned(),
            run_id: run_id.to_owned(),
            sequence,
            event_type,
            timestamp: timestamp.to_owned(),
            redacted,
            redaction_status: if redacted { "redacted" } else { "clean" }.to_owned(),
            event_hash: format!("blake3:{event_id}"),
            chain_status: if sequence == 1 { "root" } else { "linked" }.to_owned(),
        }
    }

    #[test]
    fn start_schema_is_stable() -> TestResult {
        ensure(
            RECORDER_START_SCHEMA_V1,
            "ee.recorder.start.v1",
            "start schema",
        )
    }

    #[test]
    fn event_response_schema_is_stable() -> TestResult {
        ensure(
            RECORDER_EVENT_RESPONSE_SCHEMA_V1,
            "ee.recorder.event_response.v1",
            "event response schema",
        )
    }

    #[test]
    fn finish_schema_is_stable() -> TestResult {
        ensure(
            RECORDER_FINISH_SCHEMA_V1,
            "ee.recorder.finish.v1",
            "finish schema",
        )
    }

    #[test]
    fn tail_schema_is_stable() -> TestResult {
        ensure(
            RECORDER_TAIL_SCHEMA_V1,
            "ee.recorder.tail.v1",
            "tail schema",
        )
    }

    #[test]
    fn start_recording_creates_run_id() {
        let options = RecorderStartOptions {
            agent_id: "test-agent".to_string(),
            session_id: None,
            workspace_id: None,
            dry_run: false,
        };

        let report = start_recording(&options);

        assert!(report.run_id.starts_with("run_"));
        assert_eq!(report.agent_id, "test-agent");
        assert!(!report.dry_run);
    }

    #[test]
    fn start_recording_json_has_required_fields() {
        let options = RecorderStartOptions {
            agent_id: "agent-1".to_string(),
            session_id: Some("session-1".to_string()),
            workspace_id: None,
            dry_run: true,
        };

        let report = start_recording(&options);
        let json = report.data_json();

        assert_eq!(json["schema"], RECORDER_START_SCHEMA_V1);
        assert_eq!(json["command"], "recorder start");
        assert!(json["runId"].is_string());
        assert_eq!(json["agentId"], "agent-1");
        assert_eq!(json["sessionId"], "session-1");
        assert_eq!(json["dryRun"], true);
    }

    #[test]
    fn record_event_creates_event_id() -> TestResult {
        let options = RecorderEventOptions {
            run_id: "run_test".to_string(),
            event_type: RecorderEventType::ToolCall,
            payload: Some("test payload".to_string()),
            redact: false,
            previous_event_hash: None,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };

        let report = record_event(&options, 1).map_err(|error| error.to_string())?;

        assert!(report.event_id.starts_with("evt_"));
        assert_eq!(report.run_id, "run_test");
        assert_eq!(report.sequence, 1);
        assert!(report.payload_hash.is_some());
        assert!(report.event_hash.starts_with("blake3:"));
        assert_eq!(report.chain_status, RecorderEventChainStatus::Root);
        Ok(())
    }

    #[test]
    fn record_event_with_redaction() -> TestResult {
        let options = RecorderEventOptions {
            run_id: "run_test".to_string(),
            event_type: RecorderEventType::UserMessage,
            payload: Some("secret".to_string()),
            redact: true,
            previous_event_hash: None,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };

        let report = record_event(&options, 5).map_err(|error| error.to_string())?;

        assert_eq!(report.redaction_status, RedactionStatus::Full);
        assert_eq!(report.redaction_classes, vec!["manual".to_string()]);
        assert_eq!(report.redacted_bytes, 6);
        Ok(())
    }

    #[test]
    fn record_event_auto_redacts_sensitive_payload_before_hashing() -> TestResult {
        let options = RecorderEventOptions {
            run_id: "run_test".to_string(),
            event_type: RecorderEventType::UserMessage,
            payload: Some("password marker token marker".to_string()),
            redact: false,
            previous_event_hash: None,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };

        let report = record_event(&options, 1).map_err(|error| error.to_string())?;

        assert_eq!(report.redaction_status, RedactionStatus::Full);
        assert_eq!(
            report.redaction_classes,
            vec!["password".to_string(), "token".to_string()]
        );
        assert_eq!(report.placeholder_count, 2);
        assert_eq!(report.redacted_bytes, 28);
        assert!(report.payload_hash.is_some());
        Ok(())
    }

    #[test]
    fn record_event_rejects_oversized_payload() -> TestResult {
        let options = RecorderEventOptions {
            run_id: "run_test".to_string(),
            event_type: RecorderEventType::ToolCall,
            payload: Some("0123456789".to_string()),
            redact: false,
            previous_event_hash: None,
            max_payload_bytes: 4,
            dry_run: false,
        };

        let error = match record_event(&options, 1) {
            Ok(_) => return Err("oversized payload should fail".to_string()),
            Err(error) => error,
        };

        assert_eq!(error.code, RecorderEventRejectionCode::PayloadTooLarge);
        assert_eq!(error.payload_bytes, 10);
        assert_eq!(error.max_payload_bytes, 4);
        Ok(())
    }

    #[test]
    fn record_event_links_to_previous_hash() -> TestResult {
        let options = RecorderEventOptions {
            run_id: "run_test".to_string(),
            event_type: RecorderEventType::ToolResult,
            payload: Some("ok".to_string()),
            redact: false,
            previous_event_hash: Some("blake3:previous".to_string()),
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };

        let report = record_event(&options, 2).map_err(|error| error.to_string())?;

        assert_eq!(
            report.previous_event_hash,
            Some("blake3:previous".to_string())
        );
        assert_eq!(report.chain_status, RecorderEventChainStatus::Linked);
        assert!(report.event_hash.starts_with("blake3:"));
        Ok(())
    }

    #[test]
    fn recorder_import_plan_maps_cass_lines_deterministically() -> TestResult {
        let input = json!({
            "lines": [
                {
                    "line": 7,
                    "content": "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"format release\"}}"
                },
                {
                    "line": 8,
                    "content": "{\"type\":\"tool_use\",\"name\":\"shell\"}"
                },
                {
                    "line": 9,
                    "content": "{\"type\":\"tool_result\",\"content\":\"ok\"}"
                }
            ]
        });
        let options = RecorderImportOptions {
            source_type: ImportSourceType::Cass,
            source_id: "/sessions/cass-a.jsonl".to_string(),
            input_json: Some(input.to_string()),
            input_path: Some("cass-view.json".to_string()),
            agent_id: Some("codex".to_string()),
            session_id: Some("cass-session-a".to_string()),
            workspace_id: Some("workspace-a".to_string()),
            max_events: DEFAULT_RECORDER_IMPORT_LIMIT,
            redact: false,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: true,
        };

        let report = plan_recorder_import(&options).map_err(|error| error.to_string())?;

        ensure(report.schema, RECORDER_IMPORT_PLAN_SCHEMA_V1, "schema")?;
        ensure(report.events_discovered, 3, "events discovered")?;
        ensure(report.events_mapped, 3, "events mapped")?;
        ensure(report.agent_id, "codex".to_string(), "agent")?;
        ensure(
            report.events[0].event_type,
            RecorderEventType::UserMessage,
            "user event",
        )?;
        ensure(
            report.events[1].event_type,
            RecorderEventType::ToolCall,
            "tool call",
        )?;
        ensure(
            report.events[2].event_type,
            RecorderEventType::ToolResult,
            "tool result",
        )?;
        ensure(
            report.events[1].previous_event_hash.clone(),
            Some(report.events[0].event_hash.clone()),
            "hash chain",
        )?;
        ensure(report.chain_complete, true, "chain complete")
    }

    #[test]
    fn recorder_import_plan_can_force_redaction_without_echoing_payload() -> TestResult {
        let input = json!({
            "lines": [
                {"line": 1, "content": "ordinary transcript text"}
            ]
        });
        let options = RecorderImportOptions {
            source_type: ImportSourceType::Cass,
            source_id: "cass://session/redact".to_string(),
            input_json: Some(input.to_string()),
            input_path: None,
            agent_id: None,
            session_id: None,
            workspace_id: None,
            max_events: DEFAULT_RECORDER_IMPORT_LIMIT,
            redact: true,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: true,
        };

        let report = plan_recorder_import(&options).map_err(|error| error.to_string())?;
        let json = report.data_json().to_string();

        ensure(
            report.events[0].redaction_status,
            RedactionStatus::Full,
            "redacted",
        )?;
        ensure(
            report.events[0].redaction_classes.clone(),
            vec!["manual".to_string()],
            "manual class",
        )?;
        ensure(
            json.contains("ordinary transcript text"),
            false,
            "raw payload omitted",
        )
    }

    #[test]
    fn recorder_import_plan_is_always_dry_run() -> TestResult {
        let options = RecorderImportOptions {
            source_type: ImportSourceType::Cass,
            source_id: "cass://session/write".to_string(),
            input_json: None,
            input_path: None,
            agent_id: None,
            session_id: None,
            workspace_id: None,
            max_events: DEFAULT_RECORDER_IMPORT_LIMIT,
            redact: false,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };

        let plan = plan_recorder_import(&options).map_err(|error| error.message)?;

        ensure(plan.dry_run, true, "plan remains dry-run")
    }

    #[test]
    fn finish_recording_sets_status() {
        let options = RecorderFinishOptions {
            run_id: "run_test".to_string(),
            status: RecorderRunStatus::Completed,
            dry_run: false,
        };

        let report = finish_recording(&options, 10);

        assert_eq!(report.run_id, "run_test");
        assert_eq!(report.status, RecorderRunStatus::Completed);
        assert_eq!(report.event_count, 10);
    }

    #[test]
    fn tail_recording_without_store_returns_empty_snapshot() {
        let options = RecorderTailOptions {
            run_id: Some("run_test".to_string()),
            since: None,
            limit: 10,
            from_sequence: None,
            follow: false,
            filter: None,
        };

        let report = tail_recording(&options);

        assert_eq!(report.run_id.as_deref(), Some("run_test"));
        assert!(report.events.is_empty());
        assert_eq!(report.total_events, 0);
    }

    #[test]
    fn tail_recording_from_events_filters_sorts_and_limits() {
        let options = RecorderTailOptions {
            run_id: Some("run_test".to_string()),
            since: None,
            limit: 2,
            from_sequence: Some(1),
            follow: false,
            filter: None,
        };
        let events = vec![
            event_summary(
                "run_test",
                "evt_003",
                3,
                RecorderEventType::ToolResult,
                "2026-01-01T00:00:03Z",
                false,
            ),
            event_summary(
                "run_test",
                "evt_001",
                1,
                RecorderEventType::UserMessage,
                "2026-01-01T00:00:01Z",
                false,
            ),
            event_summary(
                "run_test",
                "evt_002",
                2,
                RecorderEventType::ToolCall,
                "2026-01-01T00:00:02Z",
                true,
            ),
            event_summary(
                "run_test",
                "evt_004",
                4,
                RecorderEventType::StateChange,
                "2026-01-01T00:00:04Z",
                false,
            ),
        ];

        let report = tail_recording_from_events(&options, &events);

        assert_eq!(report.total_events, 3);
        assert!(report.has_more);
        assert_eq!(report.events.len(), 2);
        assert_eq!(report.events[0].event_id, "evt_002");
        assert_eq!(report.events[1].event_id, "evt_003");
        assert!(report.events[0].redacted);
    }

    #[test]
    fn tail_recording_from_events_uses_from_sequence_as_exclusive_cursor() {
        let options = RecorderTailOptions {
            run_id: Some("run_test".to_string()),
            since: None,
            limit: 10,
            from_sequence: Some(2),
            follow: false,
            filter: None,
        };
        let events = vec![
            event_summary(
                "run_test",
                "evt_001",
                1,
                RecorderEventType::UserMessage,
                "2026-01-01T00:00:01Z",
                false,
            ),
            event_summary(
                "run_test",
                "evt_002",
                2,
                RecorderEventType::ToolCall,
                "2026-01-01T00:00:02Z",
                false,
            ),
            event_summary(
                "run_test",
                "evt_003",
                3,
                RecorderEventType::ToolResult,
                "2026-01-01T00:00:03Z",
                false,
            ),
        ];

        let report = tail_recording_from_events(&options, &events);

        assert_eq!(report.total_events, 1);
        assert!(!report.has_more);
        assert_eq!(report.events.len(), 1);
        assert_eq!(report.events[0].sequence, 3);
        assert_eq!(report.events[0].event_id, "evt_003");
    }

    #[test]
    fn tail_recording_since_filter_compares_rfc3339_offsets_by_instant() {
        let options = RecorderTailOptions {
            run_id: Some("run_test".to_string()),
            since: Some("2026-05-05T18:00:00Z".to_string()),
            limit: 10,
            from_sequence: None,
            follow: false,
            filter: None,
        };
        let events = vec![
            event_summary(
                "run_test",
                "evt_before",
                1,
                RecorderEventType::UserMessage,
                "2026-05-06T01:00:00+09:00",
                false,
            ),
            event_summary(
                "run_test",
                "evt_equal",
                2,
                RecorderEventType::ToolCall,
                "2026-05-06T03:00:00+09:00",
                false,
            ),
            event_summary(
                "run_test",
                "evt_after",
                3,
                RecorderEventType::ToolResult,
                "2026-05-05T18:00:01Z",
                false,
            ),
        ];

        let report = tail_recording_from_events(&options, &events);

        assert_eq!(report.total_events, 2);
        let mut event_ids = report
            .events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>();
        event_ids.sort_unstable();
        assert_eq!(event_ids, vec!["evt_after", "evt_equal"]);
    }

    #[test]
    fn tail_recording_since_filter_rejects_malformed_timestamps() {
        assert!(!timestamp_is_at_or_after(
            "2026-05-06 12:00:00",
            "2026-05-06T00:00:00Z"
        ));
        assert!(!timestamp_is_at_or_after(
            "2026-05-06T12:00:00Z",
            "2026-05-06 00:00:00"
        ));
    }

    #[test]
    fn tail_recording_from_events_with_limit_zero_returns_empty_with_has_more() {
        // Contract decision for eidetic_engine_cli-2nkb: an explicit
        // `--limit 0` on the tail surface honors the user's literal request
        // and returns zero events, but reports `has_more=true` whenever
        // any matching events exist so the caller can distinguish
        // "no data" from "you asked for nothing".
        let options = RecorderTailOptions {
            run_id: Some("run_test".to_string()),
            since: None,
            limit: 0,
            from_sequence: None,
            follow: false,
            filter: None,
        };
        let events = vec![
            event_summary(
                "run_test",
                "evt_001",
                1,
                RecorderEventType::ToolCall,
                "2026-01-01T00:00:01Z",
                false,
            ),
            event_summary(
                "run_test",
                "evt_002",
                2,
                RecorderEventType::ToolResult,
                "2026-01-01T00:00:02Z",
                false,
            ),
        ];

        let report = tail_recording_from_events(&options, &events);

        assert_eq!(report.events.len(), 0);
        assert_eq!(report.total_events, 2);
        assert!(report.has_more);
    }

    #[test]
    fn follow_event_schema_is_stable() -> TestResult {
        ensure(
            RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
            "ee.recorder.tail_follow_event.v1",
            "follow event schema",
        )
    }

    #[test]
    fn follow_event_to_jsonl_has_required_fields() {
        let event = RecorderTailFollowEvent {
            schema: RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
            run_id: "run_abc".to_string(),
            event_id: "evt_123".to_string(),
            sequence: 5,
            event_type: RecorderEventType::ToolCall,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            redacted: false,
            payload_preview: Some("preview".to_string()),
        };

        let jsonl = event.to_jsonl();

        assert!(jsonl.contains("\"schema\":\"ee.recorder.tail_follow_event.v1\""));
        assert!(jsonl.contains("\"runId\":\"run_abc\""));
        assert!(jsonl.contains("\"eventId\":\"evt_123\""));
        assert!(jsonl.contains("\"sequence\":5"));
        assert!(jsonl.contains("\"eventType\":\"tool_call\""));
        assert!(jsonl.contains("\"payloadPreview\":\"preview\""));
    }

    #[test]
    fn follow_event_to_jsonl_omits_null_preview() {
        let event = RecorderTailFollowEvent {
            schema: RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
            run_id: "run_abc".to_string(),
            event_id: "evt_123".to_string(),
            sequence: 1,
            event_type: RecorderEventType::UserMessage,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            redacted: true,
            payload_preview: None,
        };

        let jsonl = event.to_jsonl();

        assert!(!jsonl.contains("payloadPreview"));
        assert!(jsonl.contains("\"redacted\":true"));
    }

    #[test]
    fn poll_follow_events_reports_store_unavailable_without_snapshot() {
        let result = poll_follow_events("run_active_123", 0, 10);

        assert!(matches!(
            result,
            TailFollowResult::StoreUnavailable { run_id } if run_id == "run_active_123"
        ));
    }

    #[test]
    fn poll_follow_events_from_snapshot_returns_not_found_for_missing_run() {
        let result = poll_follow_events_from_snapshot(None, 0, 10);

        assert!(matches!(result, TailFollowResult::RunNotFound));
    }

    #[test]
    fn poll_follow_events_from_snapshot_returns_completed_for_finished_run() {
        let snapshot = RecorderFollowSnapshot {
            run_id: "run_completed_abc".to_string(),
            status: RecorderFollowRunStatus::Completed,
            events: vec![event_summary(
                "run_completed_abc",
                "evt_005",
                5,
                RecorderEventType::StateChange,
                "2026-01-01T00:00:05Z",
                false,
            )],
        };
        let result = poll_follow_events_from_snapshot(Some(&snapshot), 6, 10);

        assert!(matches!(
            result,
            TailFollowResult::RunCompleted { final_sequence: 5 }
        ));
    }

    #[test]
    fn poll_follow_events_from_snapshot_returns_waiting_for_active_run() {
        let snapshot = RecorderFollowSnapshot {
            run_id: "run_active_123".to_string(),
            status: RecorderFollowRunStatus::Active,
            events: Vec::new(),
        };
        let result = poll_follow_events_from_snapshot(Some(&snapshot), 0, 10);

        assert!(matches!(
            result,
            TailFollowResult::Waiting { last_sequence: 0 }
        ));
    }

    #[test]
    fn poll_follow_events_from_snapshot_returns_new_events() {
        let snapshot = RecorderFollowSnapshot {
            run_id: "run_active_123".to_string(),
            status: RecorderFollowRunStatus::Active,
            events: vec![event_summary(
                "run_active_123",
                "evt_002",
                2,
                RecorderEventType::ToolResult,
                "2026-01-01T00:00:02Z",
                true,
            )],
        };
        let result = poll_follow_events_from_snapshot(Some(&snapshot), 1, 10);

        assert!(matches!(
            result,
            TailFollowResult::Events(events)
                if events.len() == 1
                    && events[0].run_id == "run_active_123"
                    && events[0].event_id == "evt_002"
                    && events[0].redacted
        ));
    }

    #[test]
    fn follow_config_default_values() {
        let config = FollowConfig::default();

        assert_eq!(config.poll_interval_ms, 250);
        assert_eq!(config.max_backoff_ms, 2000);
        assert!((config.backoff_multiplier - 1.5).abs() < 0.01);
    }

    #[test]
    fn follow_diagnostic_returns_message_for_each_result() {
        let waiting = TailFollowResult::Waiting { last_sequence: 10 };
        let completed = TailFollowResult::RunCompleted { final_sequence: 5 };
        let not_found = TailFollowResult::RunNotFound;
        let unavailable = TailFollowResult::StoreUnavailable {
            run_id: "run".to_string(),
        };
        let events = TailFollowResult::Events(vec![RecorderTailFollowEvent {
            schema: RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
            run_id: "run".to_string(),
            event_id: "evt".to_string(),
            sequence: 1,
            event_type: RecorderEventType::ToolCall,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            redacted: false,
            payload_preview: None,
        }]);

        assert!(matches!(
            follow_diagnostic(&waiting),
            Some(message) if message.contains("waiting")
        ));
        assert!(matches!(
            follow_diagnostic(&completed),
            Some(message) if message.contains("completed")
        ));
        assert!(matches!(
            follow_diagnostic(&not_found),
            Some(message) if message.contains("not found")
        ));
        assert!(matches!(
            follow_diagnostic(&unavailable),
            Some(message) if message.contains("store unavailable")
        ));
        assert!(matches!(
            follow_diagnostic(&events),
            Some(message) if message.contains("1 event")
        ));
    }

    #[test]
    fn links_schema_is_stable() -> TestResult {
        ensure(
            RECORDER_LINKS_SCHEMA_V1,
            "ee.recorder.links.v1",
            "links schema",
        )
    }

    #[test]
    fn link_type_as_str() {
        assert_eq!(RecorderLinkType::ContextPack.as_str(), "context_pack");
        assert_eq!(RecorderLinkType::PreflightRun.as_str(), "preflight_run");
        assert_eq!(RecorderLinkType::Outcome.as_str(), "outcome");
        assert_eq!(RecorderLinkType::Tripwire.as_str(), "tripwire");
        assert_eq!(RecorderLinkType::TaskEpisode.as_str(), "task_episode");
    }

    #[test]
    fn add_link_plans_link_without_claiming_store_write() {
        let options = RecorderLinkAddOptions {
            run_id: "run_test".to_string(),
            link_type: RecorderLinkType::ContextPack,
            artifact_id: "pack_abc".to_string(),
            metadata: None,
            dry_run: false,
        };

        let report = add_link(&options);

        assert!(report.link.link_id.starts_with("link_"));
        assert_eq!(report.link.run_id, "run_test");
        assert_eq!(report.link.link_type, RecorderLinkType::ContextPack);
        assert_eq!(report.link.artifact_id, "pack_abc");
        assert!(report.dry_run);
    }

    #[test]
    fn list_links_returns_empty_without_persisted_records() {
        let options = RecorderLinksListOptions {
            run_id: Some("run_missing".to_string()),
            link_type: None,
            artifact_id: None,
            limit: 10,
        };

        let report = list_links(&options);

        assert!(report.links.is_empty());
        assert_eq!(report.total_count, 0);
    }

    #[test]
    fn list_links_from_records_filters_by_run_id_type_and_artifact() {
        let options = RecorderLinksListOptions {
            run_id: Some("run_sample".to_string()),
            link_type: Some(RecorderLinkType::ContextPack),
            artifact_id: Some("pack_abc123".to_string()),
            limit: 10,
        };
        let links = vec![
            RecorderLink {
                link_id: "link_late".to_string(),
                run_id: "run_sample".to_string(),
                link_type: RecorderLinkType::ContextPack,
                artifact_id: "pack_abc123".to_string(),
                created_at: "2026-01-01T00:00:02Z".to_string(),
                metadata: None,
            },
            RecorderLink {
                link_id: "link_other_type".to_string(),
                run_id: "run_sample".to_string(),
                link_type: RecorderLinkType::Outcome,
                artifact_id: "outcome_123".to_string(),
                created_at: "2026-01-01T00:00:01Z".to_string(),
                metadata: None,
            },
            RecorderLink {
                link_id: "link_early".to_string(),
                run_id: "run_sample".to_string(),
                link_type: RecorderLinkType::ContextPack,
                artifact_id: "pack_abc123".to_string(),
                created_at: "2026-01-01T00:00:01Z".to_string(),
                metadata: Some("selected".to_string()),
            },
            RecorderLink {
                link_id: "link_other_run".to_string(),
                run_id: "run_other".to_string(),
                link_type: RecorderLinkType::ContextPack,
                artifact_id: "pack_abc123".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                metadata: None,
            },
        ];

        let report = list_links_from_records(&options, &links);

        assert_eq!(report.total_count, 2);
        assert_eq!(report.links.len(), 2);
        assert_eq!(report.links[0].link_id, "link_early");
        assert_eq!(report.links[1].link_id, "link_late");
        assert!(
            report.links.iter().all(|link| link.run_id == "run_sample") // ubs:ignore - test fixture run ID assertion, not credential comparison.
        );
        assert!(
            report
                .links
                .iter()
                .all(|link| link.link_type == RecorderLinkType::ContextPack)
        );
    }

    #[test]
    fn import_result_schema_is_stable() -> TestResult {
        ensure(
            RECORDER_IMPORT_RESULT_SCHEMA_V1,
            "ee.recorder.import_result.v1",
            "import result schema",
        )
    }

    #[test]
    fn import_dry_run_plans_without_persisting() -> TestResult {
        let input = json!({
            "lines": [
                {"line": 1, "content": "test line"}
            ]
        });
        let options = RecorderImportOptions {
            source_type: ImportSourceType::Cass,
            source_id: "cass://dry-run-test".to_string(),
            input_json: Some(input.to_string()),
            input_path: None,
            agent_id: Some("test-agent".to_string()),
            session_id: None,
            workspace_id: None,
            max_events: DEFAULT_RECORDER_IMPORT_LIMIT,
            redact: false,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: true,
        };

        let plan = plan_recorder_import(&options).map_err(|e| e.message)?;

        ensure(plan.dry_run, true, "plan marked dry_run")?;
        ensure(plan.events_mapped, 1, "one event mapped")?;
        ensure(
            plan.events[0].action,
            "would_record",
            "action is would_record",
        )
    }

    #[test]
    fn import_execute_persists_to_database() -> TestResult {
        use crate::db::DbConnection;

        let connection = DbConnection::open_memory().map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;

        let input = json!({
            "lines": [
                {"line": 1, "content": "{\"type\":\"message\",\"role\":\"user\"}"},
                {"line": 2, "content": "{\"type\":\"tool_use\",\"name\":\"shell\"}"}
            ]
        });
        let options = RecorderImportOptions {
            source_type: ImportSourceType::Cass,
            source_id: "cass://execute-test".to_string(),
            input_json: Some(input.to_string()),
            input_path: Some("/sessions/test.jsonl".to_string()),
            agent_id: Some("test-agent".to_string()),
            session_id: Some("session-123".to_string()),
            workspace_id: None,
            max_events: DEFAULT_RECORDER_IMPORT_LIMIT,
            redact: false,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };

        let result = execute_recorder_import(&options, &connection).map_err(|e| e.message)?;

        ensure(result.dry_run, false, "result not dry_run")?;
        ensure(result.events_imported, 2, "two events imported")?;
        ensure(result.source_type, ImportSourceType::Cass, "source type")?;
        ensure(result.agent_id, "test-agent".to_string(), "agent_id")?;

        let stored_run = connection
            .get_recorder_run(&result.run_id)
            .map_err(|e| e.to_string())?
            .ok_or("run not found in database")?;
        ensure(
            stored_run.agent_id,
            "test-agent".to_string(),
            "stored agent",
        )?;
        ensure(stored_run.event_count, 2, "stored event_count")?;

        let stored_events = connection
            .list_recorder_events(&result.run_id)
            .map_err(|e| e.to_string())?;
        ensure(stored_events.len(), 2, "stored events count")?;
        ensure(stored_events[0].sequence, 1, "first event sequence")?;
        ensure(stored_events[1].sequence, 2, "second event sequence")
    }

    #[test]
    fn import_result_json_has_required_fields() -> TestResult {
        use crate::db::DbConnection;

        let connection = DbConnection::open_memory().map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;

        let input = json!({
            "lines": [
                {"line": 1, "content": "test content"}
            ]
        });
        let options = RecorderImportOptions {
            source_type: ImportSourceType::Cass,
            source_id: "cass://json-test".to_string(),
            input_json: Some(input.to_string()),
            input_path: None,
            agent_id: None,
            session_id: None,
            workspace_id: None,
            max_events: DEFAULT_RECORDER_IMPORT_LIMIT,
            redact: false,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };

        let result = execute_recorder_import(&options, &connection).map_err(|e| e.message)?;
        let json = result.data_json();

        ensure(
            json.get("schema").and_then(|v| v.as_str()),
            Some(RECORDER_IMPORT_RESULT_SCHEMA_V1),
            "schema field",
        )?;
        ensure(
            json.get("command").and_then(|v| v.as_str()),
            Some("recorder import"),
            "command field",
        )?;
        ensure(json.get("runId").is_some(), true, "runId present")?;
        ensure(json.get("sourceType").is_some(), true, "sourceType present")?;
        ensure(json.get("sourceId").is_some(), true, "sourceId present")?;
        ensure(json.get("agentId").is_some(), true, "agentId present")?;
        ensure(json.get("dryRun").is_some(), true, "dryRun present")?;
        ensure(
            json.get("eventsImported").is_some(),
            true,
            "eventsImported present",
        )?;
        ensure(
            json.get("eventsRejected").is_some(),
            true,
            "eventsRejected present",
        )?;
        ensure(
            json.get("payloadBytes").is_some(),
            true,
            "payloadBytes present",
        )?;
        ensure(
            json.get("chainComplete").is_some(),
            true,
            "chainComplete present",
        )?;
        ensure(json.get("startedAt").is_some(), true, "startedAt present")?;
        ensure(json.get("endedAt").is_some(), true, "endedAt present")?;
        ensure(json.get("warnings").is_some(), true, "warnings present")
    }

    /// Helper: import a tiny recorder run with two events so trigger
    /// tests have something to mutate.
    fn import_two_event_run(connection: &crate::db::DbConnection) -> Result<String, String> {
        let input = json!({
            "lines": [
                {"line": 1, "content": "{\"type\":\"message\",\"role\":\"user\"}"},
                {"line": 2, "content": "{\"type\":\"tool_use\",\"name\":\"shell\"}"}
            ]
        });
        let options = RecorderImportOptions {
            source_type: ImportSourceType::Cass,
            source_id: "cass://trigger-test".to_string(),
            input_json: Some(input.to_string()),
            input_path: Some("/sessions/trigger-test.jsonl".to_string()),
            agent_id: Some("trigger-test-agent".to_string()),
            session_id: Some("trigger-session".to_string()),
            workspace_id: None,
            max_events: DEFAULT_RECORDER_IMPORT_LIMIT,
            redact: false,
            max_payload_bytes: DEFAULT_MAX_RECORDER_PAYLOAD_BYTES,
            dry_run: false,
        };
        let result = execute_recorder_import(&options, connection).map_err(|e| e.message)?;
        Ok(result.run_id)
    }

    /// V036 / eidetic_engine_cli-is96 — append-only trigger on
    /// recorder_events blocks raw UPDATE attempts. Tampering with a
    /// persisted event would otherwise rewrite the chain hash basis
    /// silently between insert and the next chain-status pass.
    #[test]
    fn append_only_trigger_blocks_recorder_events_update() -> TestResult {
        use crate::db::DbConnection;
        let connection = DbConnection::open_memory().map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;

        let run_id = import_two_event_run(&connection)?;
        let stored = connection
            .list_recorder_events(&run_id)
            .map_err(|e| e.to_string())?;
        ensure(stored.len(), 2, "two events persisted")?;

        let outcome = connection.execute_raw(
            "UPDATE recorder_events SET event_type = 'error' WHERE event_type IN ('user_message','tool_call')",
        );
        let error = match outcome {
            Ok(()) => return Err("trigger should reject UPDATE on recorder_events".to_string()),
            Err(error) => error,
        };
        let message = error.to_string().to_lowercase();
        ensure(
            message.contains("recorder_events") && message.contains("append-only"),
            true,
            "trigger error mentions recorder_events + append-only",
        )?;

        // The block must leave the events untouched.
        let after = connection
            .list_recorder_events(&run_id)
            .map_err(|e| e.to_string())?;
        ensure(after.len(), 2, "events still present after blocked UPDATE")?;
        ensure(
            after
                .iter()
                .all(|event| !matches!(event.event_type.as_str(), "error")),
            true,
            "no event was rewritten to 'error'",
        )
    }

    /// V036 / eidetic_engine_cli-is96 — DELETE on recorder_events is NOT
    /// blocked, because recorder_runs uses ON DELETE CASCADE. Deleting a
    /// run must still cascade-delete its events.
    #[test]
    fn direct_delete_on_recorder_events_succeeds_and_cascade_still_works() -> TestResult {
        use crate::db::DbConnection;
        let connection = DbConnection::open_memory().map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;

        let run_id = import_two_event_run(&connection)?;
        let before = connection
            .list_recorder_events(&run_id)
            .map_err(|e| e.to_string())?;
        ensure(before.len(), 2, "two events persisted before direct delete")?;
        let deleted_event_id = before[0].event_id.clone();

        connection
            .execute_raw(&format!(
                "DELETE FROM recorder_events WHERE event_id = '{deleted_event_id}'"
            ))
            .map_err(|e| format!("direct recorder_events DELETE must remain permitted: {e}"))?;

        let after_direct_delete = connection
            .list_recorder_events(&run_id)
            .map_err(|e| e.to_string())?;
        ensure(
            after_direct_delete.len(),
            1,
            "one event remains after direct event delete",
        )?;
        ensure(
            after_direct_delete
                .iter()
                .any(|event| event.event_id == deleted_event_id),
            false,
            "direct event delete removed the targeted event",
        )?;
        ensure(
            connection
                .get_recorder_run(&run_id)
                .map_err(|e| e.to_string())?
                .is_some(),
            true,
            "parent run remains after direct event delete",
        )?;

        connection
            .execute_raw("PRAGMA foreign_keys = ON")
            .map_err(|e| e.to_string())?;
        connection
            .execute_raw(&format!(
                "DELETE FROM recorder_runs WHERE run_id = '{run_id}'"
            ))
            .map_err(|e| {
                format!("recorder_runs DELETE must still cascade after direct delete: {e}")
            })?;

        let after_parent_delete = connection
            .list_recorder_events(&run_id)
            .map_err(|e| e.to_string())?;
        ensure(
            after_parent_delete.is_empty(),
            true,
            "remaining recorder_events still cascade-delete with parent run",
        )
    }

    #[test]
    fn deleting_recorder_run_still_cascades_to_events() -> TestResult {
        use crate::db::DbConnection;
        let connection = DbConnection::open_memory().map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;

        let run_id = import_two_event_run(&connection)?;
        let before = connection
            .list_recorder_events(&run_id)
            .map_err(|e| e.to_string())?;
        ensure(before.len(), 2, "two events persisted before cascade")?;

        // Foreign keys may not be on by default in an in-memory connection;
        // re-enable them explicitly so the cascade fires (matches the
        // production open_file pragmas).
        connection
            .execute_raw("PRAGMA foreign_keys = ON")
            .map_err(|e| e.to_string())?;
        connection
            .execute_raw(&format!(
                "DELETE FROM recorder_runs WHERE run_id = '{run_id}'"
            ))
            .map_err(|e| {
                format!("recorder_runs DELETE must succeed despite append-only trigger: {e}")
            })?;

        let after = connection
            .list_recorder_events(&run_id)
            .map_err(|e| e.to_string())?;
        ensure(
            after.is_empty(),
            true,
            "recorder_events cascade-deleted with parent run",
        )
    }
}
