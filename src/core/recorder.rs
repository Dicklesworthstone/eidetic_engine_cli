//! Recorder subsystem for tracking agent recording sessions and events (EE-401).
//!
//! Provides append-only recording of agent activity for outcomes,
//! preflight feedback, replay, procedure distillation, and causal credit.

use serde_json::{Value as JsonValue, json};

use crate::models::{RecorderEventType, RecorderRunStatus, RedactionStatus};

/// Schema for recorder start response.
pub const RECORDER_START_SCHEMA_V1: &str = "ee.recorder.start.v1";

/// Schema for recorder event response.
pub const RECORDER_EVENT_RESPONSE_SCHEMA_V1: &str = "ee.recorder.event_response.v1";

/// Schema for recorder finish response.
pub const RECORDER_FINISH_SCHEMA_V1: &str = "ee.recorder.finish.v1";

/// Schema for recorder tail response.
pub const RECORDER_TAIL_SCHEMA_V1: &str = "ee.recorder.tail.v1";

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
    pub redaction_status: RedactionStatus,
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
            "redactionStatus": self.redaction_status.as_str(),
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
        out.push_str(&format!("Redacted: {}\n", self.redaction_status));
        out
    }
}

/// Record an event to a recording session.
#[must_use]
pub fn record_event(options: &RecorderEventOptions, sequence: u64) -> RecorderEventReport {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let event_id = format!("evt_{}", uuid::Uuid::now_v7());

    let payload_hash = options.payload.as_ref().map(|p| {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(p.as_bytes());
        format!("{:x}", hasher.finalize())
    });

    let redaction_status = if options.redact {
        RedactionStatus::Full
    } else {
        RedactionStatus::None
    };

    RecorderEventReport {
        schema: RECORDER_EVENT_RESPONSE_SCHEMA_V1,
        event_id,
        run_id: options.run_id.clone(),
        sequence,
        event_type: options.event_type,
        timestamp,
        payload_hash,
        redaction_status,
        dry_run: options.dry_run,
    }
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

/// Tail events from a recording session (stub - returns empty for now).
#[must_use]
pub fn tail_recording(options: &RecorderTailOptions) -> RecorderTailReport {
    RecorderTailReport {
        schema: RECORDER_TAIL_SCHEMA_V1,
        run_id: options.run_id.clone(),
        events: Vec::new(),
        total_events: 0,
        has_more: false,
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

/// Add a link between a recorder run and an artifact.
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
        dry_run: options.dry_run,
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

/// List links for a recorder run or artifact.
#[must_use]
pub fn list_links(options: &RecorderLinksListOptions) -> RecorderLinksListReport {
    let now = chrono::Utc::now().to_rfc3339();

    let sample_links = vec![
        RecorderLink {
            link_id: "link_sample1".to_owned(),
            run_id: options
                .run_id
                .clone()
                .unwrap_or_else(|| "run_sample".to_owned()),
            link_type: RecorderLinkType::ContextPack,
            artifact_id: "pack_abc123".to_owned(),
            created_at: now.clone(),
            metadata: None,
        },
        RecorderLink {
            link_id: "link_sample2".to_owned(),
            run_id: options
                .run_id
                .clone()
                .unwrap_or_else(|| "run_sample".to_owned()),
            link_type: RecorderLinkType::Outcome,
            artifact_id: "outcome_def456".to_owned(),
            created_at: now,
            metadata: Some("success".to_owned()),
        },
    ];

    let filtered: Vec<_> = sample_links
        .into_iter()
        .filter(|l| {
            options.run_id.as_ref().is_none_or(|r| &l.run_id == r)
                && options.link_type.is_none_or(|t| l.link_type == t)
                && options
                    .artifact_id
                    .as_ref()
                    .is_none_or(|a| &l.artifact_id == a)
        })
        .take(options.limit as usize)
        .collect();

    RecorderLinksListReport {
        schema: RECORDER_LINKS_SCHEMA_V1,
        total_count: filtered.len() as u32,
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
    fn record_event_creates_event_id() {
        let options = RecorderEventOptions {
            run_id: "run_test".to_string(),
            event_type: RecorderEventType::ToolCall,
            payload: Some("test payload".to_string()),
            redact: false,
            dry_run: false,
        };

        let report = record_event(&options, 1);

        assert!(report.event_id.starts_with("evt_"));
        assert_eq!(report.run_id, "run_test");
        assert_eq!(report.sequence, 1);
        assert!(report.payload_hash.is_some());
    }

    #[test]
    fn record_event_with_redaction() {
        let options = RecorderEventOptions {
            run_id: "run_test".to_string(),
            event_type: RecorderEventType::UserMessage,
            payload: Some("secret".to_string()),
            redact: true,
            dry_run: false,
        };

        let report = record_event(&options, 5);

        assert_eq!(report.redaction_status, RedactionStatus::Full);
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
    fn tail_recording_returns_empty_stub() {
        let options = RecorderTailOptions {
            run_id: "run_test".to_string(),
            limit: 10,
            from_sequence: None,
        };

        let report = tail_recording(&options);

        assert_eq!(report.run_id, "run_test");
        assert!(report.events.is_empty());
        assert_eq!(report.total_events, 0);
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
    fn add_link_creates_link_id() {
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
    }

    #[test]
    fn list_links_filters_by_run_id() {
        let options = RecorderLinksListOptions {
            run_id: Some("run_sample".to_string()),
            link_type: None,
            artifact_id: None,
            limit: 10,
        };

        let report = list_links(&options);

        assert!(report.links.iter().all(|l| l.run_id == "run_sample"));
    }

    #[test]
    fn list_links_filters_by_type() {
        let options = RecorderLinksListOptions {
            run_id: None,
            link_type: Some(RecorderLinkType::ContextPack),
            artifact_id: None,
            limit: 10,
        };

        let report = list_links(&options);

        assert!(
            report
                .links
                .iter()
                .all(|l| l.link_type == RecorderLinkType::ContextPack)
        );
    }
}
