//! CASS import execution (EE-107).
//!
//! This module is the first executable import slice: discover sessions
//! through CASS' robot JSON surface, optionally persist imported session
//! rows, capture first-line evidence spans, and update the resumable
//! import ledger.

use std::fmt;
use std::path::{Path, PathBuf};

use blake3;
use chrono::Utc;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use super::{
    CassAgent, CassClient, CassError, CassExitClass, CassRole, CassSessionInfo, CassSpanKind,
    ImportCursor,
};
use crate::db::{
    CreateEvidenceSpanInput, CreateImportLedgerInput, CreateSessionInput, CreateWorkspaceInput,
    DatabaseConfig, DbConnection, DbError, UpdateImportLedgerInput,
};
use crate::models::{EvidenceId, SessionId, WorkspaceId};

const DEFAULT_DB_FILE: &str = "ee.db";
const DEFAULT_VIEW_CONTEXT: u32 = 4;
const IMPORT_SOURCE_KIND: &str = "cass";

/// Options for one `ee import cass` run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportOptions {
    /// Workspace path passed to `cass sessions --workspace`.
    pub workspace_path: PathBuf,
    /// Database path to write; defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<PathBuf>,
    /// Maximum sessions to ask CASS to return.
    pub limit: u32,
    /// If true, query CASS but do not create files or write the DB.
    pub dry_run: bool,
    /// If true, import first-window evidence spans through `cass view`.
    pub include_spans: bool,
}

impl CassImportOptions {
    /// Build options with the project defaults.
    #[must_use]
    pub fn new(workspace_path: impl Into<PathBuf>) -> Self {
        Self {
            workspace_path: workspace_path.into(),
            database_path: None,
            limit: 10,
            dry_run: false,
            include_spans: true,
        }
    }
}

/// Per-session import result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedCassSession {
    pub source_path: String,
    pub session_id: Option<String>,
    pub status: ImportSessionStatus,
    pub spans_imported: u32,
    pub message_count: Option<u32>,
}

/// Stable per-session status string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportSessionStatus {
    Imported,
    Skipped,
    WouldImport,
}

impl ImportSessionStatus {
    /// Stable machine string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::Skipped => "skipped",
            Self::WouldImport => "would_import",
        }
    }
}

/// Summary returned by `ee import cass`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportReport {
    pub schema: &'static str,
    pub workspace_path: String,
    pub database_path: Option<String>,
    pub source_id: String,
    pub ledger_id: Option<String>,
    pub dry_run: bool,
    pub sessions_discovered: u32,
    pub sessions_imported: u32,
    pub sessions_skipped: u32,
    pub spans_imported: u32,
    pub status: String,
    pub sessions: Vec<ImportedCassSession>,
}

impl CassImportReport {
    /// Render the stable JSON data payload for the response envelope.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "workspacePath": self.workspace_path,
            "databasePath": self.database_path,
            "sourceId": self.source_id,
            "ledgerId": self.ledger_id,
            "dryRun": self.dry_run,
            "sessionsDiscovered": self.sessions_discovered,
            "sessionsImported": self.sessions_imported,
            "sessionsSkipped": self.sessions_skipped,
            "spansImported": self.spans_imported,
            "status": self.status,
            "sessions": self.sessions.iter().map(|session| {
                json!({
                    "sourcePath": session.source_path,
                    "sessionId": session.session_id,
                    "status": session.status.as_str(),
                    "spansImported": session.spans_imported,
                    "messageCount": session.message_count,
                })
            }).collect::<Vec<_>>(),
        })
    }

    /// Render a compact human summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{mode}CASS import {status}: {imported} imported, {skipped} skipped, {spans} spans from {discovered} discovered sessions\n",
            status = self.status,
            imported = self.sessions_imported,
            skipped = self.sessions_skipped,
            spans = self.spans_imported,
            discovered = self.sessions_discovered,
        )
    }
}

/// Error produced by CASS import.
#[derive(Debug)]
pub enum CassImportError {
    Cass(CassError),
    CassCommand {
        command: String,
        exit_code: Option<i32>,
        stderr: String,
    },
    InvalidJson {
        source: &'static str,
        message: String,
    },
    Io {
        path: PathBuf,
        message: String,
    },
    Storage(DbError),
}

impl CassImportError {
    /// Return a useful repair hint when one is known.
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Cass(error) => error.repair_hint(),
            Self::CassCommand { .. } => Some("run cass health --json"),
            Self::InvalidJson { .. } => Some("run cass api-version --json and cass doctor --json"),
            Self::Io { .. } => Some("check workspace and database path permissions"),
            Self::Storage(_) => Some("ee db migrate --workspace ."),
        }
    }
}

impl fmt::Display for CassImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cass(error) => write!(formatter, "{error}"),
            Self::CassCommand {
                command,
                exit_code,
                stderr,
            } => write!(
                formatter,
                "cass command `{command}` failed with exit {exit_code:?}: {stderr}",
            ),
            Self::InvalidJson { source, message } => {
                write!(formatter, "invalid CASS {source} JSON: {message}")
            }
            Self::Io { path, message } => {
                write!(formatter, "I/O error at {}: {message}", path.display())
            }
            Self::Storage(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CassImportError {}

impl From<CassError> for CassImportError {
    fn from(error: CassError) -> Self {
        Self::Cass(error)
    }
}

impl From<DbError> for CassImportError {
    fn from(error: DbError) -> Self {
        Self::Storage(error)
    }
}

/// Run one CASS import operation.
///
/// # Errors
///
/// Returns [`CassImportError`] for CASS process failures, malformed JSON,
/// filesystem setup failures, or storage errors.
pub fn import_cass_sessions(
    client: &CassClient,
    options: &CassImportOptions,
) -> Result<CassImportReport, CassImportError> {
    let workspace_path = normalize_path(&options.workspace_path);
    let source_id = source_id(&workspace_path, options.limit);
    let sessions = discover_sessions(client, &workspace_path, options.limit)?;

    if options.dry_run {
        return Ok(dry_run_report(workspace_path, source_id, sessions));
    }

    let database_path = database_path(options);
    ensure_database_parent(&database_path)?;
    let connection = DbConnection::open(DatabaseConfig::file(database_path.clone()))?;
    connection.migrate()?;
    let workspace_id = ensure_workspace(&connection, &workspace_path)?;
    let ledger_id = ensure_running_ledger(&connection, &workspace_id, &source_id)?;

    let mut cursor = ImportCursor::new();
    let mut session_reports = Vec::with_capacity(sessions.len());
    let mut imported = 0_u32;
    let mut skipped = 0_u32;
    let mut spans_imported = 0_u32;

    for session in sessions {
        cursor.record_discovered();
        if let Some(existing) =
            connection.get_session_by_cass_id(&workspace_id, &session.source_path)?
        {
            cursor.record_skipped();
            skipped = skipped.saturating_add(1);
            session_reports.push(ImportedCassSession {
                source_path: session.source_path,
                session_id: Some(existing.id),
                status: ImportSessionStatus::Skipped,
                spans_imported: 0,
                message_count: session.message_count,
            });
            continue;
        }

        let session_id = stable_session_id(&session.source_path);
        connection.insert_session(&session_id, &session_input(&workspace_id, &session))?;

        let mut session_spans = 0_u32;
        if options.include_spans {
            let spans = view_session_spans(client, &session.source_path)?;
            for span in spans {
                connection.insert_evidence_span(
                    &stable_evidence_id(&session_id, &span.cass_span_id),
                    &evidence_input(&workspace_id, &session_id, &span),
                )?;
                cursor.record_span(&session.source_path, span.end_line);
                session_spans = session_spans.saturating_add(1);
                spans_imported = spans_imported.saturating_add(1);
            }
        }

        cursor.record_imported(&session.source_path);
        imported = imported.saturating_add(1);
        session_reports.push(ImportedCassSession {
            source_path: session.source_path,
            session_id: Some(session_id),
            status: ImportSessionStatus::Imported,
            spans_imported: session_spans,
            message_count: session.message_count,
        });
    }

    complete_ledger(
        &connection,
        &ledger_id,
        &cursor,
        imported,
        spans_imported,
        None,
    )?;

    Ok(CassImportReport {
        schema: "ee.import.cass.v1",
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        database_path: Some(database_path.to_string_lossy().into_owned()),
        source_id,
        ledger_id: Some(ledger_id),
        dry_run: false,
        sessions_discovered: cursor.sessions_discovered,
        sessions_imported: imported,
        sessions_skipped: skipped,
        spans_imported,
        status: "completed".to_string(),
        sessions: session_reports,
    })
}

fn discover_sessions(
    client: &CassClient,
    workspace_path: &Path,
    limit: u32,
) -> Result<Vec<CassSessionInfo>, CassImportError> {
    let invocation = client.invocation([
        "sessions".to_string(),
        "--workspace".to_string(),
        workspace_path.to_string_lossy().into_owned(),
        "--json".to_string(),
        "--limit".to_string(),
        limit.to_string(),
    ]);
    let outcome = client.run(&invocation)?;
    ensure_successful_outcome(&outcome, "cass sessions")?;
    parse_sessions_json(outcome.stdout_bytes())
}

fn view_session_spans(
    client: &CassClient,
    source_path: &str,
) -> Result<Vec<CassViewSpanForImport>, CassImportError> {
    let invocation = client.invocation([
        "view".to_string(),
        source_path.to_string(),
        "-n".to_string(),
        "1".to_string(),
        "-C".to_string(),
        DEFAULT_VIEW_CONTEXT.to_string(),
        "--json".to_string(),
    ]);
    let outcome = client.run(&invocation)?;
    ensure_successful_outcome(&outcome, "cass view")?;
    parse_view_json(outcome.stdout_bytes(), source_path)
}

fn ensure_successful_outcome(
    outcome: &super::CassOutcome,
    command: &str,
) -> Result<(), CassImportError> {
    if matches!(
        outcome.class(),
        CassExitClass::Success | CassExitClass::Degraded
    ) && !outcome.stdout_is_empty()
    {
        return Ok(());
    }
    Err(CassImportError::CassCommand {
        command: command.to_string(),
        exit_code: outcome.exit_code(),
        stderr: outcome.stderr_utf8_lossy().trim().to_string(),
    })
}

fn parse_sessions_json(input: &[u8]) -> Result<Vec<CassSessionInfo>, CassImportError> {
    let value: JsonValue =
        serde_json::from_slice(input).map_err(|error| CassImportError::InvalidJson {
            source: "sessions",
            message: error.to_string(),
        })?;
    let sessions = value
        .get("sessions")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| CassImportError::InvalidJson {
            source: "sessions",
            message: "missing sessions array".to_string(),
        })?;

    let mut parsed = Vec::with_capacity(sessions.len());
    for item in sessions {
        let path = required_string(item, "path", "sessions")?;
        let mut session = CassSessionInfo::new(path.clone());
        if let Some(agent) = item.get("agent").and_then(JsonValue::as_str) {
            session.agent = agent.parse().unwrap_or(CassAgent::Unknown);
        }
        session.workspace_dir = item
            .get("workspace")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        session.started_at = item
            .get("started_at")
            .or_else(|| item.get("started"))
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        session.ended_at = item
            .get("ended_at")
            .or_else(|| item.get("modified"))
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        session.message_count = item
            .get("message_count")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        session.token_count = item
            .get("token_count")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        session.content_hash = item
            .get("content_hash")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .or_else(|| Some(content_hash_for_session(item, &path)));
        parsed.push(session);
    }
    Ok(parsed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CassViewSpanForImport {
    cass_span_id: String,
    span_kind: CassSpanKind,
    start_line: u32,
    end_line: u32,
    role: Option<CassRole>,
    excerpt: String,
    content_hash: String,
}

fn parse_view_json(
    input: &[u8],
    source_path: &str,
) -> Result<Vec<CassViewSpanForImport>, CassImportError> {
    let value: JsonValue =
        serde_json::from_slice(input).map_err(|error| CassImportError::InvalidJson {
            source: "view",
            message: error.to_string(),
        })?;
    let lines = value
        .get("lines")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| CassImportError::InvalidJson {
            source: "view",
            message: "missing lines array".to_string(),
        })?;

    let mut spans = Vec::with_capacity(lines.len());
    for line in lines {
        let line_number = line
            .get("line")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| CassImportError::InvalidJson {
                source: "view",
                message: "line entry missing numeric line".to_string(),
            })?;
        let content = required_string(line, "content", "view")?;
        let (span_kind, role) = classify_line(&content);
        let excerpt = truncate_excerpt(&content, 65_536);
        spans.push(CassViewSpanForImport {
            cass_span_id: format!("{source_path}:{line_number}"),
            span_kind,
            start_line: line_number,
            end_line: line_number,
            role,
            content_hash: blake3_hex(&excerpt),
            excerpt,
        });
    }
    Ok(spans)
}

fn classify_line(content: &str) -> (CassSpanKind, Option<CassRole>) {
    let Ok(value) = serde_json::from_str::<JsonValue>(content) else {
        return (CassSpanKind::Message, None);
    };
    let line_type = value
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let span_kind = match line_type {
        "tool_use" | "tool-call" | "tool_call" => CassSpanKind::ToolCall,
        "tool_result" | "tool-result" => CassSpanKind::ToolResult,
        "summary" => CassSpanKind::Summary,
        "file" | "file-history-snapshot" => CassSpanKind::File,
        _ => CassSpanKind::Message,
    };
    let role = value
        .get("message")
        .and_then(|message| message.get("role"))
        .and_then(JsonValue::as_str)
        .or_else(|| value.get("role").and_then(JsonValue::as_str))
        .and_then(|role| role.parse().ok());
    (span_kind, role)
}

fn dry_run_report(
    workspace_path: PathBuf,
    source_id: String,
    sessions: Vec<CassSessionInfo>,
) -> CassImportReport {
    CassImportReport {
        schema: "ee.import.cass.v1",
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        database_path: None,
        source_id,
        ledger_id: None,
        dry_run: true,
        sessions_discovered: saturating_len(sessions.len()),
        sessions_imported: 0,
        sessions_skipped: 0,
        spans_imported: 0,
        status: "dry_run".to_string(),
        sessions: sessions
            .into_iter()
            .map(|session| ImportedCassSession {
                source_path: session.source_path,
                session_id: None,
                status: ImportSessionStatus::WouldImport,
                spans_imported: 0,
                message_count: session.message_count,
            })
            .collect(),
    }
}

fn ensure_workspace(connection: &DbConnection, workspace_path: &Path) -> Result<String, DbError> {
    let path = workspace_path.to_string_lossy().into_owned();
    if let Some(existing) = connection.get_workspace_by_path(&path)? {
        return Ok(existing.id);
    }
    let id = stable_workspace_id(&path);
    connection.insert_workspace(
        &id,
        &CreateWorkspaceInput {
            path,
            name: workspace_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        },
    )?;
    Ok(id)
}

fn ensure_running_ledger(
    connection: &DbConnection,
    workspace_id: &str,
    source_id: &str,
) -> Result<String, DbError> {
    let now = Utc::now().to_rfc3339();
    if let Some(existing) =
        connection.get_import_ledger_by_source(workspace_id, IMPORT_SOURCE_KIND, source_id)?
    {
        let _ = connection.update_import_ledger(
            &existing.id,
            &UpdateImportLedgerInput {
                status: "running".to_string(),
                cursor_json: existing.cursor_json,
                imported_session_count: existing.imported_session_count,
                imported_span_count: existing.imported_span_count,
                attempt_count: existing.attempt_count.saturating_add(1),
                error_code: None,
                error_message: None,
                started_at: Some(now),
                completed_at: None,
            },
        )?;
        return Ok(existing.id);
    }

    let id = stable_import_id(source_id);
    connection.insert_import_ledger(
        &id,
        &CreateImportLedgerInput {
            workspace_id: workspace_id.to_string(),
            source_kind: IMPORT_SOURCE_KIND.to_string(),
            source_id: source_id.to_string(),
            status: "running".to_string(),
            cursor_json: Some(json!({"sessionsDiscovered": 0}).to_string()),
            imported_session_count: 0,
            imported_span_count: 0,
            attempt_count: 1,
            error_code: None,
            error_message: None,
            started_at: Some(now),
            completed_at: None,
            metadata_json: Some(json!({"schema":"ee.import_ledger.cass.v1"}).to_string()),
        },
    )?;
    Ok(id)
}

fn complete_ledger(
    connection: &DbConnection,
    ledger_id: &str,
    cursor: &ImportCursor,
    imported_sessions: u32,
    imported_spans: u32,
    error: Option<&CassImportError>,
) -> Result<(), DbError> {
    let status = if error.is_some() {
        "failed"
    } else {
        "completed"
    };
    let now = Utc::now().to_rfc3339();
    let _ = connection.update_import_ledger(
        ledger_id,
        &UpdateImportLedgerInput {
            status: status.to_string(),
            cursor_json: Some(
                json!({
                    "lastSourcePath": cursor.last_source_path,
                    "lastLine": cursor.last_line,
                    "sessionsDiscovered": cursor.sessions_discovered,
                    "sessionsImported": cursor.sessions_imported,
                    "sessionsSkipped": cursor.sessions_skipped,
                    "spansImported": cursor.spans_imported,
                    "complete": cursor.is_complete(),
                })
                .to_string(),
            ),
            imported_session_count: imported_sessions,
            imported_span_count: imported_spans,
            attempt_count: 1,
            error_code: error.map(|err| error_code(err).to_string()),
            error_message: error.map(ToString::to_string),
            started_at: None,
            completed_at: Some(now),
        },
    )?;
    Ok(())
}

fn error_code(error: &CassImportError) -> &'static str {
    match error {
        CassImportError::Cass(_) => "cass",
        CassImportError::CassCommand { .. } => "cass_command",
        CassImportError::InvalidJson { .. } => "invalid_json",
        CassImportError::Io { .. } => "io",
        CassImportError::Storage(_) => "storage",
    }
}

fn session_input(workspace_id: &str, session: &CassSessionInfo) -> CreateSessionInput {
    CreateSessionInput {
        workspace_id: workspace_id.to_string(),
        cass_session_id: session.source_path.clone(),
        source_path: Some(session.source_path.clone()),
        agent_name: Some(session.agent.as_str().to_string()),
        model: None,
        started_at: session.started_at.clone(),
        ended_at: session.ended_at.clone(),
        message_count: session.message_count.unwrap_or_default(),
        token_count: session.token_count,
        content_hash: session
            .content_hash
            .clone()
            .unwrap_or_else(|| blake3_hex(&session.source_path)),
        metadata_json: Some(
            json!({
                "schema": "ee.cass_session.v1",
                "workspaceDir": session.workspace_dir,
            })
            .to_string(),
        ),
    }
}

fn evidence_input(
    workspace_id: &str,
    session_id: &str,
    span: &CassViewSpanForImport,
) -> CreateEvidenceSpanInput {
    CreateEvidenceSpanInput {
        workspace_id: workspace_id.to_string(),
        session_id: session_id.to_string(),
        memory_id: None,
        cass_span_id: span.cass_span_id.clone(),
        span_kind: span.span_kind.as_str().to_string(),
        start_line: span.start_line,
        end_line: span.end_line,
        start_byte: None,
        end_byte: None,
        role: span.role.map(|role| role.as_str().to_string()),
        excerpt: span.excerpt.clone(),
        content_hash: span.content_hash.clone(),
        metadata_json: Some(json!({"schema":"ee.cass_evidence_span.v1"}).to_string()),
    }
}

fn required_string(
    value: &JsonValue,
    field: &'static str,
    source: &'static str,
) -> Result<String, CassImportError> {
    value
        .get(field)
        .and_then(JsonValue::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| CassImportError::InvalidJson {
            source,
            message: format!("missing non-empty {field}"),
        })
}

fn content_hash_for_session(item: &JsonValue, path: &str) -> String {
    let modified = item
        .get("modified")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let size = item
        .get("size_bytes")
        .and_then(JsonValue::as_u64)
        .unwrap_or_default();
    blake3_hex(&format!("{path}\n{modified}\n{size}"))
}

fn database_path(options: &CassImportOptions) -> PathBuf {
    options.database_path.clone().unwrap_or_else(|| {
        options
            .workspace_path
            .join(crate::config::WORKSPACE_MARKER)
            .join(DEFAULT_DB_FILE)
    })
}

fn ensure_database_parent(path: &Path) -> Result<(), CassImportError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| CassImportError::Io {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn source_id(workspace_path: &Path, limit: u32) -> String {
    format!(
        "cass://sessions?workspace={}&limit={limit}",
        workspace_path.to_string_lossy()
    )
}

fn stable_workspace_id(path: &str) -> String {
    WorkspaceId::from_uuid(stable_uuid(&format!("workspace:{path}"))).to_string()
}

fn stable_session_id(source_path: &str) -> String {
    SessionId::from_uuid(stable_uuid(&format!("session:{source_path}"))).to_string()
}

fn stable_evidence_id(session_id: &str, span_id: &str) -> String {
    EvidenceId::from_uuid(stable_uuid(&format!("evidence:{session_id}:{span_id}"))).to_string()
}

fn stable_import_id(source_id: &str) -> String {
    let hash = blake3_hex(&format!("import:{source_id}"));
    format!("imp_{}", &hash[..26])
}

fn stable_uuid(input: &str) -> Uuid {
    let hash = blake3::hash(input.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    Uuid::from_bytes(bytes)
}

fn blake3_hex(input: &str) -> String {
    blake3::hash(input.as_bytes()).to_hex().to_string()
}

fn truncate_excerpt(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut end = 0;
    for (index, ch) in input.char_indices() {
        let next = index + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    input[..end].to_string()
}

fn saturating_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn parses_cass_sessions_json_contract() -> TestResult {
        let input = br#"{
          "sessions": [
            {
              "path": "/tmp/session.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "modified": "2026-04-30T00:00:00Z",
              "message_count": 12,
              "token_count": 345
            }
          ]
        }"#;

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&sessions.len(), &1, "session count")?;
        let first = sessions
            .first()
            .ok_or_else(|| "missing parsed session".to_string())?;
        ensure_equal(&first.source_path.as_str(), &"/tmp/session.jsonl", "path")?;
        ensure_equal(&first.agent, &CassAgent::Codex, "agent")?;
        ensure_equal(
            &first.workspace_dir.as_deref(),
            &Some("/tmp/project"),
            "workspace",
        )?;
        ensure_equal(&first.message_count, &Some(12), "message_count")?;
        ensure(first.content_hash.is_some(), "content hash filled")
    }

    #[test]
    fn parses_view_lines_into_import_spans() -> TestResult {
        let input = br#"{
          "path": "/tmp/session.jsonl",
          "lines": [
            {"line": 3, "content": "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}"},
            {"line": 4, "content": "{\"type\":\"tool_result\",\"role\":\"tool\"}"}
          ]
        }"#;

        let spans =
            parse_view_json(input, "/tmp/session.jsonl").map_err(|error| error.to_string())?;
        ensure_equal(&spans.len(), &2, "span count")?;
        ensure_equal(&spans[0].start_line, &3, "first line")?;
        ensure_equal(&spans[0].role, &Some(CassRole::User), "user role")?;
        ensure_equal(
            &spans[1].span_kind,
            &CassSpanKind::ToolResult,
            "tool result kind",
        )?;
        ensure_equal(&spans[1].role, &Some(CassRole::Tool), "tool role")
    }

    #[test]
    fn dry_run_report_has_no_side_effect_targets() -> TestResult {
        let sessions = vec![CassSessionInfo::new("/tmp/a.jsonl")];
        let report = dry_run_report(PathBuf::from("/tmp/work"), "cass://x".to_string(), sessions);

        ensure_equal(&report.dry_run, &true, "dry run")?;
        ensure_equal(&report.database_path, &None, "no database path")?;
        ensure_equal(&report.sessions_discovered, &1, "discovered")?;
        ensure_equal(
            &report.sessions[0].status,
            &ImportSessionStatus::WouldImport,
            "would import",
        )
    }

    #[test]
    fn stable_ids_match_storage_constraints() -> TestResult {
        let workspace_id = stable_workspace_id("/tmp/work");
        let session_id = stable_session_id("/tmp/session.jsonl");
        let evidence_id = stable_evidence_id(&session_id, "span-1");
        let import_id = stable_import_id("cass://sessions?workspace=/tmp/work&limit=10");

        ensure(
            workspace_id.starts_with("wsp_") && workspace_id.len() == 30,
            "workspace id shape",
        )?;
        ensure(
            session_id.starts_with("sess_") && session_id.len() == 31,
            "session id shape",
        )?;
        ensure(
            evidence_id.starts_with("ev_") && evidence_id.len() == 29,
            "evidence id shape",
        )?;
        ensure(
            import_id.starts_with("imp_") && import_id.len() == 30,
            "import id shape",
        )
    }
}
