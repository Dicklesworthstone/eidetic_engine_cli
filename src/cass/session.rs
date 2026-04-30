//! CASS session import models (EE-106).
//!
//! Types for parsing and importing CASS session data into ee's storage.
//! These consume the stable robot/JSON contracts from `cass search`,
//! `cass view`, and `cass sessions` commands.
//!
//! The import flow is:
//! 1. Discover sessions via `cass sessions --json` or `cass search --robot`
//! 2. Parse session metadata into [`CassSessionInfo`]
//! 3. Extract evidence spans via `cass view` into [`CassViewSpan`]
//! 4. Track progress with [`ImportCursor`]
//!
//! This module defines the *shapes* only. Actual parsing and import
//! logic lands in follow-on beads (EE-107).

use std::{convert::Infallible, fmt};

/// Agent type from CASS session metadata.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CassAgent {
    /// Claude Code sessions.
    ClaudeCode,
    /// Codex sessions.
    Codex,
    /// Cursor sessions.
    Cursor,
    /// Gemini sessions.
    Gemini,
    /// ChatGPT sessions.
    ChatGpt,
    /// Unknown or unrecognized agent.
    #[default]
    Unknown,
}

impl CassAgent {
    /// Parse an agent string from CASS output, mapping unknown values to
    /// [`Self::Unknown`].
    #[must_use]
    pub fn parse_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "claude-code" | "claude_code" | "claudecode" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "cursor" => Self::Cursor,
            "gemini" => Self::Gemini,
            "chatgpt" | "chat-gpt" | "chat_gpt" => Self::ChatGpt,
            _ => Self::Unknown,
        }
    }

    /// Stable string representation for storage and JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::ChatGpt => "chatgpt",
            Self::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for CassAgent {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_lossy(s))
    }
}

impl fmt::Display for CassAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Session metadata from CASS `sessions` or `search` output.
///
/// Maps to the hit object in `cass search --robot` or the session
/// record in `cass sessions --json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassSessionInfo {
    /// Absolute path to the session JSONL file.
    pub source_path: String,
    /// Agent that created the session.
    pub agent: CassAgent,
    /// Workspace directory the session was in.
    pub workspace_dir: Option<String>,
    /// Session start timestamp (RFC 3339).
    pub started_at: Option<String>,
    /// Session end timestamp (RFC 3339).
    pub ended_at: Option<String>,
    /// Number of messages/turns in the session.
    pub message_count: Option<u32>,
    /// Total tokens in the session (if known).
    pub token_count: Option<u32>,
    /// Content hash for deduplication.
    pub content_hash: Option<String>,
}

impl CassSessionInfo {
    /// Create a minimal session info with just the source path.
    #[must_use]
    pub fn new(source_path: impl Into<String>) -> Self {
        Self {
            source_path: source_path.into(),
            agent: CassAgent::Unknown,
            workspace_dir: None,
            started_at: None,
            ended_at: None,
            message_count: None,
            token_count: None,
            content_hash: None,
        }
    }

    /// Builder: set the agent.
    #[must_use]
    pub fn with_agent(mut self, agent: CassAgent) -> Self {
        self.agent = agent;
        self
    }

    /// Builder: set the workspace directory.
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace_dir = Some(workspace.into());
        self
    }

    /// Builder: set the content hash.
    #[must_use]
    pub fn with_content_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = Some(hash.into());
        self
    }
}

/// Span kind from CASS `view` output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CassSpanKind {
    /// Human or assistant message.
    #[default]
    Message,
    /// Tool call (function invocation).
    ToolCall,
    /// Tool result (function return).
    ToolResult,
    /// File content or diff.
    File,
    /// Session summary or metadata.
    Summary,
}

impl CassSpanKind {
    /// Parse a span kind from CASS output, mapping unknown values to
    /// [`Self::Message`].
    #[must_use]
    pub fn parse_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "message" | "msg" => Self::Message,
            "tool_call" | "toolcall" | "function_call" => Self::ToolCall,
            "tool_result" | "toolresult" | "function_result" => Self::ToolResult,
            "file" | "diff" => Self::File,
            "summary" | "meta" => Self::Summary,
            _ => Self::Message,
        }
    }

    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::File => "file",
            Self::Summary => "summary",
        }
    }
}

impl std::str::FromStr for CassSpanKind {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_lossy(s))
    }
}

impl fmt::Display for CassSpanKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Role in a conversation span.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CassRole {
    /// Human/user message.
    #[default]
    User,
    /// Assistant/model response.
    Assistant,
    /// System message.
    System,
    /// Tool invocation/result.
    Tool,
}

impl CassRole {
    /// Parse a role from CASS output, mapping unknown values to
    /// [`Self::User`].
    #[must_use]
    pub fn parse_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "user" | "human" => Self::User,
            "assistant" | "model" | "ai" => Self::Assistant,
            "system" => Self::System,
            "tool" | "function" => Self::Tool,
            _ => Self::User,
        }
    }

    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

impl std::str::FromStr for CassRole {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_lossy(s))
    }
}

impl fmt::Display for CassRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evidence span from CASS `view` output.
///
/// Represents a contiguous chunk of a session transcript that can
/// be linked to a memory as supporting evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassViewSpan {
    /// Session source path this span came from.
    pub source_path: String,
    /// CASS-assigned span ID within the session.
    pub cass_span_id: String,
    /// Kind of span content.
    pub span_kind: CassSpanKind,
    /// Start line in the session file (1-indexed).
    pub start_line: u32,
    /// End line in the session file (inclusive, 1-indexed).
    pub end_line: u32,
    /// Conversation role if applicable.
    pub role: Option<CassRole>,
    /// Extracted text content.
    pub excerpt: String,
    /// Content hash for deduplication.
    pub content_hash: String,
}

impl CassViewSpan {
    /// Create a new view span.
    #[must_use]
    pub fn new(
        source_path: impl Into<String>,
        cass_span_id: impl Into<String>,
        span_kind: CassSpanKind,
        start_line: u32,
        end_line: u32,
        excerpt: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            cass_span_id: cass_span_id.into(),
            span_kind,
            start_line,
            end_line,
            role: None,
            excerpt: excerpt.into(),
            content_hash: content_hash.into(),
        }
    }

    /// Builder: set the role.
    #[must_use]
    pub fn with_role(mut self, role: CassRole) -> Self {
        self.role = Some(role);
        self
    }

    /// Line count of this span.
    #[must_use]
    pub const fn line_count(&self) -> u32 {
        self.end_line
            .saturating_sub(self.start_line)
            .saturating_add(1)
    }
}

/// Import cursor state for resumable imports.
///
/// Tracks progress through CASS session discovery and import so
/// interrupted imports can resume without re-processing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportCursor {
    /// Last processed session source path.
    pub last_source_path: Option<String>,
    /// Last processed line number within that session.
    pub last_line: Option<u32>,
    /// Timestamp when this cursor was saved.
    pub saved_at: Option<String>,
    /// Request ID for correlation with CASS.
    pub request_id: Option<String>,
    /// Total sessions discovered.
    pub sessions_discovered: u32,
    /// Sessions fully imported.
    pub sessions_imported: u32,
    /// Sessions skipped (already imported or filtered).
    pub sessions_skipped: u32,
    /// Spans imported across all sessions.
    pub spans_imported: u32,
}

impl ImportCursor {
    /// Create a fresh cursor for a new import.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a session as discovered.
    pub fn record_discovered(&mut self) {
        self.sessions_discovered = self.sessions_discovered.saturating_add(1);
    }

    /// Mark a session as fully imported.
    pub fn record_imported(&mut self, source_path: &str) {
        self.last_source_path = Some(source_path.to_owned());
        self.last_line = None;
        self.sessions_imported = self.sessions_imported.saturating_add(1);
    }

    /// Mark a session as skipped.
    pub fn record_skipped(&mut self) {
        self.sessions_skipped = self.sessions_skipped.saturating_add(1);
    }

    /// Record a span import.
    pub fn record_span(&mut self, source_path: &str, line: u32) {
        self.last_source_path = Some(source_path.to_owned());
        self.last_line = Some(line);
        self.spans_imported = self.spans_imported.saturating_add(1);
    }

    /// Total sessions seen (discovered).
    #[must_use]
    pub const fn total_discovered(&self) -> u32 {
        self.sessions_discovered
    }

    /// Percentage complete (imported + skipped over discovered).
    #[must_use]
    pub fn completion_percent(&self) -> f32 {
        if self.sessions_discovered == 0 {
            return 0.0;
        }
        let processed = self.sessions_imported + self.sessions_skipped;
        (processed as f32 / self.sessions_discovered as f32) * 100.0
    }

    /// Whether the import is complete.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.sessions_discovered > 0
            && (self.sessions_imported + self.sessions_skipped) >= self.sessions_discovered
    }
}

/// Result of attempting to import a single session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportSessionResult {
    /// Session was successfully imported.
    Imported {
        source_path: String,
        spans_created: u32,
    },
    /// Session was skipped (already imported).
    Skipped { source_path: String, reason: String },
    /// Session import failed.
    Failed { source_path: String, error: String },
}

impl ImportSessionResult {
    /// Whether this result represents success.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Imported { .. } | Self::Skipped { .. })
    }

    /// Whether this result represents a failure.
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// Get the source path.
    #[must_use]
    pub fn source_path(&self) -> &str {
        match self {
            Self::Imported { source_path, .. }
            | Self::Skipped { source_path, .. }
            | Self::Failed { source_path, .. } => source_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn cass_agent_parsing_handles_variants() -> TestResult {
        ensure_equal(
            &CassAgent::parse_lossy("claude-code"),
            &CassAgent::ClaudeCode,
            "claude-code",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("CODEX"),
            &CassAgent::Codex,
            "CODEX uppercase",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("cursor"),
            &CassAgent::Cursor,
            "cursor",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("gemini"),
            &CassAgent::Gemini,
            "gemini",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("chatgpt"),
            &CassAgent::ChatGpt,
            "chatgpt",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("unknown-agent"),
            &CassAgent::Unknown,
            "unknown",
        )
    }

    #[test]
    fn cass_agent_strings_are_stable() -> TestResult {
        ensure_equal(
            &CassAgent::ClaudeCode.as_str(),
            &"claude_code",
            "claude_code",
        )?;
        ensure_equal(&CassAgent::Codex.as_str(), &"codex", "codex")?;
        ensure_equal(&CassAgent::Cursor.as_str(), &"cursor", "cursor")?;
        ensure_equal(&CassAgent::Gemini.as_str(), &"gemini", "gemini")?;
        ensure_equal(&CassAgent::ChatGpt.as_str(), &"chatgpt", "chatgpt")?;
        ensure_equal(&CassAgent::Unknown.as_str(), &"unknown", "unknown")
    }

    #[test]
    fn cass_span_kind_parsing_handles_variants() -> TestResult {
        ensure_equal(
            &CassSpanKind::parse_lossy("message"),
            &CassSpanKind::Message,
            "message",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("tool_call"),
            &CassSpanKind::ToolCall,
            "tool_call",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("tool_result"),
            &CassSpanKind::ToolResult,
            "tool_result",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("file"),
            &CassSpanKind::File,
            "file",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("summary"),
            &CassSpanKind::Summary,
            "summary",
        )
    }

    #[test]
    fn cass_span_kind_strings_are_stable() -> TestResult {
        ensure_equal(&CassSpanKind::Message.as_str(), &"message", "message")?;
        ensure_equal(&CassSpanKind::ToolCall.as_str(), &"tool_call", "tool_call")?;
        ensure_equal(
            &CassSpanKind::ToolResult.as_str(),
            &"tool_result",
            "tool_result",
        )?;
        ensure_equal(&CassSpanKind::File.as_str(), &"file", "file")?;
        ensure_equal(&CassSpanKind::Summary.as_str(), &"summary", "summary")
    }

    #[test]
    fn cass_role_parsing_handles_variants() -> TestResult {
        ensure_equal(&CassRole::parse_lossy("user"), &CassRole::User, "user")?;
        ensure_equal(&CassRole::parse_lossy("human"), &CassRole::User, "human")?;
        ensure_equal(
            &CassRole::parse_lossy("assistant"),
            &CassRole::Assistant,
            "assistant",
        )?;
        ensure_equal(
            &CassRole::parse_lossy("system"),
            &CassRole::System,
            "system",
        )?;
        ensure_equal(&CassRole::parse_lossy("tool"), &CassRole::Tool, "tool")
    }

    #[test]
    fn cass_role_strings_are_stable() -> TestResult {
        ensure_equal(&CassRole::User.as_str(), &"user", "user")?;
        ensure_equal(&CassRole::Assistant.as_str(), &"assistant", "assistant")?;
        ensure_equal(&CassRole::System.as_str(), &"system", "system")?;
        ensure_equal(&CassRole::Tool.as_str(), &"tool", "tool")
    }

    #[test]
    fn cass_session_info_builder_works() {
        let info = CassSessionInfo::new("/path/to/session.jsonl")
            .with_agent(CassAgent::ClaudeCode)
            .with_workspace("/project")
            .with_content_hash("abc123");

        assert_eq!(info.source_path, "/path/to/session.jsonl");
        assert_eq!(info.agent, CassAgent::ClaudeCode);
        assert_eq!(info.workspace_dir, Some("/project".to_string()));
        assert_eq!(info.content_hash, Some("abc123".to_string()));
    }

    #[test]
    fn cass_view_span_line_count_is_correct() -> TestResult {
        let span = CassViewSpan::new(
            "/session.jsonl",
            "span-1",
            CassSpanKind::Message,
            10,
            15,
            "content",
            "hash",
        );
        ensure_equal(&span.line_count(), &6, "10-15 inclusive is 6 lines")?;

        let single = CassViewSpan::new(
            "/session.jsonl",
            "span-2",
            CassSpanKind::Message,
            5,
            5,
            "single line",
            "hash",
        );
        ensure_equal(&single.line_count(), &1, "5-5 is 1 line")
    }

    #[test]
    fn import_cursor_tracks_progress() {
        let mut cursor = ImportCursor::new();

        cursor.record_discovered();
        cursor.record_discovered();
        cursor.record_discovered();
        assert_eq!(cursor.total_discovered(), 3);
        assert!(!cursor.is_complete());

        cursor.record_imported("/session1.jsonl");
        cursor.record_skipped();
        assert_eq!(cursor.sessions_imported, 1);
        assert_eq!(cursor.sessions_skipped, 1);
        assert!(!cursor.is_complete());

        cursor.record_imported("/session2.jsonl");
        assert!(cursor.is_complete());
        assert!((cursor.completion_percent() - 100.0).abs() < 0.01);
    }

    #[test]
    fn import_cursor_completion_percent_handles_zero() {
        let cursor = ImportCursor::new();
        assert!((cursor.completion_percent() - 0.0).abs() < 0.01);
    }

    #[test]
    fn import_session_result_predicates() {
        let imported = ImportSessionResult::Imported {
            source_path: "/s.jsonl".to_string(),
            spans_created: 10,
        };
        assert!(imported.is_success());
        assert!(!imported.is_failure());

        let skipped = ImportSessionResult::Skipped {
            source_path: "/s.jsonl".to_string(),
            reason: "already imported".to_string(),
        };
        assert!(skipped.is_success());
        assert!(!skipped.is_failure());

        let failed = ImportSessionResult::Failed {
            source_path: "/s.jsonl".to_string(),
            error: "parse error".to_string(),
        };
        assert!(!failed.is_success());
        assert!(failed.is_failure());
    }
}
