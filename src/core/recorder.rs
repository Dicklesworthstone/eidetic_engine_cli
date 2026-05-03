//! Recorder subsystem for tracking agent recording sessions and events (EE-401).
//!
//! Provides append-only recording of agent activity for outcomes,
//! preflight feedback, replay, procedure distillation, and causal credit.

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
        if let Some(ref session_id) = self.session_id {
            obj["sessionId"] = json!(session_id);
        }
        if let Some(ref workspace_id) = self.workspace_id {
            obj["workspaceId"] = json!(workspace_id);
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
        if let Some(ref hash) = self.payload_hash {
            obj["payloadHash"] = json!(hash);
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
    if !options.dry_run {
        return Err(RecorderImportError {
            code: RecorderImportErrorCode::DryRunRequired,
            message: "Recorder import writes are not implemented; use --dry-run.".to_string(),
            repair: "Re-run with ee recorder import --dry-run --json.".to_string(),
            details: Box::new(json!({"dryRun": options.dry_run})),
        });
    }

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
    /// Run ID to tail.
    pub run_id: String,
    /// Number of events to return.
    pub limit: u32,
    /// Starting sequence number.
    pub from_sequence: Option<u64>,
    /// Follow mode: continuously poll for new events.
    pub follow: bool,
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
        if let Some(ref preview) = self.payload_preview {
            obj["payloadPreview"] = json!(preview);
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
    pub run_id: String,
    pub events: Vec<RecorderEventSummary>,
    pub total_events: u64,
    pub has_more: bool,
}

/// Summary of a recorded event for tail output.
#[derive(Clone, Debug)]
pub struct RecorderEventSummary {
    pub event_id: String,
    pub sequence: u64,
    pub event_type: RecorderEventType,
    pub timestamp: String,
    pub redacted: bool,
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
                "sequence": e.sequence,
                "eventType": e.event_type.as_str(),
                "timestamp": e.timestamp,
                "redacted": e.redacted,
            })).collect::<Vec<_>>(),
            "totalEvents": self.total_events,
            "hasMore": self.has_more,
        })
    }

    /// Render as human-readable string.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str(&format!("Recording Tail: {}\n", self.run_id));
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
    let from_sequence = options.from_sequence.unwrap_or(0);
    let mut matching = events
        .iter()
        .filter(|event| event.sequence >= from_sequence)
        .cloned()
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });

    let total_events = usize_to_u64(matching.len());
    let limit = usize::try_from(options.limit).unwrap_or(usize::MAX);
    let has_more = matching.len() > limit;
    matching.truncate(limit);

    RecorderTailReport {
        schema: RECORDER_TAIL_SCHEMA_V1,
        run_id: options.run_id.clone(),
        events: matching,
        total_events,
        has_more,
    }
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
            run_id: snapshot.run_id.clone(),
            limit,
            from_sequence: Some(from_sequence),
            follow: true,
        },
        &snapshot.events,
    );

    if !tail.events.is_empty() {
        let events = tail
            .events
            .into_iter()
            .map(|event| RecorderTailFollowEvent {
                schema: RECORDER_TAIL_FOLLOW_EVENT_SCHEMA_V1,
                run_id: snapshot.run_id.clone(),
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
        if let Some(ref meta) = self.metadata {
            obj["metadata"] = json!(meta);
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
            options.run_id.as_ref().is_none_or(|r| &l.run_id == r)
                && options.link_type.is_none_or(|t| l.link_type == t)
                && options
                    .artifact_id
                    .as_ref()
                    .is_none_or(|a| &l.artifact_id == a)
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
// Tests
// ============================================================================

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
    fn recorder_import_plan_requires_dry_run() -> TestResult {
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

        let error = match plan_recorder_import(&options) {
            Ok(_) => return Err("non-dry-run recorder import should fail".to_string()),
            Err(error) => error,
        };

        ensure(
            error.code,
            RecorderImportErrorCode::DryRunRequired,
            "error code",
        )
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
            run_id: "run_test".to_string(),
            limit: 10,
            from_sequence: None,
            follow: false,
        };

        let report = tail_recording(&options);

        assert_eq!(report.run_id, "run_test");
        assert!(report.events.is_empty());
        assert_eq!(report.total_events, 0);
    }

    #[test]
    fn tail_recording_from_events_filters_sorts_and_limits() {
        let options = RecorderTailOptions {
            run_id: "run_test".to_string(),
            limit: 2,
            from_sequence: Some(2),
            follow: false,
        };
        let events = vec![
            RecorderEventSummary {
                event_id: "evt_003".to_string(),
                sequence: 3,
                event_type: RecorderEventType::ToolResult,
                timestamp: "2026-01-01T00:00:03Z".to_string(),
                redacted: false,
            },
            RecorderEventSummary {
                event_id: "evt_001".to_string(),
                sequence: 1,
                event_type: RecorderEventType::UserMessage,
                timestamp: "2026-01-01T00:00:01Z".to_string(),
                redacted: false,
            },
            RecorderEventSummary {
                event_id: "evt_002".to_string(),
                sequence: 2,
                event_type: RecorderEventType::ToolCall,
                timestamp: "2026-01-01T00:00:02Z".to_string(),
                redacted: true,
            },
            RecorderEventSummary {
                event_id: "evt_004".to_string(),
                sequence: 4,
                event_type: RecorderEventType::StateChange,
                timestamp: "2026-01-01T00:00:04Z".to_string(),
                redacted: false,
            },
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
            events: vec![RecorderEventSummary {
                event_id: "evt_005".to_string(),
                sequence: 5,
                event_type: RecorderEventType::StateChange,
                timestamp: "2026-01-01T00:00:05Z".to_string(),
                redacted: false,
            }],
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
            events: vec![RecorderEventSummary {
                event_id: "evt_002".to_string(),
                sequence: 2,
                event_type: RecorderEventType::ToolResult,
                timestamp: "2026-01-01T00:00:02Z".to_string(),
                redacted: true,
            }],
        };
        let result = poll_follow_events_from_snapshot(Some(&snapshot), 2, 10);

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
        assert!(report.links.iter().all(|link| link.run_id == "run_sample"));
        assert!(
            report
                .links
                .iter()
                .all(|link| link.link_type == RecorderLinkType::ContextPack)
        );
    }
}
