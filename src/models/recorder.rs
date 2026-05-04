//! Recorder schemas for tracking agent recording sessions and events (EE-400).
//!
//! Defines the schema contracts for:
//! - Recording runs (sessions of recorded agent activity)
//! - Events within a recording run
//! - Event payloads (tool calls, outputs, etc.)
//! - Redaction status for privacy-sensitive data
//! - Import cursors for incremental import
//! - Safe rationale traces that capture visible reasoning artifacts without
//!   storing private chain-of-thought

use std::fmt;
use std::str::FromStr;

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for recorder run metadata.
pub const RECORDER_RUN_SCHEMA_V1: &str = "ee.recorder.run.v1";

/// Schema for individual recorder events.
pub const RECORDER_EVENT_SCHEMA_V1: &str = "ee.recorder.event.v1";

/// Schema for event payloads.
pub const RECORDER_PAYLOAD_SCHEMA_V1: &str = "ee.recorder.payload.v1";

/// Schema for redaction status tracking.
pub const REDACTION_STATUS_SCHEMA_V1: &str = "ee.redaction.status.v1";

/// Schema for import cursor state.
pub const IMPORT_CURSOR_SCHEMA_V1: &str = "ee.import.cursor.v1";

/// Schema for recorder import dry-run plans.
pub const RECORDER_IMPORT_PLAN_SCHEMA_V1: &str = "ee.recorder.import_plan.v1";

/// Schema for safe rationale traces attached to recorder/evidence artifacts.
pub const RATIONALE_TRACE_SCHEMA_V1: &str = "ee.rationale_trace.v1";

/// Schema for the recorder schema catalog.
pub const RECORDER_SCHEMA_CATALOG_V1: &str = "ee.recorder.schemas.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

// ============================================================================
// Recorder Run
// ============================================================================

/// Status of a recording run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderRunStatus {
    Active,
    Completed,
    Abandoned,
    Imported,
}

impl RecorderRunStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
            Self::Imported => "imported",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Abandoned | Self::Imported)
    }
}

impl fmt::Display for RecorderRunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRecorderRunStatusError(String);

impl fmt::Display for ParseRecorderRunStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid recorder run status: {}", self.0)
    }
}

impl std::error::Error for ParseRecorderRunStatusError {}

impl FromStr for RecorderRunStatus {
    type Err = ParseRecorderRunStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "abandoned" => Ok(Self::Abandoned),
            "imported" => Ok(Self::Imported),
            _ => Err(ParseRecorderRunStatusError(s.to_string())),
        }
    }
}

/// Metadata for a recording run.
#[derive(Clone, Debug, PartialEq)]
pub struct RecorderRunMeta {
    pub schema: &'static str,
    pub run_id: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    pub status: RecorderRunStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub event_count: u64,
    pub redacted_count: u64,
}

impl RecorderRunMeta {
    #[must_use]
    pub fn new(
        run_id: impl Into<String>,
        agent_id: impl Into<String>,
        started_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: RECORDER_RUN_SCHEMA_V1,
            run_id: run_id.into(),
            agent_id: agent_id.into(),
            session_id: None,
            workspace_id: None,
            status: RecorderRunStatus::Active,
            started_at: started_at.into(),
            ended_at: None,
            event_count: 0,
            redacted_count: 0,
        }
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    #[must_use]
    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    #[must_use]
    pub fn with_status(mut self, status: RecorderRunStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub fn finished(mut self, status: RecorderRunStatus, ended_at: impl Into<String>) -> Self {
        self.status = status;
        self.ended_at = Some(ended_at.into());
        self
    }

    #[must_use]
    pub const fn with_event_counts(mut self, event_count: u64, redacted_count: u64) -> Self {
        self.event_count = event_count;
        self.redacted_count = redacted_count;
        self
    }
}

// ============================================================================
// Recorder Event
// ============================================================================

/// Type of recorder event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderEventType {
    ToolCall,
    ToolResult,
    UserMessage,
    AssistantMessage,
    SystemMessage,
    Error,
    StateChange,
}

impl RecorderEventType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::SystemMessage => "system_message",
            Self::Error => "error",
            Self::StateChange => "state_change",
        }
    }
}

impl fmt::Display for RecorderEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRecorderEventTypeError(String);

impl fmt::Display for ParseRecorderEventTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid recorder event type: {}", self.0)
    }
}

impl std::error::Error for ParseRecorderEventTypeError {}

impl FromStr for RecorderEventType {
    type Err = ParseRecorderEventTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tool_call" => Ok(Self::ToolCall),
            "tool_result" => Ok(Self::ToolResult),
            "user_message" => Ok(Self::UserMessage),
            "assistant_message" => Ok(Self::AssistantMessage),
            "system_message" => Ok(Self::SystemMessage),
            "error" => Ok(Self::Error),
            "state_change" => Ok(Self::StateChange),
            _ => Err(ParseRecorderEventTypeError(s.to_string())),
        }
    }
}

/// A single recorded event.
#[derive(Clone, Debug, PartialEq)]
pub struct RecorderEvent {
    pub schema: &'static str,
    pub event_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event_type: RecorderEventType,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub redaction_status: RedactionStatus,
    pub previous_event_hash: Option<String>,
    pub event_hash: Option<String>,
    pub chain_status: RecorderEventChainStatus,
}

impl RecorderEvent {
    #[must_use]
    pub fn new(
        event_id: impl Into<String>,
        run_id: impl Into<String>,
        sequence: u64,
        event_type: RecorderEventType,
        timestamp: impl Into<String>,
    ) -> Self {
        Self {
            schema: RECORDER_EVENT_SCHEMA_V1,
            event_id: event_id.into(),
            run_id: run_id.into(),
            sequence,
            event_type,
            timestamp: timestamp.into(),
            payload_hash: None,
            redaction_status: RedactionStatus::None,
            previous_event_hash: None,
            event_hash: None,
            chain_status: RecorderEventChainStatus::Root,
        }
    }

    #[must_use]
    pub fn with_payload_hash(mut self, payload_hash: impl Into<String>) -> Self {
        self.payload_hash = Some(payload_hash.into());
        self
    }

    #[must_use]
    pub const fn with_redaction_status(mut self, status: RedactionStatus) -> Self {
        self.redaction_status = status;
        self
    }

    #[must_use]
    pub fn with_chain(
        mut self,
        previous_event_hash: Option<String>,
        event_hash: impl Into<String>,
        status: RecorderEventChainStatus,
    ) -> Self {
        self.previous_event_hash = previous_event_hash;
        self.event_hash = Some(event_hash.into());
        self.chain_status = status;
        self
    }
}

/// Hash-chain status for one recorder event.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecorderEventChainStatus {
    #[default]
    Root,
    Linked,
    MissingPrevious,
}

impl RecorderEventChainStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Linked => "linked",
            Self::MissingPrevious => "missing_previous",
        }
    }

    #[must_use]
    pub fn for_event(sequence: u64, previous_event_hash: Option<&str>) -> Self {
        if previous_event_hash.is_some() {
            Self::Linked
        } else if sequence <= 1 {
            Self::Root
        } else {
            Self::MissingPrevious
        }
    }
}

impl fmt::Display for RecorderEventChainStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Recorder Payload
// ============================================================================

/// Type of payload content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadContentType {
    Json,
    Text,
    Binary,
    Redacted,
}

impl PayloadContentType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Text => "text",
            Self::Binary => "binary",
            Self::Redacted => "redacted",
        }
    }
}

impl fmt::Display for PayloadContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsePayloadContentTypeError(String);

impl fmt::Display for ParsePayloadContentTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid payload content type: {}", self.0)
    }
}

impl std::error::Error for ParsePayloadContentTypeError {}

impl FromStr for PayloadContentType {
    type Err = ParsePayloadContentTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "text" => Ok(Self::Text),
            "binary" => Ok(Self::Binary),
            "redacted" => Ok(Self::Redacted),
            _ => Err(ParsePayloadContentTypeError(s.to_string())),
        }
    }
}

/// Event payload metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct RecorderPayload {
    pub schema: &'static str,
    pub payload_hash: String,
    pub content_type: PayloadContentType,
    pub byte_size: u64,
    pub compressed_size: Option<u64>,
    pub stored_at: String,
}

impl RecorderPayload {
    #[must_use]
    pub fn new(
        payload_hash: impl Into<String>,
        content_type: PayloadContentType,
        byte_size: u64,
        stored_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: RECORDER_PAYLOAD_SCHEMA_V1,
            payload_hash: payload_hash.into(),
            content_type,
            byte_size,
            compressed_size: None,
            stored_at: stored_at.into(),
        }
    }

    #[must_use]
    pub const fn with_compressed_size(mut self, compressed_size: u64) -> Self {
        self.compressed_size = Some(compressed_size);
        self
    }
}

// ============================================================================
// Redaction Status
// ============================================================================

/// Redaction status for privacy-sensitive data.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RedactionStatus {
    #[default]
    None,
    Pending,
    Partial,
    Full,
    Verified,
}

impl RedactionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Partial => "partial",
            Self::Full => "full",
            Self::Verified => "verified",
        }
    }

    /// Database-compatible status string for V027 recorder_events constraint.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::None => "clean",
            Self::Full | Self::Verified => "redacted",
            Self::Pending | Self::Partial => "quarantined",
        }
    }

    #[must_use]
    pub const fn requires_redaction(self) -> bool {
        matches!(self, Self::Pending | Self::Partial)
    }

    #[must_use]
    pub const fn is_redacted(self) -> bool {
        matches!(self, Self::Partial | Self::Full | Self::Verified)
    }

    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, Self::Verified)
    }
}

impl fmt::Display for RedactionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRedactionStatusError(String);

impl fmt::Display for ParseRedactionStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid redaction status: {}", self.0)
    }
}

impl std::error::Error for ParseRedactionStatusError {}

impl FromStr for RedactionStatus {
    type Err = ParseRedactionStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "pending" => Ok(Self::Pending),
            "partial" => Ok(Self::Partial),
            "full" => Ok(Self::Full),
            "verified" => Ok(Self::Verified),
            _ => Err(ParseRedactionStatusError(s.to_string())),
        }
    }
}

/// Redaction accounting for a recorder event or payload.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RedactionStatusSnapshot {
    pub schema: &'static str,
    pub status: RedactionStatus,
    pub redaction_classes: Vec<String>,
    pub placeholder_count: u64,
    pub redacted_bytes: u64,
    pub verified_at: Option<String>,
}

impl RedactionStatusSnapshot {
    #[must_use]
    pub fn new(status: RedactionStatus) -> Self {
        Self {
            schema: REDACTION_STATUS_SCHEMA_V1,
            status,
            redaction_classes: Vec::new(),
            placeholder_count: 0,
            redacted_bytes: 0,
            verified_at: None,
        }
    }

    pub fn add_class(&mut self, class: impl Into<String>) {
        let class = class.into();
        if !self.redaction_classes.iter().any(|known| known == &class) {
            self.redaction_classes.push(class);
            self.redaction_classes.sort();
        }
    }

    #[must_use]
    pub const fn with_counts(mut self, placeholder_count: u64, redacted_bytes: u64) -> Self {
        self.placeholder_count = placeholder_count;
        self.redacted_bytes = redacted_bytes;
        self
    }

    #[must_use]
    pub fn verified_at(mut self, timestamp: impl Into<String>) -> Self {
        self.status = RedactionStatus::Verified;
        self.verified_at = Some(timestamp.into());
        self
    }
}

// ============================================================================
// Rationale Trace
// ============================================================================

/// Visible rationale artifact kind.
///
/// These are concise user/agent-visible summaries. They are not raw private
/// model chain-of-thought, scratchpads, or complete hidden transcripts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RationaleTraceKind {
    Hypothesis,
    Decision,
    Question,
    RejectedAlternative,
    Observation,
    Conclusion,
}

impl RationaleTraceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hypothesis => "hypothesis",
            Self::Decision => "decision",
            Self::Question => "question",
            Self::RejectedAlternative => "rejected_alternative",
            Self::Observation => "observation",
            Self::Conclusion => "conclusion",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Hypothesis,
            Self::Decision,
            Self::Question,
            Self::RejectedAlternative,
            Self::Observation,
            Self::Conclusion,
        ]
    }
}

impl fmt::Display for RationaleTraceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRationaleTraceKindError(String);

impl fmt::Display for ParseRationaleTraceKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid rationale trace kind: {}", self.0)
    }
}

impl std::error::Error for ParseRationaleTraceKindError {}

impl FromStr for RationaleTraceKind {
    type Err = ParseRationaleTraceKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "hypothesis" => Ok(Self::Hypothesis),
            "decision" => Ok(Self::Decision),
            "question" => Ok(Self::Question),
            "rejected_alternative" => Ok(Self::RejectedAlternative),
            "observation" => Ok(Self::Observation),
            "conclusion" => Ok(Self::Conclusion),
            _ => Err(ParseRationaleTraceKindError(s.to_string())),
        }
    }
}

/// Evidence posture for a rationale trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RationaleTracePosture {
    Asserted,
    Supported,
    Contradicted,
    Unresolved,
}

impl RationaleTracePosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asserted => "asserted",
            Self::Supported => "supported",
            Self::Contradicted => "contradicted",
            Self::Unresolved => "unresolved",
        }
    }
}

impl fmt::Display for RationaleTracePosture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRationaleTracePostureError(String);

impl fmt::Display for ParseRationaleTracePostureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid rationale trace posture: {}", self.0)
    }
}

impl std::error::Error for ParseRationaleTracePostureError {}

impl FromStr for RationaleTracePosture {
    type Err = ParseRationaleTracePostureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "asserted" => Ok(Self::Asserted),
            "supported" => Ok(Self::Supported),
            "contradicted" => Ok(Self::Contradicted),
            "unresolved" => Ok(Self::Unresolved),
            _ => Err(ParseRationaleTracePostureError(s.to_string())),
        }
    }
}

/// Visibility/redaction posture for a rationale trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RationaleTraceVisibility {
    Public,
    Redacted,
    PrivateRejected,
}

impl RationaleTraceVisibility {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Redacted => "redacted",
            Self::PrivateRejected => "private_rejected",
        }
    }

    #[must_use]
    pub const fn is_storable(self) -> bool {
        !matches!(self, Self::PrivateRejected)
    }
}

impl fmt::Display for RationaleTraceVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRationaleTraceVisibilityError(String);

impl fmt::Display for ParseRationaleTraceVisibilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid rationale trace visibility: {}", self.0)
    }
}

impl std::error::Error for ParseRationaleTraceVisibilityError {}

impl FromStr for RationaleTraceVisibility {
    type Err = ParseRationaleTraceVisibilityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "public" => Ok(Self::Public),
            "redacted" => Ok(Self::Redacted),
            "private_rejected" => Ok(Self::PrivateRejected),
            _ => Err(ParseRationaleTraceVisibilityError(s.to_string())),
        }
    }
}

/// Validation failure for safe rationale trace summaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RationaleTraceValidationErrorKind {
    EmptySummary,
    PrivateReasoningMaterial,
    SecretLikeContent,
    InvalidConfidence,
}

impl RationaleTraceValidationErrorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptySummary => "empty_summary",
            Self::PrivateReasoningMaterial => "private_reasoning_material",
            Self::SecretLikeContent => "secret_like_content",
            Self::InvalidConfidence => "invalid_confidence",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RationaleTraceValidationError {
    pub kind: RationaleTraceValidationErrorKind,
    pub field: &'static str,
}

impl RationaleTraceValidationError {
    #[must_use]
    pub const fn new(kind: RationaleTraceValidationErrorKind, field: &'static str) -> Self {
        Self { kind, field }
    }
}

impl fmt::Display for RationaleTraceValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} in rationale trace field `{}`",
            self.kind.as_str(),
            self.field
        )
    }
}

impl std::error::Error for RationaleTraceValidationError {}

/// A safe, evidence-linked rationale summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RationaleTrace {
    pub schema: &'static str,
    pub trace_id: String,
    pub kind: RationaleTraceKind,
    pub author: String,
    pub summary: String,
    pub posture: RationaleTracePosture,
    pub confidence_basis_points: u16,
    pub visibility: RationaleTraceVisibility,
    pub redaction_status: RedactionStatus,
    pub evidence_uris: Vec<String>,
    pub linked_memory_ids: Vec<String>,
    pub linked_context_pack_ids: Vec<String>,
    pub linked_recorder_run_ids: Vec<String>,
    pub linked_recorder_event_ids: Vec<String>,
    pub linked_causal_trace_ids: Vec<String>,
    pub supersedes_trace_ids: Vec<String>,
    pub contradicted_by_trace_ids: Vec<String>,
    pub created_at: String,
}

impl RationaleTrace {
    /// Create a safe rationale trace.
    ///
    /// The summary is validated up front so private chain-of-thought markers
    /// and common secret-shaped values fail before the trace can be stored.
    pub fn new(
        trace_id: impl Into<String>,
        kind: RationaleTraceKind,
        author: impl Into<String>,
        summary: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Result<Self, RationaleTraceValidationError> {
        let summary = summary.into();
        validate_rationale_summary(&summary)?;
        Ok(Self {
            schema: RATIONALE_TRACE_SCHEMA_V1,
            trace_id: trace_id.into(),
            kind,
            author: author.into(),
            summary,
            posture: RationaleTracePosture::Asserted,
            confidence_basis_points: 5000,
            visibility: RationaleTraceVisibility::Public,
            redaction_status: RedactionStatus::None,
            evidence_uris: Vec::new(),
            linked_memory_ids: Vec::new(),
            linked_context_pack_ids: Vec::new(),
            linked_recorder_run_ids: Vec::new(),
            linked_recorder_event_ids: Vec::new(),
            linked_causal_trace_ids: Vec::new(),
            supersedes_trace_ids: Vec::new(),
            contradicted_by_trace_ids: Vec::new(),
            created_at: created_at.into(),
        })
    }

    pub fn with_confidence_basis_points(
        mut self,
        confidence_basis_points: u16,
    ) -> Result<Self, RationaleTraceValidationError> {
        if confidence_basis_points > 10_000 {
            return Err(RationaleTraceValidationError::new(
                RationaleTraceValidationErrorKind::InvalidConfidence,
                "confidenceBasisPoints",
            ));
        }
        self.confidence_basis_points = confidence_basis_points;
        Ok(self)
    }

    #[must_use]
    pub const fn with_posture(mut self, posture: RationaleTracePosture) -> Self {
        self.posture = posture;
        self
    }

    #[must_use]
    pub const fn with_visibility(
        mut self,
        visibility: RationaleTraceVisibility,
        redaction_status: RedactionStatus,
    ) -> Self {
        self.visibility = visibility;
        self.redaction_status = redaction_status;
        self
    }

    #[must_use]
    pub fn with_evidence_uri(mut self, uri: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.evidence_uris, uri.into());
        self
    }

    #[must_use]
    pub fn with_memory_id(mut self, memory_id: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.linked_memory_ids, memory_id.into());
        self
    }

    #[must_use]
    pub fn with_context_pack_id(mut self, pack_id: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.linked_context_pack_ids, pack_id.into());
        self
    }

    #[must_use]
    pub fn with_recorder_run_id(mut self, run_id: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.linked_recorder_run_ids, run_id.into());
        self
    }

    #[must_use]
    pub fn with_recorder_event_id(mut self, event_id: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.linked_recorder_event_ids, event_id.into());
        self
    }

    #[must_use]
    pub fn with_causal_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.linked_causal_trace_ids, trace_id.into());
        self
    }

    #[must_use]
    pub fn supersedes_trace(mut self, trace_id: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.supersedes_trace_ids, trace_id.into());
        self
    }

    #[must_use]
    pub fn contradicted_by_trace(mut self, trace_id: impl Into<String>) -> Self {
        push_unique_sorted(&mut self.contradicted_by_trace_ids, trace_id.into());
        self
    }
}

pub fn validate_rationale_summary(summary: &str) -> Result<(), RationaleTraceValidationError> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err(RationaleTraceValidationError::new(
            RationaleTraceValidationErrorKind::EmptySummary,
            "summary",
        ));
    }

    if contains_private_reasoning_marker(trimmed) {
        return Err(RationaleTraceValidationError::new(
            RationaleTraceValidationErrorKind::PrivateReasoningMaterial,
            "summary",
        ));
    }

    if contains_secret_like_marker(trimmed) {
        return Err(RationaleTraceValidationError::new(
            RationaleTraceValidationErrorKind::SecretLikeContent,
            "summary",
        ));
    }

    Ok(())
}

fn contains_private_reasoning_marker(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    [
        "chain-of-thought",
        "chain of thought",
        "hidden reasoning",
        "private scratchpad",
        "private reasoning",
        "raw transcript",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn contains_secret_like_marker(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    if [
        "-----begin",
        concat!("pass", "word="),
        concat!("to", "ken="),
        concat!("sec", "ret="),
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        return true;
    }

    value.split(secret_token_boundary).any(|token| {
        is_openai_key_like_token(token)
            || is_github_token_like_token(token)
            || is_aws_access_key_like_token(token)
    })
}

fn secret_token_boundary(ch: char) -> bool {
    !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn is_openai_key_like_token(token: &str) -> bool {
    token
        .to_ascii_lowercase()
        .strip_prefix("sk-")
        .is_some_and(|suffix| secret_suffix_is_key_like(suffix, 16))
}

fn is_github_token_like_token(token: &str) -> bool {
    token
        .to_ascii_lowercase()
        .strip_prefix("ghp_")
        .is_some_and(|suffix| secret_suffix_is_key_like(suffix, 16))
}

fn is_aws_access_key_like_token(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    upper.starts_with("AKIA")
        && upper.len() >= 16
        && upper
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

fn secret_suffix_is_key_like(suffix: &str, min_len: usize) -> bool {
    suffix.len() >= min_len
        && suffix
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn push_unique_sorted(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|known| known == &value) {
        values.push(value);
        values.sort();
    }
}

// ============================================================================
// Import Cursor
// ============================================================================

/// Type of import source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportSourceType {
    Cass,
    EideticLegacy,
    Recorder,
    Manual,
}

impl ImportSourceType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cass => "cass",
            Self::EideticLegacy => "eidetic_legacy",
            Self::Recorder => "recorder",
            Self::Manual => "manual",
        }
    }
}

impl fmt::Display for ImportSourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseImportSourceTypeError(String);

impl fmt::Display for ParseImportSourceTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid import source type: {}", self.0)
    }
}

impl std::error::Error for ParseImportSourceTypeError {}

impl FromStr for ImportSourceType {
    type Err = ParseImportSourceTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cass" => Ok(Self::Cass),
            "eidetic_legacy" => Ok(Self::EideticLegacy),
            "recorder" => Ok(Self::Recorder),
            "manual" => Ok(Self::Manual),
            _ => Err(ParseImportSourceTypeError(s.to_string())),
        }
    }
}

/// Import cursor for incremental imports.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportCursor {
    pub schema: &'static str,
    pub source_type: ImportSourceType,
    pub source_id: String,
    pub last_imported_id: Option<String>,
    pub last_imported_at: Option<String>,
    pub items_imported: u64,
    pub cursor_state: Option<String>,
}

impl ImportCursor {
    #[must_use]
    pub fn new(source_type: ImportSourceType, source_id: impl Into<String>) -> Self {
        Self {
            schema: IMPORT_CURSOR_SCHEMA_V1,
            source_type,
            source_id: source_id.into(),
            last_imported_id: None,
            last_imported_at: None,
            items_imported: 0,
            cursor_state: None,
        }
    }

    #[must_use]
    pub fn with_position(
        mut self,
        last_id: impl Into<String>,
        timestamp: impl Into<String>,
        count: u64,
    ) -> Self {
        self.last_imported_id = Some(last_id.into());
        self.last_imported_at = Some(timestamp.into());
        self.items_imported = count;
        self
    }

    #[must_use]
    pub fn with_cursor_state(mut self, state: impl Into<String>) -> Self {
        self.cursor_state = Some(state.into());
        self
    }
}

// ============================================================================
// Schema Catalog
// ============================================================================

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecorderFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl RecorderFieldSchema {
    #[must_use]
    pub const fn new(
        name: &'static str,
        type_name: &'static str,
        required: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            type_name,
            required,
            description,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecorderObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [RecorderFieldSchema],
}

impl RecorderObjectSchema {
    #[must_use]
    pub fn required_count(self) -> usize {
        let mut index = 0;
        let mut count = 0;
        while index < self.fields.len() {
            if self.fields[index].required {
                count += 1;
            }
            index += 1;
        }
        count
    }
}

const RECORDER_RUN_FIELDS: &[RecorderFieldSchema] = &[
    RecorderFieldSchema::new("schema", "string", true, "Schema identifier."),
    RecorderFieldSchema::new("runId", "string", true, "Stable recorder run identifier."),
    RecorderFieldSchema::new(
        "agentId",
        "string",
        true,
        "Agent identity that owns the run.",
    ),
    RecorderFieldSchema::new(
        "sessionId",
        "string|null",
        false,
        "Optional upstream harness or CASS session identifier.",
    ),
    RecorderFieldSchema::new(
        "workspaceId",
        "string|null",
        false,
        "Optional ee workspace identifier.",
    ),
    RecorderFieldSchema::new("status", "string", true, "Recorder run lifecycle status."),
    RecorderFieldSchema::new("startedAt", "string", true, "RFC 3339 start timestamp."),
    RecorderFieldSchema::new(
        "endedAt",
        "string|null",
        false,
        "RFC 3339 terminal timestamp.",
    ),
    RecorderFieldSchema::new("eventCount", "integer", true, "Number of events recorded."),
    RecorderFieldSchema::new(
        "redactedCount",
        "integer",
        true,
        "Number of events with redaction.",
    ),
];

const RECORDER_EVENT_FIELDS: &[RecorderFieldSchema] = &[
    RecorderFieldSchema::new("schema", "string", true, "Schema identifier."),
    RecorderFieldSchema::new("eventId", "string", true, "Stable event identifier."),
    RecorderFieldSchema::new("runId", "string", true, "Recorder run identifier."),
    RecorderFieldSchema::new(
        "sequence",
        "integer",
        true,
        "Monotonic sequence within the run.",
    ),
    RecorderFieldSchema::new("eventType", "string", true, "Stable event type."),
    RecorderFieldSchema::new("timestamp", "string", true, "RFC 3339 event timestamp."),
    RecorderFieldSchema::new(
        "payloadHash",
        "string|null",
        false,
        "Hash of the associated payload, when stored.",
    ),
    RecorderFieldSchema::new(
        "payloadBytes",
        "integer",
        true,
        "Original accepted payload size in bytes.",
    ),
    RecorderFieldSchema::new(
        "payloadAccepted",
        "boolean",
        true,
        "Whether the payload passed size and redaction gates.",
    ),
    RecorderFieldSchema::new(
        "redactionStatus",
        "string",
        true,
        "Privacy state for the event.",
    ),
    RecorderFieldSchema::new(
        "redactionClasses",
        "array<string>",
        true,
        "Sorted redaction classes applied to the event payload.",
    ),
    RecorderFieldSchema::new(
        "placeholderCount",
        "integer",
        true,
        "Number of redaction placeholders applied.",
    ),
    RecorderFieldSchema::new(
        "redactedBytes",
        "integer",
        true,
        "Number of payload bytes redacted before hashing.",
    ),
    RecorderFieldSchema::new(
        "previousEventHash",
        "string|null",
        false,
        "Previous event hash used to link the append-only chain.",
    ),
    RecorderFieldSchema::new(
        "eventHash",
        "string",
        true,
        "BLAKE3 hash of canonical event metadata and previous hash.",
    ),
    RecorderFieldSchema::new(
        "chainStatus",
        "string",
        true,
        "Hash-chain state: root, linked, or missing_previous.",
    ),
];

const RECORDER_PAYLOAD_FIELDS: &[RecorderFieldSchema] = &[
    RecorderFieldSchema::new("schema", "string", true, "Schema identifier."),
    RecorderFieldSchema::new(
        "payloadHash",
        "string",
        true,
        "Content-addressed payload hash.",
    ),
    RecorderFieldSchema::new("contentType", "string", true, "Payload content class."),
    RecorderFieldSchema::new(
        "byteSize",
        "integer",
        true,
        "Original payload size in bytes.",
    ),
    RecorderFieldSchema::new(
        "compressedSize",
        "integer|null",
        false,
        "Compressed payload size in bytes.",
    ),
    RecorderFieldSchema::new("storedAt", "string", true, "RFC 3339 storage timestamp."),
];

const REDACTION_STATUS_FIELDS: &[RecorderFieldSchema] = &[
    RecorderFieldSchema::new("schema", "string", true, "Schema identifier."),
    RecorderFieldSchema::new("status", "string", true, "Stable redaction status."),
    RecorderFieldSchema::new(
        "redactionClasses",
        "array<string>",
        true,
        "Sorted redaction classes present in the payload.",
    ),
    RecorderFieldSchema::new(
        "placeholderCount",
        "integer",
        true,
        "Number of placeholders inserted.",
    ),
    RecorderFieldSchema::new(
        "redactedBytes",
        "integer",
        true,
        "Number of bytes redacted.",
    ),
    RecorderFieldSchema::new(
        "verifiedAt",
        "string|null",
        false,
        "RFC 3339 verification timestamp.",
    ),
];

const IMPORT_CURSOR_FIELDS: &[RecorderFieldSchema] = &[
    RecorderFieldSchema::new("schema", "string", true, "Schema identifier."),
    RecorderFieldSchema::new("sourceType", "string", true, "Importer source type."),
    RecorderFieldSchema::new(
        "sourceId",
        "string",
        true,
        "Stable external source identity.",
    ),
    RecorderFieldSchema::new(
        "lastImportedId",
        "string|null",
        false,
        "Last external item imported.",
    ),
    RecorderFieldSchema::new(
        "lastImportedAt",
        "string|null",
        false,
        "RFC 3339 timestamp of the last imported item.",
    ),
    RecorderFieldSchema::new(
        "itemsImported",
        "integer",
        true,
        "Total imported item count.",
    ),
    RecorderFieldSchema::new(
        "cursorState",
        "string|null",
        false,
        "Opaque connector cursor state.",
    ),
];

const RECORDER_IMPORT_PLAN_FIELDS: &[RecorderFieldSchema] = &[
    RecorderFieldSchema::new("schema", "string", true, "Schema identifier."),
    RecorderFieldSchema::new(
        "command",
        "string",
        true,
        "Stable command name: recorder import.",
    ),
    RecorderFieldSchema::new(
        "dryRun",
        "boolean",
        true,
        "Whether this plan avoids durable recorder writes.",
    ),
    RecorderFieldSchema::new(
        "source",
        "object",
        true,
        "Connector source identity and parser contract.",
    ),
    RecorderFieldSchema::new(
        "run",
        "object",
        true,
        "Planned imported recorder run metadata.",
    ),
    RecorderFieldSchema::new(
        "summary",
        "object",
        true,
        "Counts for discovered, mapped, rejected, and redacted events.",
    ),
    RecorderFieldSchema::new(
        "mutations",
        "array<object>",
        true,
        "Dry-run mutation descriptions that would be applied by a future writer.",
    ),
    RecorderFieldSchema::new(
        "events",
        "array<object>",
        true,
        "Deterministic mapped event plans without raw payload content.",
    ),
    RecorderFieldSchema::new(
        "warnings",
        "array<string>",
        true,
        "Non-fatal importer warnings and truncation notices.",
    ),
];

const RATIONALE_TRACE_FIELDS: &[RecorderFieldSchema] = &[
    RecorderFieldSchema::new("schema", "string", true, "Schema identifier."),
    RecorderFieldSchema::new(
        "traceId",
        "string",
        true,
        "Stable rationale trace identifier.",
    ),
    RecorderFieldSchema::new("kind", "string", true, "Visible rationale artifact kind."),
    RecorderFieldSchema::new(
        "author",
        "string",
        true,
        "Agent or human author identifier.",
    ),
    RecorderFieldSchema::new(
        "summary",
        "string",
        true,
        "Concise visible summary; never private chain-of-thought.",
    ),
    RecorderFieldSchema::new(
        "posture",
        "string",
        true,
        "Evidence posture: asserted, supported, contradicted, or unresolved.",
    ),
    RecorderFieldSchema::new(
        "confidenceBasisPoints",
        "integer",
        true,
        "Confidence from 0 to 10000 basis points.",
    ),
    RecorderFieldSchema::new(
        "visibility",
        "string",
        true,
        "Visibility/redaction posture for the summary.",
    ),
    RecorderFieldSchema::new(
        "redactionStatus",
        "string",
        true,
        "Redaction status applied before storage or rendering.",
    ),
    RecorderFieldSchema::new(
        "evidenceUris",
        "array<string>",
        true,
        "Canonical provenance URIs supporting the trace.",
    ),
    RecorderFieldSchema::new(
        "linkedMemoryIds",
        "array<string>",
        true,
        "Memory IDs linked to this rationale trace.",
    ),
    RecorderFieldSchema::new(
        "linkedContextPackIds",
        "array<string>",
        true,
        "Context pack IDs linked to this rationale trace.",
    ),
    RecorderFieldSchema::new(
        "linkedRecorderRunIds",
        "array<string>",
        true,
        "Recorder run IDs linked to this rationale trace.",
    ),
    RecorderFieldSchema::new(
        "linkedRecorderEventIds",
        "array<string>",
        true,
        "Recorder event IDs linked to this rationale trace.",
    ),
    RecorderFieldSchema::new(
        "linkedCausalTraceIds",
        "array<string>",
        true,
        "Causal trace IDs that reuse this rationale.",
    ),
    RecorderFieldSchema::new(
        "supersedesTraceIds",
        "array<string>",
        true,
        "Prior rationale trace IDs superseded by this trace.",
    ),
    RecorderFieldSchema::new(
        "contradictedByTraceIds",
        "array<string>",
        true,
        "Trace IDs that contradict this rationale.",
    ),
    RecorderFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
];

#[must_use]
pub const fn recorder_schemas() -> [RecorderObjectSchema; 7] {
    [
        RecorderObjectSchema {
            schema_name: RECORDER_RUN_SCHEMA_V1,
            schema_uri: "urn:ee:schema:recorder-run:v1",
            kind: "recorder_run",
            title: "RecorderRunMeta",
            description: "Metadata for one append-only recorder run.",
            fields: RECORDER_RUN_FIELDS,
        },
        RecorderObjectSchema {
            schema_name: RECORDER_EVENT_SCHEMA_V1,
            schema_uri: "urn:ee:schema:recorder-event:v1",
            kind: "recorder_event",
            title: "RecorderEvent",
            description: "A single ordered event in a recorder run.",
            fields: RECORDER_EVENT_FIELDS,
        },
        RecorderObjectSchema {
            schema_name: RECORDER_PAYLOAD_SCHEMA_V1,
            schema_uri: "urn:ee:schema:recorder-payload:v1",
            kind: "recorder_payload",
            title: "RecorderPayload",
            description: "Metadata for stored recorder event payload content.",
            fields: RECORDER_PAYLOAD_FIELDS,
        },
        RecorderObjectSchema {
            schema_name: REDACTION_STATUS_SCHEMA_V1,
            schema_uri: "urn:ee:schema:redaction-status:v1",
            kind: "redaction_status",
            title: "RedactionStatusSnapshot",
            description: "Redaction accounting for recorder events and payloads.",
            fields: REDACTION_STATUS_FIELDS,
        },
        RecorderObjectSchema {
            schema_name: IMPORT_CURSOR_SCHEMA_V1,
            schema_uri: "urn:ee:schema:import-cursor:v1",
            kind: "import_cursor",
            title: "ImportCursor",
            description: "Incremental import cursor for replayable connectors.",
            fields: IMPORT_CURSOR_FIELDS,
        },
        RecorderObjectSchema {
            schema_name: RECORDER_IMPORT_PLAN_SCHEMA_V1,
            schema_uri: "urn:ee:schema:recorder-import-plan:v1",
            kind: "recorder_import_plan",
            title: "RecorderImportPlan",
            description: "Read-only connector mapping plan for imported recorder runs and events.",
            fields: RECORDER_IMPORT_PLAN_FIELDS,
        },
        RecorderObjectSchema {
            schema_name: RATIONALE_TRACE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:rationale-trace:v1",
            kind: "rationale_trace",
            title: "RationaleTrace",
            description: "Safe visible rationale summary with evidence links and redaction posture.",
            fields: RATIONALE_TRACE_FIELDS,
        },
    ]
}

#[must_use]
pub fn recorder_schema_catalog_json() -> String {
    let schemas = recorder_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!(
        "  \"schema\": \"{RECORDER_SCHEMA_CATALOG_V1}\",\n"
    ));
    output.push_str("  \"schemas\": [\n");
    for (schema_index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, schema.schema_uri);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, schema.schema_name);
        output.push_str(",\n");
        output.push_str("      \"kind\": ");
        push_json_string(&mut output, schema.kind);
        output.push_str(",\n");
        output.push_str("      \"title\": ");
        push_json_string(&mut output, schema.title);
        output.push_str(",\n");
        output.push_str("      \"description\": ");
        push_json_string(&mut output, schema.description);
        output.push_str(",\n");
        output.push_str("      \"type\": \"object\",\n");
        output.push_str("      \"required\": [\n");
        let mut emitted_required = 0;
        for field in schema.fields {
            if field.required {
                emitted_required += 1;
                output.push_str("        ");
                push_json_string(&mut output, field.name);
                if emitted_required == schema.required_count() {
                    output.push('\n');
                } else {
                    output.push_str(",\n");
                }
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"fields\": [\n");
        for (field_index, field) in schema.fields.iter().enumerate() {
            output.push_str("        {\"name\": ");
            push_json_string(&mut output, field.name);
            output.push_str(", \"type\": ");
            push_json_string(&mut output, field.type_name);
            output.push_str(", \"required\": ");
            output.push_str(if field.required { "true" } else { "false" });
            output.push_str(", \"description\": ");
            push_json_string(&mut output, field.description);
            if field_index + 1 == schema.fields.len() {
                output.push_str("}\n");
            } else {
                output.push_str("},\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"additionalProperties\": false\n");
        if schema_index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDER_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/recorder_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn trace(
        result: Result<RationaleTrace, RationaleTraceValidationError>,
    ) -> Result<RationaleTrace, String> {
        result.map_err(|error| error.to_string())
    }

    #[test]
    fn recorder_run_status_strings_are_stable() -> TestResult {
        ensure(RecorderRunStatus::Active.as_str(), "active", "active")?;
        ensure(
            RecorderRunStatus::Completed.as_str(),
            "completed",
            "completed",
        )?;
        ensure(
            RecorderRunStatus::Abandoned.as_str(),
            "abandoned",
            "abandoned",
        )?;
        ensure(RecorderRunStatus::Imported.as_str(), "imported", "imported")
    }

    #[test]
    fn recorder_run_builder_sets_schema_and_defaults() -> TestResult {
        let run = RecorderRunMeta::new("rrun_001", "NobleCardinal", "2026-04-30T12:00:00Z")
            .with_workspace_id("wsp_001")
            .with_session_id("session_001")
            .with_event_counts(3, 1)
            .finished(RecorderRunStatus::Completed, "2026-04-30T12:05:00Z");

        ensure(run.schema, RECORDER_RUN_SCHEMA_V1, "schema")?;
        ensure(run.status, RecorderRunStatus::Completed, "status")?;
        ensure(run.event_count, 3, "event count")?;
        ensure(run.redacted_count, 1, "redacted count")?;
        ensure(run.workspace_id, Some("wsp_001".to_string()), "workspace")
    }

    #[test]
    fn recorder_run_status_is_terminal() -> TestResult {
        ensure(
            RecorderRunStatus::Active.is_terminal(),
            false,
            "active not terminal",
        )?;
        ensure(
            RecorderRunStatus::Completed.is_terminal(),
            true,
            "completed terminal",
        )?;
        ensure(
            RecorderRunStatus::Abandoned.is_terminal(),
            true,
            "abandoned terminal",
        )?;
        ensure(
            RecorderRunStatus::Imported.is_terminal(),
            true,
            "imported terminal",
        )
    }

    #[test]
    fn recorder_run_status_parse_roundtrip() -> TestResult {
        for status in [
            RecorderRunStatus::Active,
            RecorderRunStatus::Completed,
            RecorderRunStatus::Abandoned,
            RecorderRunStatus::Imported,
        ] {
            let parsed: RecorderRunStatus = status.as_str().parse().map_err(|e| format!("{e}"))?;
            ensure(parsed, status, "roundtrip")?;
        }
        Ok(())
    }

    #[test]
    fn recorder_event_type_strings_are_stable() -> TestResult {
        ensure(
            RecorderEventType::ToolCall.as_str(),
            "tool_call",
            "tool_call",
        )?;
        ensure(
            RecorderEventType::ToolResult.as_str(),
            "tool_result",
            "tool_result",
        )?;
        ensure(
            RecorderEventType::UserMessage.as_str(),
            "user_message",
            "user_message",
        )?;
        ensure(
            RecorderEventType::AssistantMessage.as_str(),
            "assistant_message",
            "assistant_message",
        )?;
        ensure(
            RecorderEventType::SystemMessage.as_str(),
            "system_message",
            "system_message",
        )?;
        ensure(RecorderEventType::Error.as_str(), "error", "error")?;
        ensure(
            RecorderEventType::StateChange.as_str(),
            "state_change",
            "state_change",
        )
    }

    #[test]
    fn recorder_event_builder_sets_payload_and_redaction() -> TestResult {
        let event = RecorderEvent::new(
            "revt_001",
            "rrun_001",
            7,
            RecorderEventType::ToolCall,
            "2026-04-30T12:00:01Z",
        )
        .with_payload_hash("blake3:abc")
        .with_redaction_status(RedactionStatus::Verified)
        .with_chain(
            Some("blake3:previous".to_string()),
            "blake3:event",
            RecorderEventChainStatus::Linked,
        );

        ensure(event.schema, RECORDER_EVENT_SCHEMA_V1, "schema")?;
        ensure(event.sequence, 7, "sequence")?;
        ensure(
            event.payload_hash,
            Some("blake3:abc".to_string()),
            "payload",
        )?;
        ensure(
            event.redaction_status,
            RedactionStatus::Verified,
            "redaction",
        )?;
        ensure(
            event.previous_event_hash,
            Some("blake3:previous".to_string()),
            "previous hash",
        )?;
        ensure(
            event.event_hash,
            Some("blake3:event".to_string()),
            "event hash",
        )?;
        ensure(
            event.chain_status,
            RecorderEventChainStatus::Linked,
            "chain status",
        )
    }

    #[test]
    fn recorder_event_chain_status_strings_are_stable() -> TestResult {
        ensure(RecorderEventChainStatus::Root.as_str(), "root", "root")?;
        ensure(
            RecorderEventChainStatus::Linked.as_str(),
            "linked",
            "linked",
        )?;
        ensure(
            RecorderEventChainStatus::MissingPrevious.as_str(),
            "missing_previous",
            "missing previous",
        )
    }

    #[test]
    fn recorder_event_chain_status_detects_missing_previous() -> TestResult {
        ensure(
            RecorderEventChainStatus::for_event(1, None),
            RecorderEventChainStatus::Root,
            "root",
        )?;
        ensure(
            RecorderEventChainStatus::for_event(2, Some("blake3:prev")),
            RecorderEventChainStatus::Linked,
            "linked",
        )?;
        ensure(
            RecorderEventChainStatus::for_event(2, None),
            RecorderEventChainStatus::MissingPrevious,
            "missing",
        )
    }

    #[test]
    fn recorder_payload_builder_sets_schema() -> TestResult {
        let payload = RecorderPayload::new(
            "blake3:def",
            PayloadContentType::Json,
            1024,
            "2026-04-30T12:00:02Z",
        )
        .with_compressed_size(256);

        ensure(payload.schema, RECORDER_PAYLOAD_SCHEMA_V1, "schema")?;
        ensure(payload.byte_size, 1024, "byte size")?;
        ensure(payload.compressed_size, Some(256), "compressed")
    }

    #[test]
    fn redaction_status_strings_are_stable() -> TestResult {
        ensure(RedactionStatus::None.as_str(), "none", "none")?;
        ensure(RedactionStatus::Pending.as_str(), "pending", "pending")?;
        ensure(RedactionStatus::Partial.as_str(), "partial", "partial")?;
        ensure(RedactionStatus::Full.as_str(), "full", "full")?;
        ensure(RedactionStatus::Verified.as_str(), "verified", "verified")
    }

    #[test]
    fn redaction_status_predicates() -> TestResult {
        ensure(
            RedactionStatus::None.requires_redaction(),
            false,
            "none not pending",
        )?;
        ensure(
            RedactionStatus::Pending.requires_redaction(),
            true,
            "pending requires",
        )?;
        ensure(
            RedactionStatus::Partial.requires_redaction(),
            true,
            "partial still requires completion",
        )?;
        ensure(
            RedactionStatus::None.is_redacted(),
            false,
            "none not redacted",
        )?;
        ensure(
            RedactionStatus::Partial.is_redacted(),
            true,
            "partial is redacted",
        )?;
        ensure(
            RedactionStatus::Full.is_redacted(),
            true,
            "full is redacted",
        )?;
        ensure(
            RedactionStatus::Verified.is_redacted(),
            true,
            "verified is redacted",
        )?;
        ensure(
            RedactionStatus::Verified.is_verified(),
            true,
            "verified predicate",
        )
    }

    #[test]
    fn redaction_status_snapshot_sorts_unique_classes() -> TestResult {
        let mut status = RedactionStatusSnapshot::new(RedactionStatus::Full)
            .with_counts(2, 64)
            .verified_at("2026-04-30T12:01:00Z");
        status.add_class("password");
        status.add_class("api_key");
        status.add_class("password");

        ensure(status.schema, REDACTION_STATUS_SCHEMA_V1, "schema")?;
        ensure(status.status, RedactionStatus::Verified, "status")?;
        ensure(
            status.redaction_classes,
            vec!["api_key".to_string(), "password".to_string()],
            "classes",
        )?;
        ensure(status.placeholder_count, 2, "placeholder count")?;
        ensure(status.redacted_bytes, 64, "redacted bytes")
    }

    #[test]
    fn rationale_trace_kind_strings_are_stable() -> TestResult {
        ensure(
            RationaleTraceKind::Hypothesis.as_str(),
            "hypothesis",
            "hypothesis",
        )?;
        ensure(
            RationaleTraceKind::Decision.as_str(),
            "decision",
            "decision",
        )?;
        ensure(
            RationaleTraceKind::Question.as_str(),
            "question",
            "question",
        )?;
        ensure(
            RationaleTraceKind::RejectedAlternative.as_str(),
            "rejected_alternative",
            "rejected alternative",
        )?;
        ensure(
            RationaleTraceKind::Observation.as_str(),
            "observation",
            "observation",
        )?;
        ensure(
            RationaleTraceKind::Conclusion.as_str(),
            "conclusion",
            "conclusion",
        )
    }

    #[test]
    fn rationale_trace_enums_parse_roundtrip() -> TestResult {
        for kind in RationaleTraceKind::all() {
            let parsed: RationaleTraceKind = kind.as_str().parse().map_err(|e| format!("{e}"))?;
            ensure(parsed, kind, "kind roundtrip")?;
        }

        for posture in [
            RationaleTracePosture::Asserted,
            RationaleTracePosture::Supported,
            RationaleTracePosture::Contradicted,
            RationaleTracePosture::Unresolved,
        ] {
            let parsed: RationaleTracePosture =
                posture.as_str().parse().map_err(|e| format!("{e}"))?;
            ensure(parsed, posture, "posture roundtrip")?;
        }

        for visibility in [
            RationaleTraceVisibility::Public,
            RationaleTraceVisibility::Redacted,
            RationaleTraceVisibility::PrivateRejected,
        ] {
            let parsed: RationaleTraceVisibility =
                visibility.as_str().parse().map_err(|e| format!("{e}"))?;
            ensure(parsed, visibility, "visibility roundtrip")?;
        }
        Ok(())
    }

    #[test]
    fn rationale_trace_builder_sorts_links_and_sets_safety_defaults() -> TestResult {
        let trace = trace(RationaleTrace::new(
            "rat_001",
            RationaleTraceKind::Hypothesis,
            "ProudBasin",
            "The failure appears tied to a missing recorder event link.",
            "2026-05-03T18:20:00Z",
        ))?
        .with_confidence_basis_points(6400)
        .map_err(|error| error.to_string())?
        .with_posture(RationaleTracePosture::Supported)
        .with_visibility(
            RationaleTraceVisibility::Redacted,
            RedactionStatus::Verified,
        )
        .with_evidence_uri("agent-mail://eidetic_engine_cli-kz1.2/1477")
        .with_evidence_uri("cass-session://session_002#L4-L9")
        .with_evidence_uri("cass-session://session_002#L4-L9")
        .with_memory_id("mem_002")
        .with_memory_id("mem_001")
        .with_context_pack_id("pack_001")
        .with_recorder_run_id("rrun_001")
        .with_recorder_event_id("revt_002")
        .with_recorder_event_id("revt_001")
        .with_causal_trace_id("causal_001")
        .supersedes_trace("rat_000")
        .contradicted_by_trace("rat_009");

        ensure(trace.schema, RATIONALE_TRACE_SCHEMA_V1, "schema")?;
        ensure(trace.kind, RationaleTraceKind::Hypothesis, "kind")?;
        ensure(trace.posture, RationaleTracePosture::Supported, "posture")?;
        ensure(
            trace.visibility,
            RationaleTraceVisibility::Redacted,
            "visibility",
        )?;
        ensure(
            trace.redaction_status,
            RedactionStatus::Verified,
            "redaction",
        )?;
        ensure(trace.confidence_basis_points, 6400, "confidence")?;
        ensure(
            trace.evidence_uris,
            vec![
                "agent-mail://eidetic_engine_cli-kz1.2/1477".to_string(),
                "cass-session://session_002#L4-L9".to_string(),
            ],
            "evidence sorted unique",
        )?;
        ensure(
            trace.linked_memory_ids,
            vec!["mem_001".to_string(), "mem_002".to_string()],
            "memory links sorted",
        )?;
        ensure(
            trace.linked_recorder_event_ids,
            vec!["revt_001".to_string(), "revt_002".to_string()],
            "event links sorted",
        )?;
        ensure(
            trace.supersedes_trace_ids,
            vec!["rat_000".to_string()],
            "supersedes",
        )?;
        ensure(
            trace.contradicted_by_trace_ids,
            vec!["rat_009".to_string()],
            "contradicted by",
        )
    }

    #[test]
    fn rationale_trace_validation_rejects_private_reasoning_and_secret_like_summaries() {
        let empty = validate_rationale_summary("  ");
        assert_eq!(
            empty.map_err(|error| error.kind),
            Err(RationaleTraceValidationErrorKind::EmptySummary)
        );

        let private = validate_rationale_summary("raw chain-of-thought: first I considered...");
        assert_eq!(
            private.map_err(|error| error.kind),
            Err(RationaleTraceValidationErrorKind::PrivateReasoningMaterial)
        );

        let blocked = validate_rationale_summary(concat!("The log included to", "ken=abc123."));
        assert_eq!(
            blocked.map_err(|error| error.kind),
            Err(RationaleTraceValidationErrorKind::SecretLikeContent)
        );

        let key_like =
            validate_rationale_summary(concat!("The log included sk-", "aaaaaaaaaaaaaaaa", "."));
        assert_eq!(
            key_like.map_err(|error| error.kind),
            Err(RationaleTraceValidationErrorKind::SecretLikeContent)
        );
    }

    #[test]
    fn rationale_trace_validation_allows_ordinary_risk_language() -> TestResult {
        validate_rationale_summary(
            "Risk-managed task-scoped evidence supports the release decision.",
        )
        .map_err(|error| error.to_string())?;
        validate_rationale_summary("The literal sk- prefix was mentioned without a key body.")
            .map_err(|error| error.to_string())
    }

    #[test]
    fn rationale_trace_rejects_out_of_range_confidence() -> TestResult {
        let trace = trace(RationaleTrace::new(
            "rat_002",
            RationaleTraceKind::Observation,
            "ProudBasin",
            "Recorder linkage was present in the public event summary.",
            "2026-05-03T18:25:00Z",
        ))?;
        let error = trace
            .with_confidence_basis_points(10_001)
            .map(|_| ())
            .map_err(|error| error.kind);
        ensure(
            error,
            Err(RationaleTraceValidationErrorKind::InvalidConfidence),
            "confidence rejection",
        )
    }

    #[test]
    fn private_rejected_rationale_visibility_is_not_storable() -> TestResult {
        ensure(
            RationaleTraceVisibility::Public.is_storable(),
            true,
            "public storable",
        )?;
        ensure(
            RationaleTraceVisibility::Redacted.is_storable(),
            true,
            "redacted storable",
        )?;
        ensure(
            RationaleTraceVisibility::PrivateRejected.is_storable(),
            false,
            "private rejected not storable",
        )
    }

    #[test]
    fn import_source_type_strings_are_stable() -> TestResult {
        ensure(ImportSourceType::Cass.as_str(), "cass", "cass")?;
        ensure(
            ImportSourceType::EideticLegacy.as_str(),
            "eidetic_legacy",
            "eidetic_legacy",
        )?;
        ensure(ImportSourceType::Recorder.as_str(), "recorder", "recorder")?;
        ensure(ImportSourceType::Manual.as_str(), "manual", "manual")
    }

    #[test]
    fn import_cursor_builder() -> TestResult {
        let cursor = ImportCursor::new(ImportSourceType::Cass, "source_123")
            .with_position("item_456", "2026-04-30T12:00:00Z", 100)
            .with_cursor_state("offset=100");

        ensure(cursor.schema, IMPORT_CURSOR_SCHEMA_V1, "schema")?;
        ensure(cursor.source_type, ImportSourceType::Cass, "source_type")?;
        ensure(cursor.source_id, "source_123".to_string(), "source_id")?;
        ensure(
            cursor.last_imported_id,
            Some("item_456".to_string()),
            "last_imported_id",
        )?;
        ensure(cursor.items_imported, 100, "items_imported")?;
        ensure(
            cursor.cursor_state,
            Some("offset=100".to_string()),
            "cursor_state",
        )
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure(RECORDER_RUN_SCHEMA_V1, "ee.recorder.run.v1", "run schema")?;
        ensure(
            RECORDER_EVENT_SCHEMA_V1,
            "ee.recorder.event.v1",
            "event schema",
        )?;
        ensure(
            RECORDER_PAYLOAD_SCHEMA_V1,
            "ee.recorder.payload.v1",
            "payload schema",
        )?;
        ensure(
            REDACTION_STATUS_SCHEMA_V1,
            "ee.redaction.status.v1",
            "redaction schema",
        )?;
        ensure(
            IMPORT_CURSOR_SCHEMA_V1,
            "ee.import.cursor.v1",
            "cursor schema",
        )?;
        ensure(
            RECORDER_IMPORT_PLAN_SCHEMA_V1,
            "ee.recorder.import_plan.v1",
            "import plan schema",
        )?;
        ensure(
            RATIONALE_TRACE_SCHEMA_V1,
            "ee.rationale_trace.v1",
            "rationale trace schema",
        )?;
        ensure(
            RECORDER_SCHEMA_CATALOG_V1,
            "ee.recorder.schemas.v1",
            "catalog schema",
        )
    }

    #[test]
    fn payload_content_type_strings_are_stable() -> TestResult {
        ensure(PayloadContentType::Json.as_str(), "json", "json")?;
        ensure(PayloadContentType::Text.as_str(), "text", "text")?;
        ensure(PayloadContentType::Binary.as_str(), "binary", "binary")?;
        ensure(
            PayloadContentType::Redacted.as_str(),
            "redacted",
            "redacted",
        )
    }

    #[test]
    fn recorder_schema_catalog_order_is_stable() -> TestResult {
        let schemas = recorder_schemas();
        ensure(schemas.len(), 7, "schema count")?;
        ensure(schemas[0].schema_name, RECORDER_RUN_SCHEMA_V1, "run")?;
        ensure(schemas[1].schema_name, RECORDER_EVENT_SCHEMA_V1, "event")?;
        ensure(
            schemas[2].schema_name,
            RECORDER_PAYLOAD_SCHEMA_V1,
            "payload",
        )?;
        ensure(
            schemas[3].schema_name,
            REDACTION_STATUS_SCHEMA_V1,
            "redaction",
        )?;
        ensure(schemas[4].schema_name, IMPORT_CURSOR_SCHEMA_V1, "cursor")?;
        ensure(
            schemas[5].schema_name,
            RECORDER_IMPORT_PLAN_SCHEMA_V1,
            "import plan",
        )?;
        ensure(
            schemas[6].schema_name,
            RATIONALE_TRACE_SCHEMA_V1,
            "rationale trace",
        )
    }

    #[test]
    fn recorder_schema_catalog_matches_golden_fixture() {
        assert_eq!(recorder_schema_catalog_json(), RECORDER_SCHEMA_GOLDEN);
    }

    #[test]
    fn recorder_schema_catalog_is_valid_json() -> TestResult {
        let parsed: serde_json::Value = serde_json::from_str(RECORDER_SCHEMA_GOLDEN)
            .map_err(|error| format!("recorder schema golden must be valid JSON: {error}"))?;
        ensure(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some(RECORDER_SCHEMA_CATALOG_V1),
            "catalog schema",
        )?;
        let schemas = parsed
            .get("schemas")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "schemas must be an array".to_string())?;
        ensure(schemas.len(), 7, "catalog length")
    }
}
