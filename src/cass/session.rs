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

use std::{collections::BTreeMap, convert::Infallible, fmt};

use serde::Deserialize;
use serde_json::Value as JsonValue;

use super::error::CassError;

fn normalized_cass_token(input: &str) -> String {
    let trimmed = input.trim();
    let mut normalized = String::with_capacity(trimmed.len());
    let mut previous_was_lowercase = false;
    let mut previous_was_separator = false;

    for character in trimmed.chars() {
        match character {
            '-' | '_' => {
                if !normalized.is_empty() && !previous_was_separator {
                    normalized.push('_');
                }
                previous_was_lowercase = false;
                previous_was_separator = true;
            }
            character if character.is_whitespace() => {
                if !normalized.is_empty() && !previous_was_separator {
                    normalized.push('_');
                }
                previous_was_lowercase = false;
                previous_was_separator = true;
            }
            character if character.is_ascii_uppercase() => {
                if previous_was_lowercase && !previous_was_separator {
                    normalized.push('_');
                }
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = false;
                previous_was_separator = false;
            }
            character => {
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = character.is_ascii_lowercase();
                previous_was_separator = false;
            }
        }
    }

    normalized
}

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
        match normalized_cass_token(s).as_str() {
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
    /// CASS metadata fields that were missing or unusable when this session
    /// record was parsed.
    pub missing_metadata: Vec<String>,
    /// How `content_hash` was obtained, for provenance and degraded imports.
    pub content_hash_source: Option<String>,
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
            missing_metadata: Vec::new(),
            content_hash_source: None,
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
        self.content_hash_source = Some("provided".to_owned());
        self
    }
}

/// Stable reference to a CASS session or line range used by mesh evidence refs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassSessionReference {
    pub session_id: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
}

impl CassSessionReference {
    #[must_use]
    pub fn to_uri(&self) -> String {
        let mut uri = format!("cass-session://{}", self.session_id);
        if let Some(line_start) = self.line_start {
            uri.push_str("#L");
            uri.push_str(&line_start.to_string());
            if self.line_end.is_some_and(|line_end| line_end != line_start) {
                uri.push('-');
                uri.push_str(&self.line_end.unwrap_or(line_start).to_string());
            }
        }
        uri
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassSessionReferenceError {
    reason: &'static str,
}

impl CassSessionReferenceError {
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for CassSessionReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid CASS session reference: {}", self.reason)
    }
}

impl std::error::Error for CassSessionReferenceError {}

/// Normalize the remote-evidence CASS URI shape without touching the session body.
///
/// Accepted forms are `cass-session://<session-id>` and
/// `cass-session://<session-id>#L<start>-<end>`. Local file paths and arbitrary
/// URI query strings are rejected because peer evidence refs must preserve
/// fetchability without leaking host paths.
pub fn normalize_cass_session_uri(
    raw: &str,
) -> Result<CassSessionReference, CassSessionReferenceError> {
    let raw = raw.trim();
    let Some(rest) = raw.strip_prefix("cass-session://") else {
        return Err(CassSessionReferenceError {
            reason: "missing_cass_session_scheme",
        });
    };
    if rest.is_empty() {
        return Err(CassSessionReferenceError {
            reason: "missing_session_id",
        });
    }
    if raw.chars().any(char::is_control) || raw.contains('?') {
        return Err(CassSessionReferenceError {
            reason: "unsupported_uri_component",
        });
    }

    let (session_id, fragment) = rest
        .split_once('#')
        .map_or((rest, None), |(id, fragment)| (id, Some(fragment)));
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
    {
        return Err(CassSessionReferenceError {
            reason: "unsafe_session_id",
        });
    }
    if !session_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(CassSessionReferenceError {
            reason: "unsupported_session_id_character",
        });
    }

    let (line_start, line_end) = match fragment {
        None => (None, None),
        Some(fragment) => parse_cass_line_fragment(fragment)?,
    };

    Ok(CassSessionReference {
        session_id: session_id.to_owned(),
        line_start,
        line_end,
    })
}

fn parse_cass_line_fragment(
    fragment: &str,
) -> Result<(Option<u32>, Option<u32>), CassSessionReferenceError> {
    let Some(rest) = fragment.strip_prefix('L') else {
        return Err(CassSessionReferenceError {
            reason: "unsupported_fragment",
        });
    };
    let (start, end) = rest
        .split_once('-')
        .map_or((rest, rest), |(start, end)| (start, end));
    // Range fragments may include a leading `L` on the end side
    // (e.g. `L10-L20`); strip it before parsing so both endpoints share
    // the same digit-only form.
    let end = end.strip_prefix('L').unwrap_or(end);
    let start = parse_positive_cass_line(start)?;
    let end = parse_positive_cass_line(end)?;
    if end < start {
        return Err(CassSessionReferenceError {
            reason: "line_range_reversed",
        });
    }
    Ok((Some(start), Some(end)))
}

fn parse_positive_cass_line(value: &str) -> Result<u32, CassSessionReferenceError> {
    if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
        return Err(CassSessionReferenceError {
            reason: "invalid_line_number",
        });
    }
    let parsed = value
        .parse::<u32>()
        .map_err(|_| CassSessionReferenceError {
            reason: "invalid_line_number",
        })?;
    if parsed == 0 {
        return Err(CassSessionReferenceError {
            reason: "line_number_zero",
        });
    }
    Ok(parsed)
}

/// Parsed response from `cass search --robot --robot-meta`.
///
/// The field set mirrors CASS contract version 1. Unknown fields are
/// **silently ignored** so a newer CASS version that adds optional
/// metadata can still be consumed by an older `ee` build — the documented
/// forward-compat policy (see `docs/query-schema.md`). Any extras observed
/// during [`Self::from_robot_json`] are emitted at `tracing::debug` so
/// operators can see drift surface in the audit log without breaking
/// retrieval. Schema-drift regressions are caught by the conformance
/// harness, not by parser strictness.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CassSearchResponse {
    /// Original query text.
    pub query: String,
    /// Requested result limit.
    pub limit: u32,
    /// Requested offset.
    pub offset: u32,
    /// Number of hits returned in this payload.
    pub count: u32,
    /// Total matches reported by CASS.
    pub total_matches: u64,
    /// Applied token budget, when requested.
    pub max_tokens: Option<u32>,
    /// Caller-supplied request ID echoed by CASS.
    pub request_id: Option<String>,
    /// Pagination cursor for the current page.
    pub cursor: Option<String>,
    /// True when CASS had to clamp the requested hit count.
    pub hits_clamped: bool,
    /// Search result hits.
    pub hits: Vec<CassSearchHit>,
    /// Aggregation buckets keyed by aggregation name.
    pub aggregations: Option<BTreeMap<String, Vec<CassAggregationBucket>>>,
    /// CASS warning string when degraded data is still returned.
    #[serde(rename = "_warning")]
    pub warning: Option<String>,
    /// Robot metadata emitted by `--robot-meta`.
    #[serde(rename = "_meta")]
    pub meta: CassSearchMeta,
    /// Query suggestions, left as documented extension objects.
    #[serde(default)]
    pub suggestions: Vec<JsonValue>,
    /// Query explanation object from `--explain`, when present.
    pub explanation: Option<JsonValue>,
    /// Timeout diagnostic object, when CASS returns partial results.
    #[serde(rename = "_timeout")]
    pub timeout: Option<JsonValue>,
}

/// Top-level field names defined by CASS search contract v1.
///
/// Used by [`CassSearchResponse::from_robot_json`] to detect (and log)
/// unknown extras under the forward-compat policy.
const KNOWN_CASS_SEARCH_RESPONSE_FIELDS: &[&str] = &[
    "query",
    "limit",
    "offset",
    "count",
    "total_matches",
    "max_tokens",
    "request_id",
    "cursor",
    "hits_clamped",
    "hits",
    "aggregations",
    "_warning",
    "_meta",
    "suggestions",
    "explanation",
    "_timeout",
];

impl CassSearchResponse {
    /// Parse a `cass search --robot` JSON payload into the typed contract.
    ///
    /// Unknown top-level fields are accepted (and emitted at `tracing::debug`)
    /// so newer CASS versions remain forward-compatible with older `ee`
    /// builds. See the type-level docstring for the policy rationale.
    ///
    /// # Errors
    ///
    /// Returns [`CassError::InvalidStdoutJson`] when stdout is not valid JSON
    /// or when a known field has an incompatible shape (wrong type).
    pub fn from_robot_json(input: &[u8]) -> Result<Self, CassError> {
        let parsed: Self =
            serde_json::from_slice(input).map_err(|error| CassError::InvalidStdoutJson {
                hint: format!("search robot JSON did not match documented CASS contract: {error}"),
            })?;

        // Best-effort drift-detection: re-parse as a generic Value and report
        // any top-level keys outside the documented contract at debug. We
        // ignore parse failures here because they cannot occur on input that
        // already deserialized successfully above.
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<JsonValue>(input) {
            let unknown: Vec<&str> = map
                .keys()
                .map(String::as_str)
                .filter(|key| !KNOWN_CASS_SEARCH_RESPONSE_FIELDS.contains(key))
                .collect();
            if !unknown.is_empty() {
                tracing::debug!(
                    target: "ee::cass",
                    unknown_fields = ?unknown,
                    "cass search response contained fields outside the documented contract; \
                     ignoring under forward-compat policy"
                );
            }
        }

        Ok(parsed)
    }
}

/// One hit from `cass search --robot`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CassSearchHit {
    /// Session source path containing the hit.
    pub source_path: String,
    /// 1-indexed line number when the hit maps to a specific line.
    pub line_number: Option<u32>,
    /// Agent that produced the session.
    #[serde(deserialize_with = "deserialize_agent")]
    pub agent: CassAgent,
    /// Current workspace path.
    pub workspace: Option<String>,
    /// Original workspace path before remote path mapping.
    pub workspace_original: Option<String>,
    /// Human-readable title, when available.
    pub title: Option<String>,
    /// Full content field, when requested.
    pub content: Option<String>,
    /// Snippet field, when requested.
    pub snippet: Option<String>,
    /// CASS score for the hit.
    pub score: Option<f64>,
    /// Creation timestamp as reported by the source connector.
    pub created_at: Option<CassTimestamp>,
    /// Match type label.
    pub match_type: Option<String>,
    /// Source identifier, for example `local`.
    pub source_id: String,
    /// Origin kind, for example `local` or `ssh`.
    pub origin_kind: String,
    /// Host label for remote sources.
    pub origin_host: Option<String>,
}

/// CASS timestamp fields may be integer epochs or strings depending on source.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum CassTimestamp {
    /// Numeric timestamp.
    Integer(i64),
    /// String timestamp.
    String(String),
}

/// One aggregation bucket from a CASS search response.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct CassAggregationBucket {
    /// Bucket key.
    pub key: String,
    /// Bucket hit count.
    pub count: u64,
}

/// Robot metadata from `cass search --robot-meta`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CassSearchMeta {
    /// Total elapsed time in milliseconds.
    pub elapsed_ms: u64,
    /// Effective search mode.
    pub search_mode: Option<String>,
    /// Search mode requested by the caller.
    pub requested_search_mode: Option<String>,
    /// Whether CASS defaulted the requested mode.
    pub mode_defaulted: Option<bool>,
    /// Fallback tier used by degraded retrieval.
    pub fallback_tier: Option<String>,
    /// Fallback reason used by degraded retrieval.
    pub fallback_reason: Option<String>,
    /// Whether semantic refinement ran.
    pub semantic_refinement: Option<bool>,
    /// True when wildcard fallback was used.
    pub wildcard_fallback: bool,
    /// Search cache statistics.
    pub cache_stats: CassSearchCacheStats,
    /// Timing component breakdown.
    pub timing: CassSearchTiming,
    /// Estimated output tokens.
    pub tokens_estimated: Option<u32>,
    /// Applied token budget.
    pub max_tokens: Option<u32>,
    /// Request ID echoed by CASS.
    pub request_id: Option<String>,
    /// Next cursor for pagination.
    pub next_cursor: Option<String>,
    /// True when CASS had to clamp hits.
    pub hits_clamped: bool,
    /// Full CASS state snapshot; retained as JSON because it is large and
    /// reused by status parsing.
    pub state: JsonValue,
    /// Index freshness snapshot.
    pub index_freshness: CassIndexFreshness,
    /// Applied timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// True when the search timed out.
    pub timed_out: Option<bool>,
    /// True when timeout returned partial results.
    pub partial_results: Option<bool>,
    /// ANN diagnostics, when semantic ANN search was used.
    pub ann_stats: Option<JsonValue>,
}

/// Search cache statistics from CASS robot metadata.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct CassSearchCacheStats {
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Requested results missing after cache lookup.
    pub shortfall: u64,
}

/// Search timing breakdown from CASS robot metadata.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct CassSearchTiming {
    /// Core search time.
    pub search_ms: u64,
    /// Rerank time.
    pub rerank_ms: u64,
    /// Other time.
    pub other_ms: u64,
}

/// Index freshness subset embedded in CASS search metadata.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct CassIndexFreshness {
    /// Whether the index exists.
    pub exists: bool,
    /// Stable status string.
    pub status: String,
    /// Status reason.
    pub reason: Option<String>,
    /// Whether the index is fresh.
    pub fresh: bool,
    /// Last indexed timestamp.
    pub last_indexed_at: Option<String>,
    /// Index age in seconds.
    pub age_seconds: Option<u64>,
    /// Whether the index is stale.
    pub stale: bool,
    /// Staleness threshold in seconds.
    pub stale_threshold_seconds: u64,
    /// Whether a rebuild is active.
    pub rebuilding: bool,
    /// Pending session count.
    pub pending_sessions: u64,
}

fn deserialize_agent<'de, D>(deserializer: D) -> Result<CassAgent, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Ok(CassAgent::parse_lossy(&value))
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
        match normalized_cass_token(s).as_str() {
            "message" | "msg" => Self::Message,
            "tool_call" | "toolcall" | "tool_use" | "function_call" => Self::ToolCall,
            "tool_result" | "toolresult" | "function_result" => Self::ToolResult,
            "file" | "diff" | "file_history_snapshot" => Self::File,
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
        match normalized_cass_token(s).as_str() {
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
        if self.end_line < self.start_line {
            0
        } else {
            self.end_line
                .saturating_sub(self.start_line)
                .saturating_add(1)
        }
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
        let line_belongs_to_session = self.last_source_path.as_deref() == Some(source_path);
        self.last_source_path = Some(source_path.to_owned());
        if !line_belongs_to_session {
            self.last_line = None;
        }
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
        let processed = self.sessions_imported.saturating_add(self.sessions_skipped);
        (processed as f32 / self.sessions_discovered as f32) * 100.0
    }

    /// Whether the import is complete.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.sessions_discovered > 0
            && self.sessions_imported.saturating_add(self.sessions_skipped)
                >= self.sessions_discovered
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

    fn ensure_session_uri_error(input: &str, expected_reason: &'static str) -> TestResult {
        match normalize_cass_session_uri(input) {
            Ok(reference) => Err(format!(
                "{input}: expected error {expected_reason}, got Ok({reference:?})"
            )),
            Err(error) => ensure_equal(
                &error.reason(),
                &expected_reason,
                &format!("reason for {input:?}"),
            ),
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
            &CassAgent::parse_lossy("Claude Code"),
            &CassAgent::ClaudeCode,
            "space-separated claude code",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("ClaudeCode"),
            &CassAgent::ClaudeCode,
            "PascalCase claude code",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("CODEX"),
            &CassAgent::Codex,
            "CODEX uppercase",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy(" codex "),
            &CassAgent::Codex,
            "codex whitespace",
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
            &CassAgent::parse_lossy("Chat GPT"),
            &CassAgent::ChatGpt,
            "space-separated Chat GPT",
        )?;
        ensure_equal(
            &CassAgent::parse_lossy("ChatGpt"),
            &CassAgent::ChatGpt,
            "PascalCase ChatGpt",
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
            &CassSpanKind::parse_lossy("tool call"),
            &CassSpanKind::ToolCall,
            "space-separated tool call",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("ToolCall"),
            &CassSpanKind::ToolCall,
            "PascalCase tool call",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("tool_use"),
            &CassSpanKind::ToolCall,
            "tool_use alias",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy(" Tool_Result "),
            &CassSpanKind::ToolResult,
            "tool_result whitespace and case",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("tool result"),
            &CassSpanKind::ToolResult,
            "space-separated tool result",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("toolResult"),
            &CassSpanKind::ToolResult,
            "camelCase tool result",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("file"),
            &CassSpanKind::File,
            "file",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("fileHistorySnapshot"),
            &CassSpanKind::File,
            "camelCase file history snapshot",
        )?;
        ensure_equal(
            &CassSpanKind::parse_lossy("file history snapshot"),
            &CassSpanKind::File,
            "space-separated file history snapshot",
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
            &CassRole::parse_lossy("AI"),
            &CassRole::Assistant,
            "AI uppercase alias",
        )?;
        ensure_equal(
            &CassRole::parse_lossy(" Assistant "),
            &CassRole::Assistant,
            "assistant whitespace and case",
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
    fn cass_session_uri_rejects_signed_or_empty_line_tokens() -> TestResult {
        ensure_session_uri_error("cass-session://abc#L+1", "invalid_line_number")?;
        ensure_session_uri_error("cass-session://abc#L1-+2", "invalid_line_number")?;
        ensure_session_uri_error("cass-session://abc#L1-", "invalid_line_number")?;
        ensure_session_uri_error("cass-session://abc#L-L2", "invalid_line_number")
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
    fn cass_search_response_parses_robot_meta_contract() -> TestResult {
        let input = br#"{
          "query": "format before release",
          "limit": 2,
          "offset": 0,
          "count": 1,
          "total_matches": 1,
          "max_tokens": 200,
          "request_id": "ee-test-search-001",
          "cursor": null,
          "hits_clamped": false,
          "hits": [
            {
              "source_path": "/workspace/session-a.jsonl",
              "line_number": 42,
              "agent": "codex",
              "workspace": "/workspace",
              "workspace_original": "/remote/workspace",
              "title": "release prep",
              "content": "Run cargo fmt --check before release.",
              "snippet": "cargo fmt --check",
              "score": 1.5,
              "created_at": "2026-04-30T00:00:00Z",
              "match_type": "lexical",
              "source_id": "local",
              "origin_kind": "local",
              "origin_host": null
            }
          ],
          "aggregations": {"agent": [{"key": "codex", "count": 1}]},
          "_warning": null,
          "_meta": {
            "elapsed_ms": 12,
            "search_mode": "lexical",
            "requested_search_mode": "hybrid",
            "mode_defaulted": false,
            "fallback_tier": "lexical",
            "fallback_reason": "semantic context unavailable in fixture",
            "semantic_refinement": false,
            "wildcard_fallback": false,
            "cache_stats": {"hits": 0, "misses": 1, "shortfall": 0},
            "timing": {"search_ms": 9, "rerank_ms": 0, "other_ms": 3},
            "tokens_estimated": 24,
            "max_tokens": 200,
            "request_id": "ee-test-search-001",
            "next_cursor": null,
            "hits_clamped": false,
            "state": {"status": "fixture"},
            "index_freshness": {
              "exists": true,
              "status": "fresh",
              "reason": null,
              "fresh": true,
              "last_indexed_at": "2026-04-30T00:00:00Z",
              "age_seconds": 1,
              "stale": false,
              "stale_threshold_seconds": 300,
              "rebuilding": false,
              "pending_sessions": 0
            },
            "timeout_ms": 30000,
            "timed_out": false,
            "partial_results": false,
            "ann_stats": null
          },
          "suggestions": [{"query": "cargo fmt before release"}],
          "explanation": {"strategy": "fixture"},
          "_timeout": null
        }"#;

        let parsed =
            CassSearchResponse::from_robot_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&parsed.query.as_str(), &"format before release", "query")?;
        ensure_equal(&parsed.hits.len(), &1, "hit count")?;
        let hit = parsed
            .hits
            .first()
            .ok_or_else(|| "parsed response missing hit".to_string())?;
        ensure_equal(&hit.agent, &CassAgent::Codex, "hit agent")?;
        ensure_equal(
            &hit.workspace_original.as_deref(),
            &Some("/remote/workspace"),
            "workspace original",
        )?;
        ensure_equal(
            &parsed.meta.index_freshness.status.as_str(),
            &"fresh",
            "index freshness",
        )?;
        ensure_equal(&parsed.meta.cache_stats.misses, &1, "cache misses")?;
        ensure_equal(&parsed.meta.timing.search_ms, &9, "search timing")
    }

    #[test]
    fn cass_search_response_accepts_synthetic_future_fields_at_root() -> TestResult {
        // Forward-compat policy: a CASS upgrade that adds optional metadata
        // must not break parsing in older ee builds. The synthetic
        // `surprise`, `next_protocol_version`, and `experimental_*` fields
        // stand in for any future additions and must round-trip into the
        // typed view without affecting the documented fields.
        let input = br#"{
          "query": "format before release",
          "limit": 1,
          "offset": 0,
          "count": 0,
          "total_matches": 0,
          "cursor": null,
          "hits_clamped": false,
          "hits": [],
          "surprise": true,
          "next_protocol_version": "v2.5",
          "experimental_routing": {"shard": "alpha", "weight": 0.42},
          "_meta": {
            "elapsed_ms": 1,
            "search_mode": "lexical",
            "requested_search_mode": "lexical",
            "mode_defaulted": false,
            "fallback_tier": null,
            "fallback_reason": null,
            "semantic_refinement": false,
            "wildcard_fallback": false,
            "cache_stats": {"hits": 0, "misses": 0, "shortfall": 0},
            "timing": {"search_ms": 1, "rerank_ms": 0, "other_ms": 0},
            "tokens_estimated": null,
            "max_tokens": null,
            "request_id": null,
            "next_cursor": null,
            "hits_clamped": false,
            "state": {},
            "index_freshness": {
              "exists": false,
              "status": "missing",
              "reason": null,
              "fresh": false,
              "last_indexed_at": null,
              "age_seconds": null,
              "stale": false,
              "stale_threshold_seconds": 300,
              "rebuilding": false,
              "pending_sessions": 0
            },
            "timeout_ms": null,
            "timed_out": null,
            "partial_results": null,
            "ann_stats": null
          },
          "suggestions": [],
          "explanation": null,
          "_timeout": null
        }"#;

        let parsed = CassSearchResponse::from_robot_json(input)
            .map_err(|error| format!("undocumented future fields must NOT fail; got {error:?}"))?;
        ensure_equal(
            &parsed.query.as_str(),
            &"format before release",
            "query echo",
        )?;
        ensure_equal(&parsed.limit, &1, "limit echo")?;
        ensure_equal(
            &parsed.meta.search_mode.as_deref(),
            &Some("lexical"),
            "meta search_mode echo",
        )
    }

    #[test]
    fn cass_search_response_accepts_synthetic_future_fields_in_nested_meta() -> TestResult {
        // Forward-compat must reach into nested structs too: a future CASS
        // could add fields to _meta, _meta.cache_stats, _meta.timing, or
        // _meta.index_freshness. None of those should break parsing.
        let input = br#"{
          "query": "q",
          "limit": 0,
          "offset": 0,
          "count": 0,
          "total_matches": 0,
          "cursor": null,
          "hits_clamped": false,
          "hits": [],
          "_meta": {
            "elapsed_ms": 1,
            "wildcard_fallback": false,
            "cache_stats": {
              "hits": 0,
              "misses": 0,
              "shortfall": 0,
              "future_eviction_count": 7
            },
            "timing": {
              "search_ms": 1,
              "rerank_ms": 0,
              "other_ms": 0,
              "telemetry_emit_ms": 12
            },
            "state": {},
            "index_freshness": {
              "exists": true,
              "status": "fresh",
              "fresh": true,
              "stale": false,
              "stale_threshold_seconds": 300,
              "rebuilding": false,
              "pending_sessions": 0,
              "next_index_version": "v3.1"
            },
            "hits_clamped": false,
            "vendor_tag": "cass-canary"
          }
        }"#;

        let parsed = CassSearchResponse::from_robot_json(input)
            .map_err(|error| format!("nested future fields must NOT fail; got {error:?}"))?;
        ensure_equal(
            &parsed.meta.cache_stats.misses,
            &0,
            "nested cache_stats parsed",
        )?;
        ensure_equal(&parsed.meta.timing.search_ms, &1, "nested timing parsed")?;
        ensure_equal(
            &parsed.meta.index_freshness.status.as_str(),
            &"fresh",
            "nested index_freshness parsed",
        )
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
    fn cass_view_span_line_count_rejects_inverted_ranges() -> TestResult {
        let span = CassViewSpan::new(
            "/session.jsonl",
            "span-inverted",
            CassSpanKind::Message,
            15,
            10,
            "content",
            "hash",
        );
        ensure_equal(&span.line_count(), &0, "inverted line range has no lines")
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
    fn import_cursor_preserves_line_for_completed_session() {
        let mut cursor = ImportCursor::new();

        cursor.record_discovered();
        cursor.record_span("/session1.jsonl", 42);
        cursor.record_imported("/session1.jsonl");
        assert_eq!(cursor.last_source_path.as_deref(), Some("/session1.jsonl"));
        assert_eq!(cursor.last_line, Some(42));

        cursor.record_discovered();
        cursor.record_imported("/session2.jsonl");
        assert_eq!(cursor.last_source_path.as_deref(), Some("/session2.jsonl"));
        assert_eq!(cursor.last_line, None);
    }

    #[test]
    fn import_cursor_completion_percent_handles_zero() {
        let cursor = ImportCursor::new();
        assert!((cursor.completion_percent() - 0.0).abs() < 0.01);
    }

    #[test]
    fn import_cursor_processed_count_math_saturates() {
        let mut cursor = ImportCursor::new();
        cursor.sessions_discovered = u32::MAX;
        cursor.sessions_imported = u32::MAX;
        cursor.sessions_skipped = 1;

        assert!(cursor.is_complete());
        assert!((cursor.completion_percent() - 100.0).abs() < 0.01);
    }

    #[test]
    fn import_cursor_is_complete_truth_table() {
        // bd-jlv54: pin every branch of the AND expression in is_complete.
        // Default cursor (all zeros) -> false. Guards against a regression
        // where `sessions_discovered > 0` is weakened to `>= 0`.
        let empty = ImportCursor::new();
        assert!(!empty.is_complete(), "default cursor must not be complete");

        // discovered = 0 but imported > 0 -> still false (guard branch).
        let mut imported_without_discovery = ImportCursor::new();
        imported_without_discovery.sessions_imported = 5;
        assert!(
            !imported_without_discovery.is_complete(),
            "imported>0 with discovered=0 must not be complete",
        );

        // Underfill: imported + skipped == discovered - 1 -> false.
        let mut underfilled = ImportCursor::new();
        underfilled.sessions_discovered = 3;
        underfilled.sessions_imported = 1;
        underfilled.sessions_skipped = 1;
        assert!(
            !underfilled.is_complete(),
            "imported+skipped < discovered must not be complete",
        );

        // Exact boundary: imported + skipped == discovered -> true.
        let mut boundary = ImportCursor::new();
        boundary.sessions_discovered = 3;
        boundary.sessions_imported = 2;
        boundary.sessions_skipped = 1;
        assert!(
            boundary.is_complete(),
            "imported+skipped == discovered must be complete",
        );

        // Overcount: imported + skipped > discovered -> still true.
        // The predicate uses `>=`, not `==`, so over-reported counters
        // (e.g. duplicate record_imported calls) keep is_complete true.
        let mut overcounted = ImportCursor::new();
        overcounted.sessions_discovered = 2;
        overcounted.sessions_imported = 3;
        overcounted.sessions_skipped = 1;
        assert!(
            overcounted.is_complete(),
            "imported+skipped > discovered must remain complete",
        );
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
