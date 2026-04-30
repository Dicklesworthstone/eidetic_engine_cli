//! Recorder schemas for tracking agent recording sessions and events (EE-400).
//!
//! Defines the schema contracts for:
//! - Recording runs (sessions of recorded agent activity)
//! - Events within a recording run
//! - Event payloads (tool calls, outputs, etc.)
//! - Redaction status for privacy-sensitive data
//! - Import cursors for incremental import

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
    pub event_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event_type: RecorderEventType,
    pub timestamp: String,
    pub payload_hash: Option<String>,
    pub redaction_status: RedactionStatus,
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
    pub payload_hash: String,
    pub content_type: PayloadContentType,
    pub byte_size: u64,
    pub compressed_size: Option<u64>,
    pub stored_at: String,
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

    #[must_use]
    pub const fn requires_redaction(self) -> bool {
        matches!(self, Self::Pending)
    }

    #[must_use]
    pub const fn is_redacted(self) -> bool {
        matches!(self, Self::Partial | Self::Full | Self::Verified)
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
    pub source_type: ImportSourceType,
    pub source_id: String,
    pub last_imported_id: Option<String>,
    pub last_imported_at: Option<String>,
    pub items_imported: u64,
    pub cursor_state: Option<String>,
}

impl ImportCursor {
    pub fn new(source_type: ImportSourceType, source_id: impl Into<String>) -> Self {
        Self {
            source_type,
            source_id: source_id.into(),
            last_imported_id: None,
            last_imported_at: None,
            items_imported: 0,
            cursor_state: None,
        }
    }

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

    pub fn with_cursor_state(mut self, state: impl Into<String>) -> Self {
        self.cursor_state = Some(state.into());
        self
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
}
