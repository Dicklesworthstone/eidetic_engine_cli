//! CASS import execution (EE-107).
//!
//! This module is the first executable import slice: discover sessions
//! through CASS' robot JSON surface, optionally persist imported session
//! rows, capture first-line evidence spans, and update the resumable
//! import ledger.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use blake3;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value as JsonValue, json};
use sqlmodel_core::IsolationLevel;
use uuid::Uuid;

use super::{
    CassAgent, CassClient, CassError, CassExitClass, CassRole, CassSessionInfo, CassSpanKind,
    ImportCursor,
};
use crate::db::{
    CompleteImportLedgerInput, CreateAuditInput, CreateEvidenceSpanInput, CreateImportLedgerInput,
    CreateSearchIndexJobInput, CreateSessionInput, CreateWorkspaceInput, DatabaseConfig,
    DbConnection, DbError, DbOperation, SearchIndexJobType,
};
use crate::models::{
    AuditId, CASS_EVIDENCE_SPAN_SCHEMA_V1, CASS_SESSION_SCHEMA_V1, EvidenceId,
    IMPORT_CASS_SCHEMA_V1, IMPORT_LEDGER_CASS_SCHEMA_V1, SessionId, WorkspaceId,
};

const DEFAULT_DB_FILE: &str = "ee.db";
const DEFAULT_VIEW_CONTEXT: u32 = 4;
const IMPORT_SOURCE_KIND: &str = "cass";
const CASS_REDACTION_AUDIT_SCHEMA_V1: &str = "ee.cass.redaction_audit.v1";
const CASS_REDACTION_AUDIT_ACTION: &str = "cass.evidence.redacted";

/// Options for one `ee import cass` run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportOptions {
    /// Workspace path passed to `cass sessions --workspace`.
    pub workspace_path: PathBuf,
    /// Database path to write; defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<PathBuf>,
    /// Maximum sessions to ask CASS to return.
    pub limit: u32,
    /// Only import sessions whose start time is at or after this UTC cutoff.
    pub since: Option<DateTime<Utc>>,
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
            since: None,
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
    pub index_job_id: Option<String>,
    pub status: ImportSessionStatus,
    pub spans_imported: u32,
    pub message_count: Option<u32>,
    pub missing_metadata: Vec<String>,
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
    pub since: Option<String>,
    pub sessions_discovered: u32,
    pub sessions_imported: u32,
    pub sessions_skipped: u32,
    pub spans_imported: u32,
    pub index_jobs_queued: u32,
    pub index_required_action: Option<String>,
    pub status: String,
    pub sessions: Vec<ImportedCassSession>,
}

impl CassImportReport {
    /// Render the stable JSON data payload for the response envelope.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "import cass",
            "workspacePath": self.workspace_path,
            "databasePath": self.database_path,
            "sourceId": self.source_id,
            "ledgerId": self.ledger_id,
            "dryRun": self.dry_run,
            "since": self.since,
            "sessionsDiscovered": self.sessions_discovered,
            "sessionsImported": self.sessions_imported,
            "sessionsSkipped": self.sessions_skipped,
            "spansImported": self.spans_imported,
            "indexJobsQueued": self.index_jobs_queued,
            "indexRequiredAction": self.index_required_action,
            "status": self.status,
            "sessions": self.sessions.iter().map(|session| {
                json!({
                    "sourcePath": session.source_path,
                    "sessionId": session.session_id,
                    "indexJobId": session.index_job_id,
                    "status": session.status.as_str(),
                    "spansImported": session.spans_imported,
                    "messageCount": session.message_count,
                    "missingMetadata": session.missing_metadata,
                })
            }).collect::<Vec<_>>(),
        })
    }

    /// Render a compact human summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run { "DRY RUN: " } else { "" };
        let since = self
            .since
            .as_deref()
            .map_or_else(String::new, |cutoff| format!(" since {cutoff}"));
        format!(
            "{mode}CASS import {status}{since}: {imported} imported, {skipped} skipped, {spans} spans, {index_jobs} index jobs from {discovered} discovered sessions\n",
            status = self.status,
            imported = self.sessions_imported,
            skipped = self.sessions_skipped,
            spans = self.spans_imported,
            index_jobs = self.index_jobs_queued,
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
    InvalidSince {
        value: String,
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
            Self::InvalidSince { .. } => Some("use --since with a duration like 90d, 24h, or 7d3h"),
            Self::Io { .. } => Some("check workspace and database path permissions"),
            Self::Storage(_) => Some("ee init --workspace . --repair-plan"),
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
            Self::InvalidSince { value, message } => {
                write!(formatter, "invalid --since value `{value}`: {message}")
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

/// Bounded summary of CASS import parser output for fuzzing and parser
/// contract checks without exposing private storage-row construction types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportParseSummary {
    pub accepted_items: u32,
    pub max_line: u32,
    pub max_excerpt_bytes: usize,
}

/// Parse CASS `sessions --json` bytes through the same import parser used by
/// the real importer, returning bounded public facts for fuzz harnesses.
///
/// # Errors
///
/// Returns [`CassImportError::InvalidJson`] when CASS emits malformed or
/// structurally invalid session discovery JSON.
pub fn parse_sessions_json_summary(
    input: &[u8],
) -> Result<CassImportParseSummary, CassImportError> {
    let sessions = parse_sessions_json(input)?;
    Ok(CassImportParseSummary {
        accepted_items: saturating_len(sessions.len()),
        max_line: 0,
        max_excerpt_bytes: 0,
    })
}

/// Parse CASS `view --json` bytes through the same import parser used by the
/// real importer, returning bounded public facts for fuzz harnesses.
///
/// # Errors
///
/// Returns [`CassImportError::InvalidJson`] when CASS emits malformed or
/// structurally invalid view JSON.
pub fn parse_view_json_summary(
    input: &[u8],
    source_path: &str,
) -> Result<CassImportParseSummary, CassImportError> {
    let spans = parse_view_json(input, source_path)?;
    let max_line = spans
        .iter()
        .map(|span| span.end_line)
        .max()
        .unwrap_or_default();
    let max_excerpt_bytes = spans
        .iter()
        .map(|span| span.excerpt.len())
        .max()
        .unwrap_or_default();
    Ok(CassImportParseSummary {
        accepted_items: saturating_len(spans.len()),
        max_line,
        max_excerpt_bytes,
    })
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
    let since_cutoff = options.since.map(format_since_cutoff);
    let source_id = source_id(&workspace_path, options.limit, since_cutoff.as_deref());
    let sessions = filter_sessions_since(
        discover_sessions(client, &workspace_path, options.limit)?,
        options.since,
    )?;

    if options.dry_run {
        return Ok(dry_run_report(
            workspace_path,
            source_id,
            since_cutoff,
            sessions,
        ));
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
    let mut index_jobs_queued = 0_u32;

    let import_result: Result<(), CassImportError> = (|| {
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
                    index_job_id: None,
                    status: ImportSessionStatus::Skipped,
                    spans_imported: 0,
                    message_count: session.message_count,
                    missing_metadata: session.missing_metadata,
                });
                continue;
            }

            let spans = if options.include_spans {
                view_session_spans(client, &session.source_path)?
            } else {
                Vec::new()
            };

            match persist_session_import_if_absent(&connection, &workspace_id, &session, &spans)? {
                SessionImportPersistResult::Skipped { session_id } => {
                    cursor.record_skipped();
                    skipped = skipped.saturating_add(1);
                    session_reports.push(ImportedCassSession {
                        source_path: session.source_path,
                        session_id: Some(session_id),
                        index_job_id: None,
                        status: ImportSessionStatus::Skipped,
                        spans_imported: 0,
                        message_count: session.message_count,
                        missing_metadata: session.missing_metadata,
                    });
                    continue;
                }
                SessionImportPersistResult::Imported {
                    session_id,
                    index_job_id,
                } => {
                    for span in &spans {
                        cursor.record_span(&session.source_path, span.end_line);
                    }
                    let session_spans = saturating_len(spans.len());
                    spans_imported = spans_imported.saturating_add(session_spans);

                    cursor.record_imported(&session.source_path);
                    imported = imported.saturating_add(1);
                    index_jobs_queued = index_jobs_queued.saturating_add(1);
                    session_reports.push(ImportedCassSession {
                        source_path: session.source_path,
                        session_id: Some(session_id),
                        index_job_id: Some(index_job_id),
                        status: ImportSessionStatus::Imported,
                        spans_imported: session_spans,
                        message_count: session.message_count,
                        missing_metadata: session.missing_metadata,
                    });
                }
            }
        }
        Ok(())
    })();

    if let Err(error) = import_result {
        complete_ledger(
            &connection,
            &ledger_id,
            &cursor,
            imported,
            spans_imported,
            Some(&error),
        )?;
        return Err(error);
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
        schema: IMPORT_CASS_SCHEMA_V1,
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        database_path: Some(database_path.to_string_lossy().into_owned()),
        source_id,
        ledger_id: Some(ledger_id),
        dry_run: false,
        since: since_cutoff,
        sessions_discovered: cursor.sessions_discovered,
        sessions_imported: imported,
        sessions_skipped: skipped,
        spans_imported,
        index_jobs_queued,
        index_required_action: Some(index_required_action(&workspace_path, Some(&database_path))),
        status: "completed".to_string(),
        sessions: session_reports,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionImportPersistResult {
    Imported {
        session_id: String,
        index_job_id: String,
    },
    Skipped {
        session_id: String,
    },
}

fn persist_session_import_if_absent(
    connection: &DbConnection,
    workspace_id: &str,
    session: &CassSessionInfo,
    spans: &[CassViewSpanForImport],
) -> Result<SessionImportPersistResult, DbError> {
    let session_id = stable_session_id(&session.source_path);
    let index_job_id = stable_search_index_job_id(workspace_id, &session_id);

    with_import_session_transaction(connection, || {
        if let Some(existing) =
            connection.get_session_by_cass_id(workspace_id, &session.source_path)?
        {
            return Ok(SessionImportPersistResult::Skipped {
                session_id: existing.id,
            });
        }

        connection.insert_session(&session_id, &session_input(workspace_id, session))?;
        for span in spans {
            let evidence_id = stable_evidence_id(&session_id, &span.cass_span_id);
            connection.insert_evidence_span(
                &evidence_id,
                &evidence_input(workspace_id, &session_id, span),
            )?;
            if span.redacted {
                connection.insert_audit(
                    &stable_cass_redaction_audit_id(&evidence_id),
                    &cass_redaction_audit_input(workspace_id, &session_id, &evidence_id, span),
                )?;
            }
        }
        connection.insert_search_index_job(
            &index_job_id,
            &search_index_job_input(workspace_id, &session_id),
        )?;
        Ok(SessionImportPersistResult::Imported {
            session_id: session_id.clone(),
            index_job_id: index_job_id.clone(),
        })
    })
}

fn with_import_session_transaction<T>(
    connection: &DbConnection,
    mut operation: impl FnMut() -> Result<T, DbError>,
) -> Result<T, DbError> {
    const MAX_ATTEMPTS: usize = 16;
    let mut last_retryable_error = None;

    for attempt in 0..MAX_ATTEMPTS {
        begin_import_session_transaction(connection)?;
        match operation() {
            Ok(result) => match connection.commit() {
                Ok(()) => return Ok(result),
                Err(error) if import_session_transaction_error_is_retryable(&error) => {
                    let _ = connection.rollback();
                    last_retryable_error = Some(error);
                }
                Err(error) => {
                    let _ = connection.rollback();
                    return Err(error);
                }
            },
            Err(error) if import_session_transaction_error_is_retryable(&error) => {
                let _ = connection.rollback();
                last_retryable_error = Some(error);
            }
            Err(error) => {
                let _ = connection.rollback();
                return Err(error);
            }
        }

        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(import_session_transaction_retry_delay(attempt));
        }
    }

    match last_retryable_error {
        Some(error) => Err(error),
        None => Err(DbError::MalformedRow {
            operation: DbOperation::CommitTransaction,
            message: "import session transaction retry loop exhausted without a retryable error"
                .to_string(),
        }),
    }
}

fn begin_import_session_transaction(connection: &DbConnection) -> Result<(), DbError> {
    const MAX_ATTEMPTS: usize = 16;
    let mut last_retryable_error = None;

    for attempt in 0..MAX_ATTEMPTS {
        match connection.begin_transaction(IsolationLevel::RepeatableRead) {
            Ok(()) => return Ok(()),
            Err(error) if import_session_transaction_error_is_retryable(&error) => {
                last_retryable_error = Some(error);
                if attempt + 1 < MAX_ATTEMPTS {
                    std::thread::sleep(import_session_transaction_retry_delay(attempt));
                }
            }
            Err(error) => return Err(error),
        }
    }

    match last_retryable_error {
        Some(error) => Err(error),
        None => connection.begin_transaction(IsolationLevel::RepeatableRead),
    }
}

fn import_session_transaction_error_is_retryable(error: &DbError) -> bool {
    let DbError::SqlModel { source, .. } = error else {
        return false;
    };

    match source.as_ref() {
        sqlmodel_core::Error::Connection(connection) => {
            matches!(
                connection.kind,
                sqlmodel_core::error::ConnectionErrorKind::Connect
            ) && sqlite_contention_message_is_retryable(&connection.message)
        }
        sqlmodel_core::Error::Query(query) => match query.kind {
            sqlmodel_core::error::QueryErrorKind::Deadlock
            | sqlmodel_core::error::QueryErrorKind::Serialization => true,
            sqlmodel_core::error::QueryErrorKind::Database => {
                sqlite_contention_message_is_retryable(&query.message)
            }
            sqlmodel_core::error::QueryErrorKind::Syntax
            | sqlmodel_core::error::QueryErrorKind::Constraint
            | sqlmodel_core::error::QueryErrorKind::NotFound
            | sqlmodel_core::error::QueryErrorKind::Permission
            | sqlmodel_core::error::QueryErrorKind::DataTruncation
            | sqlmodel_core::error::QueryErrorKind::Timeout
            | sqlmodel_core::error::QueryErrorKind::Cancelled => false,
        },
        sqlmodel_core::Error::Type(_)
        | sqlmodel_core::Error::Transaction(_)
        | sqlmodel_core::Error::Protocol(_)
        | sqlmodel_core::Error::Pool(_)
        | sqlmodel_core::Error::Schema(_)
        | sqlmodel_core::Error::Config(_)
        | sqlmodel_core::Error::Validation(_)
        | sqlmodel_core::Error::Io(_)
        | sqlmodel_core::Error::Timeout
        | sqlmodel_core::Error::Cancelled
        | sqlmodel_core::Error::Serde(_)
        | sqlmodel_core::Error::Custom(_) => false,
    }
}

fn sqlite_contention_message_is_retryable(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("database is busy") || message.contains("snapshot conflict")
}

fn import_session_transaction_retry_delay(attempt: usize) -> Duration {
    const BASE_DELAY_MS: u64 = 1;
    const MAX_DELAY_MS: u64 = 50;

    let multiplier = 1_u64 << attempt.min(6);
    Duration::from_millis(BASE_DELAY_MS.saturating_mul(multiplier).min(MAX_DELAY_MS))
}

fn discover_sessions(
    client: &CassClient,
    workspace_path: &Path,
    limit: u32,
) -> Result<Vec<CassSessionInfo>, CassImportError> {
    let invocation = client.import_sessions_invocation(workspace_path, limit)?;
    let outcome = client.run(&invocation)?;
    ensure_successful_outcome(&outcome, "cass sessions")?;
    parse_sessions_json(outcome.stdout_bytes())
}

/// Parse an `ee import cass --since <duration>` value into a UTC cutoff.
///
/// Supported units are seconds, minutes, hours, days, and weeks. Adjacent
/// units are allowed, so `7d3h` and `7d 3h` are equivalent.
///
/// # Errors
///
/// Returns [`CassImportError::InvalidSince`] when the value is empty, uses an
/// unsupported unit, overflows the supported duration range, or would produce a
/// cutoff outside Chrono's representable range.
pub fn parse_import_since_duration(
    value: &str,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, CassImportError> {
    let duration = parse_since_duration(value)?;
    now.checked_sub_signed(duration)
        .ok_or_else(|| CassImportError::InvalidSince {
            value: value.to_string(),
            message: "duration is too large".to_string(),
        })
}

fn filter_sessions_since(
    sessions: Vec<CassSessionInfo>,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<CassSessionInfo>, CassImportError> {
    let Some(cutoff) = since else {
        return Ok(sessions);
    };

    let mut filtered = Vec::with_capacity(sessions.len());
    for session in sessions {
        let Some(session_time) = session_time_for_since_filter(&session)? else {
            continue;
        };
        if session_time >= cutoff {
            filtered.push(session);
        }
    }
    Ok(filtered)
}

fn session_time_for_since_filter(
    session: &CassSessionInfo,
) -> Result<Option<DateTime<Utc>>, CassImportError> {
    let Some(raw_timestamp) = session
        .started_at
        .as_deref()
        .or(session.ended_at.as_deref())
    else {
        return Ok(None);
    };
    let timestamp = DateTime::parse_from_rfc3339(raw_timestamp).map_err(|error| {
        CassImportError::InvalidJson {
            source: "sessions",
            message: format!("invalid session timestamp `{raw_timestamp}`: {error}"),
        }
    })?;
    Ok(Some(timestamp.with_timezone(&Utc)))
}

fn view_session_spans(
    client: &CassClient,
    source_path: &str,
) -> Result<Vec<CassViewSpanForImport>, CassImportError> {
    let invocation = client.import_view_invocation(source_path, 1, DEFAULT_VIEW_CONTEXT)?;
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
    let Some(sessions) = value.get("sessions").and_then(JsonValue::as_array) else {
        if let Some(hits) = value.get("hits").and_then(JsonValue::as_array) {
            return parse_legacy_search_hits_as_sessions(hits);
        }
        return Err(CassImportError::InvalidJson {
            source: "sessions",
            message: "missing sessions array".to_string(),
        });
    };

    let mut parsed = Vec::with_capacity(sessions.len());
    for item in sessions {
        let path = required_string(item, "path", "sessions")?;
        validate_reported_session_path(&path)?;
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
        session.message_count = optional_u32(item, "message_count", "sessions")?;
        session.token_count = optional_u32(item, "token_count", "sessions")?;
        if session.message_count.is_none() {
            push_missing_metadata(&mut session.missing_metadata, "message_count");
        }
        let content_hash = content_hash_for_session(item, &path);
        session.content_hash = Some(content_hash.value);
        session.content_hash_source = Some(content_hash.source);
        for field in content_hash.missing_metadata {
            push_missing_metadata(&mut session.missing_metadata, field);
        }
        parsed.push(session);
    }
    Ok(parsed)
}

fn parse_legacy_search_hits_as_sessions(
    hits: &[JsonValue],
) -> Result<Vec<CassSessionInfo>, CassImportError> {
    let mut sessions =
        std::collections::BTreeMap::<String, (CassSessionInfo, Option<i64>, Option<i64>)>::new();
    for hit in hits {
        let path = required_string(hit, "source_path", "sessions")?;
        validate_reported_session_path(&path)?;
        let created_at = hit.get("created_at").and_then(JsonValue::as_i64);
        let entry = sessions.entry(path.clone()).or_insert_with(|| {
            let mut session = CassSessionInfo::new(path.clone());
            let content_hash = content_hash_for_session(hit, &path);
            session.content_hash = Some(content_hash.value);
            session.content_hash_source = Some(content_hash.source);
            for field in content_hash.missing_metadata {
                push_missing_metadata(&mut session.missing_metadata, field);
            }
            (session, None, None)
        });
        if let Some(agent) = hit.get("agent").and_then(JsonValue::as_str) {
            entry.0.agent = agent.parse().unwrap_or(CassAgent::Unknown);
        }
        if entry.0.workspace_dir.is_none() {
            entry.0.workspace_dir = hit
                .get("workspace")
                .and_then(JsonValue::as_str)
                .map(str::to_string);
        }
        entry.0.message_count = Some(entry.0.message_count.unwrap_or(0).saturating_add(1));
        if let Some(timestamp) = created_at {
            entry.1 = Some(
                entry
                    .1
                    .map_or(timestamp, |existing| existing.min(timestamp)),
            );
            entry.2 = Some(
                entry
                    .2
                    .map_or(timestamp, |existing| existing.max(timestamp)),
            );
        }
    }

    let mut parsed = Vec::with_capacity(sessions.len());
    for (_, (mut session, started_at, ended_at)) in sessions {
        session.started_at = started_at.and_then(millis_to_rfc3339);
        session.ended_at = ended_at.and_then(millis_to_rfc3339);
        parsed.push(session);
    }
    Ok(parsed)
}

fn optional_u32(
    item: &JsonValue,
    field: &'static str,
    source: &'static str,
) -> Result<Option<u32>, CassImportError> {
    let Some(value) = item.get(field) else {
        return Ok(None);
    };
    let Some(value) = value.as_u64().and_then(|value| u32::try_from(value).ok()) else {
        return Err(CassImportError::InvalidJson {
            source,
            message: format!("{field} must be a non-negative integer within u32 range"),
        });
    };
    Ok(Some(value))
}

fn millis_to_rfc3339(value: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(value)
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn validate_reported_session_path(path: &str) -> Result<(), CassImportError> {
    if path.trim() != path {
        return Err(CassImportError::InvalidJson {
            source: "sessions",
            message: "session path has leading or trailing whitespace".to_string(),
        });
    }
    if path.starts_with('-') {
        return Err(CassImportError::InvalidJson {
            source: "sessions",
            message: "session path must not begin with '-'".to_string(),
        });
    }
    if path.contains('\0') {
        return Err(CassImportError::InvalidJson {
            source: "sessions",
            message: "session path must not contain NUL bytes".to_string(),
        });
    }
    Ok(())
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
    redacted: bool,
    redacted_reasons: Vec<String>,
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
        let raw_excerpt = truncate_excerpt(&content, 65_536);
        let redaction = crate::policy::redact_secret_like_content(&raw_excerpt);
        let redacted = redaction.redacted;
        let redacted_reasons = redaction
            .redacted_reasons
            .iter()
            .map(|reason| (*reason).to_string())
            .collect();
        let excerpt = redaction.content;
        spans.push(CassViewSpanForImport {
            cass_span_id: format!("{source_path}:{line_number}"),
            span_kind,
            start_line: line_number,
            end_line: line_number,
            role,
            content_hash: blake3_hex(&excerpt),
            excerpt,
            redacted,
            redacted_reasons,
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
    since: Option<String>,
    sessions: Vec<CassSessionInfo>,
) -> CassImportReport {
    CassImportReport {
        schema: IMPORT_CASS_SCHEMA_V1,
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        database_path: None,
        source_id,
        ledger_id: None,
        dry_run: true,
        since,
        sessions_discovered: saturating_len(sessions.len()),
        sessions_imported: 0,
        sessions_skipped: 0,
        spans_imported: 0,
        index_jobs_queued: 0,
        index_required_action: None,
        status: "dry_run".to_string(),
        sessions: sessions
            .into_iter()
            .map(|session| ImportedCassSession {
                source_path: session.source_path,
                session_id: None,
                index_job_id: None,
                status: ImportSessionStatus::WouldImport,
                spans_imported: 0,
                message_count: session.message_count,
                missing_metadata: session.missing_metadata,
            })
            .collect(),
    }
}

fn search_index_job_input(workspace_id: &str, session_id: &str) -> CreateSearchIndexJobInput {
    CreateSearchIndexJobInput {
        workspace_id: workspace_id.to_string(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("session".to_string()),
        document_id: Some(session_id.to_string()),
        documents_total: 1,
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
    let id = stable_import_id(source_id);
    let ledger = connection.upsert_running_import_ledger(
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
            metadata_json: Some(json!({"schema": IMPORT_LEDGER_CASS_SCHEMA_V1}).to_string()),
        },
    )?;
    Ok(ledger.id)
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
    let _ = connection.complete_import_ledger_attempt(
        ledger_id,
        &CompleteImportLedgerInput {
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
            imported_session_delta: imported_sessions,
            imported_span_delta: imported_spans,
            error_code: error.map(|err| error_code(err).to_string()),
            error_message: error.map(ToString::to_string),
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
        CassImportError::InvalidSince { .. } => "invalid_since",
        CassImportError::Io { .. } => "io",
        CassImportError::Storage(_) => "storage",
    }
}

fn session_input(workspace_id: &str, session: &CassSessionInfo) -> CreateSessionInput {
    let mut missing_metadata = session.missing_metadata.clone();
    if session.message_count.is_none() {
        push_missing_metadata(&mut missing_metadata, "message_count");
    }
    let (content_hash, content_hash_source) = match session.content_hash.as_deref() {
        Some(hash) if !hash.trim().is_empty() => (
            hash.to_owned(),
            session.content_hash_source.as_deref().unwrap_or("provided"),
        ),
        Some(_) | None => {
            push_missing_metadata(&mut missing_metadata, "content_hash");
            (
                blake3_hex(&format!(
                    "cass-session-missing-content-hash-v1\n{}",
                    session.source_path
                )),
                "derived_from_path_missing_content_hash",
            )
        }
    };
    CreateSessionInput {
        workspace_id: workspace_id.to_string(),
        cass_session_id: session.source_path.clone(),
        source_path: Some(session.source_path.clone()),
        agent_name: Some(session.agent.as_str().to_string()),
        model: None,
        started_at: session.started_at.clone(),
        ended_at: session.ended_at.clone(),
        message_count: session.message_count.unwrap_or(0),
        token_count: session.token_count,
        content_hash,
        metadata_json: Some(
            json!({
                "schema": CASS_SESSION_SCHEMA_V1,
                "workspaceDir": session.workspace_dir,
                "missingCassMetadata": missing_metadata,
                "messageCountObserved": session.message_count.is_some(),
                "contentHashSource": content_hash_source,
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
        metadata_json: Some(
            json!({
                "schema": CASS_EVIDENCE_SPAN_SCHEMA_V1,
                "redactionStatus": if span.redacted { "redacted" } else { "clean" },
                "redactionClasses": span.redacted_reasons,
            })
            .to_string(),
        ),
    }
}

fn cass_redaction_audit_input(
    workspace_id: &str,
    session_id: &str,
    evidence_id: &str,
    span: &CassViewSpanForImport,
) -> CreateAuditInput {
    CreateAuditInput {
        workspace_id: Some(workspace_id.to_string()),
        actor: Some("ee import cass".to_string()),
        action: CASS_REDACTION_AUDIT_ACTION.to_string(),
        target_type: Some("evidence_span".to_string()),
        target_id: Some(evidence_id.to_string()),
        details: Some(
            json!({
                "schema": CASS_REDACTION_AUDIT_SCHEMA_V1,
                "sessionId": session_id,
                "cassSpanId": span.cass_span_id,
                "redactionClasses": span.redacted_reasons,
            })
            .to_string(),
        ),
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionContentHash {
    value: String,
    source: String,
    missing_metadata: Vec<&'static str>,
}

fn content_hash_for_session(item: &JsonValue, path: &str) -> SessionContentHash {
    if let Some(content_hash) = item
        .get("content_hash")
        .and_then(JsonValue::as_str)
        .filter(|hash| !hash.trim().is_empty())
    {
        return SessionContentHash {
            value: content_hash.to_owned(),
            source: "provided".to_owned(),
            missing_metadata: Vec::new(),
        };
    }

    let modified = item.get("modified").and_then(JsonValue::as_str);
    let size = item.get("size_bytes").and_then(JsonValue::as_u64);
    let mut missing_metadata = vec!["content_hash"];
    if modified.is_none() {
        missing_metadata.push("modified");
    }
    if size.is_none() {
        missing_metadata.push("size_bytes");
    }

    let source = if missing_metadata.len() == 1 {
        "derived_from_path_modified_size"
    } else {
        "derived_from_path_with_missing_metadata"
    };
    SessionContentHash {
        value: blake3_hex(&format!(
            "cass-session-fallback-v1\npath={path}\nmodified={}\nsize_bytes={}",
            modified.unwrap_or("<missing>"),
            size.map_or_else(|| "<missing>".to_owned(), |value| value.to_string())
        )),
        source: source.to_owned(),
        missing_metadata,
    }
}

fn push_missing_metadata(fields: &mut Vec<String>, field: &'static str) {
    if !fields.iter().any(|existing| existing == field) {
        fields.push(field.to_owned());
    }
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

fn source_id(workspace_path: &Path, limit: u32, since: Option<&str>) -> String {
    let mut id = format!(
        "cass://sessions?workspace={}&limit={limit}",
        workspace_path.to_string_lossy()
    );
    if let Some(cutoff) = since {
        id.push_str("&since=");
        id.push_str(cutoff);
    }
    id
}

fn format_since_cutoff(since: DateTime<Utc>) -> String {
    since.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_since_duration(value: &str) -> Result<chrono::Duration, CassImportError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid_since(value, "duration must not be empty"));
    }

    let bytes = trimmed.as_bytes();
    let mut index = 0_usize;
    let mut total_seconds = 0_u64;
    while index < bytes.len() {
        while byte_at(bytes, index).is_some_and(|byte| byte.is_ascii_whitespace()) {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }

        let number_start = index;
        while byte_at(bytes, index).is_some_and(|byte| byte.is_ascii_digit()) {
            index += 1;
        }
        if number_start == index {
            return Err(invalid_since(value, "expected a positive number"));
        }
        let amount_text = trimmed
            .get(number_start..index)
            .ok_or_else(|| invalid_since(value, "invalid duration number"))?;
        let amount: u64 = amount_text
            .parse()
            .map_err(|_| invalid_since(value, "duration number is too large"))?;

        while byte_at(bytes, index).is_some_and(|byte| byte.is_ascii_whitespace()) {
            index += 1;
        }
        let unit_start = index;
        while byte_at(bytes, index).is_some_and(|byte| byte.is_ascii_alphabetic()) {
            index += 1;
        }
        if unit_start == index {
            return Err(invalid_since(value, "missing duration unit"));
        }

        let unit = trimmed
            .get(unit_start..index)
            .ok_or_else(|| invalid_since(value, "invalid duration unit"))?
            .to_ascii_lowercase();
        let multiplier = since_unit_seconds(&unit)
            .ok_or_else(|| invalid_since(value, "unsupported duration unit"))?;
        let seconds = amount
            .checked_mul(multiplier)
            .ok_or_else(|| invalid_since(value, "duration is too large"))?;
        total_seconds = total_seconds
            .checked_add(seconds)
            .ok_or_else(|| invalid_since(value, "duration is too large"))?;
    }

    if total_seconds == 0 {
        return Err(invalid_since(value, "duration must be greater than zero"));
    }

    let total_seconds =
        i64::try_from(total_seconds).map_err(|_| invalid_since(value, "duration is too large"))?;
    chrono::Duration::try_seconds(total_seconds)
        .ok_or_else(|| invalid_since(value, "duration is too large"))
}

fn byte_at(bytes: &[u8], index: usize) -> Option<u8> {
    bytes.get(index).copied()
}

fn since_unit_seconds(unit: &str) -> Option<u64> {
    match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(60 * 60),
        "d" | "day" | "days" => Some(24 * 60 * 60),
        "w" | "week" | "weeks" => Some(7 * 24 * 60 * 60),
        _ => None,
    }
}

fn invalid_since(value: &str, message: &str) -> CassImportError {
    CassImportError::InvalidSince {
        value: value.to_string(),
        message: message.to_string(),
    }
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

fn stable_cass_redaction_audit_id(evidence_id: &str) -> String {
    AuditId::from_uuid(stable_uuid(&format!("audit:cass-redaction:{evidence_id}"))).to_string()
}

fn stable_search_index_job_id(workspace_id: &str, session_id: &str) -> String {
    let hash = blake3_hex(&format!("search-index-job:{workspace_id}:{session_id}"));
    format!("sidx_{}", &hash[..26])
}

fn stable_import_id(source_id: &str) -> String {
    let hash = blake3_hex(&format!("import:{source_id}"));
    format!("imp_{}", &hash[..26])
}

fn index_required_action(workspace_path: &Path, database_path: Option<&Path>) -> String {
    let workspace = workspace_path.to_string_lossy();
    let Some(database_path) = database_path else {
        return format!("ee index rebuild --workspace {workspace}");
    };
    format!(
        "ee index rebuild --workspace {workspace} --database {}",
        database_path.to_string_lossy()
    )
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
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::{Arc, Barrier};
    #[cfg(unix)]
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    #[cfg(unix)]
    type TestResultWith<T> = Result<T, String>;

    #[cfg(unix)]
    fn unique_test_dir(prefix: &str) -> TestResultWith<PathBuf> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        Ok(target_dir
            .join("ee-cass-import-tests")
            .join(format!("{prefix}-{}-{now}", std::process::id())))
    }

    #[cfg(unix)]
    fn write_fake_cass_binary(
        path: &Path,
        workspace_path: &Path,
        session_path: &Path,
    ) -> TestResult {
        let sessions = json!({
            "sessions": [{
                "path": session_path.to_string_lossy(),
                "workspace": workspace_path.to_string_lossy(),
                "agent": "codex",
                "modified": "2026-05-05T00:00:00Z",
                "message_count": 1,
                "token_count": 8
            }]
        });
        let view = json!({
            "path": session_path.to_string_lossy(),
            "lines": [{
                "line": 1,
                "content": "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"index me\"}}"
            }]
        });
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  sessions) printf '%s\\n' '{}';;\n  view) printf '%s\\n' '{}';;\n  *) printf 'unexpected cass command: %s\\n' \"$1\" >&2; exit 2;;\nesac\n",
            sessions, view
        );
        fs::write(path, script).map_err(|error| error.to_string())?;
        let mut permissions = fs::metadata(path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).map_err(|error| error.to_string())
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
              "size_bytes": 4096,
              "message_count": 12,
              "token_count": 345,
              "content_hash": "cass-content-hash"
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
        ensure_equal(
            &first.content_hash.as_deref(),
            &Some("cass-content-hash"),
            "content hash",
        )?;
        ensure_equal(
            &first.content_hash_source.as_deref(),
            &Some("provided"),
            "content hash source",
        )?;
        ensure_equal(
            &first.missing_metadata,
            &Vec::<String>::new(),
            "missing metadata",
        )
    }

    #[test]
    fn missing_cass_session_metadata_is_observable() -> TestResult {
        let input = br#"{
          "sessions": [
            {
              "path": "/tmp/session.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex"
            }
          ]
        }"#;

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&sessions.len(), &1, "session count")?;
        let first = sessions
            .first()
            .ok_or_else(|| "missing parsed session".to_string())?;
        let expected_missing = vec![
            "message_count".to_string(),
            "content_hash".to_string(),
            "modified".to_string(),
            "size_bytes".to_string(),
        ];
        ensure_equal(&first.message_count, &None::<u32>, "missing message count")?;
        ensure_equal(
            &first.missing_metadata,
            &expected_missing,
            "missing metadata",
        )?;
        ensure_equal(
            &first.content_hash_source.as_deref(),
            &Some("derived_from_path_with_missing_metadata"),
            "content hash source",
        )?;

        let input = session_input("wsp_abc", first);
        ensure_equal(&input.message_count, &0, "stored message count fallback")?;
        let metadata = input
            .metadata_json
            .as_ref()
            .ok_or_else(|| "session metadata json should be present".to_string())
            .and_then(|metadata| {
                serde_json::from_str::<JsonValue>(metadata).map_err(|error| error.to_string())
            })?;
        ensure_equal(
            &metadata["missingCassMetadata"],
            &json!(expected_missing),
            "stored missing metadata",
        )?;
        ensure_equal(
            &metadata["messageCountObserved"],
            &json!(false),
            "message count observed",
        )?;
        ensure_equal(
            &metadata["contentHashSource"],
            &json!("derived_from_path_with_missing_metadata"),
            "stored content hash source",
        )?;

        let report = dry_run_report(
            PathBuf::from("/tmp/project"),
            "cass://x".to_string(),
            None,
            sessions,
        );
        let json = report.data_json();
        ensure_equal(
            &json["sessions"][0]["missingMetadata"],
            &json!(expected_missing),
            "reported missing metadata",
        )
    }

    #[test]
    fn parse_sessions_rejects_malformed_required_metadata() -> TestResult {
        let input = br#"{
          "sessions": [
            {
              "path": 123,
              "workspace": "/tmp/project",
              "agent": "codex"
            }
          ]
        }"#;

        let error = match parse_sessions_json(input) {
            Ok(_) => return Err("malformed path should fail".to_string()),
            Err(error) => error.to_string(),
        };
        ensure(
            error.contains("missing non-empty path"),
            format!("error should mention path requirement, got {error}"),
        )
    }

    #[test]
    fn parse_sessions_rejects_malformed_numeric_metadata() -> TestResult {
        let input = br#"{
          "sessions": [
            {
              "path": "/tmp/session.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "message_count": "twelve"
            }
          ]
        }"#;

        let error = match parse_sessions_json(input) {
            Ok(_) => return Err("malformed message_count should fail".to_string()),
            Err(error) => error.to_string(),
        };
        ensure(
            error.contains("message_count must be a non-negative integer within u32 range"),
            format!("error should mention malformed message_count, got {error}"),
        )
    }

    #[test]
    fn parses_legacy_cass_search_hits_as_session_discovery() -> TestResult {
        let input = br#"{
          "count": 2,
          "hits": [
            {
              "source_path": "/tmp/session.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "created_at": 1778133601000
            },
            {
              "source_path": "/tmp/session.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "created_at": 1778133603000
            }
          ]
        }"#;

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&sessions.len(), &1, "session count")?;
        let first = sessions
            .first()
            .ok_or_else(|| "missing parsed session".to_string())?;
        ensure_equal(&first.source_path.as_str(), &"/tmp/session.jsonl", "path")?;
        ensure_equal(
            &first.workspace_dir.as_deref(),
            &Some("/tmp/project"),
            "workspace",
        )?;
        ensure_equal(&first.message_count, &Some(2), "message_count")?;
        ensure_equal(
            &first.started_at.as_deref(),
            &Some("2026-05-07T06:00:01Z"),
            "started_at",
        )?;
        ensure_equal(
            &first.ended_at.as_deref(),
            &Some("2026-05-07T06:00:03Z"),
            "ended_at",
        )
    }

    #[test]
    fn parse_import_since_duration_accepts_compound_windows() -> TestResult {
        let now = DateTime::parse_from_rfc3339("2026-05-05T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);

        let cutoff = parse_import_since_duration("7d3h", now).map_err(|error| error.to_string())?;

        ensure_equal(
            &format_since_cutoff(cutoff),
            &"2026-04-28T09:00:00Z".to_string(),
            "compound cutoff",
        )?;
        let cutoff =
            parse_import_since_duration("90 days", now).map_err(|error| error.to_string())?;
        ensure_equal(
            &format_since_cutoff(cutoff),
            &"2026-02-04T12:00:00Z".to_string(),
            "spaced cutoff",
        )
    }

    #[test]
    fn parse_import_since_duration_rejects_invalid_windows() -> TestResult {
        for value in ["", "0d", "90", "forever", "1month", "-7d"] {
            let now = DateTime::parse_from_rfc3339("2026-05-05T12:00:00Z")
                .map_err(|error| error.to_string())?
                .with_timezone(&Utc);
            let error = match parse_import_since_duration(value, now) {
                Ok(_) => return Err(format!("since value {value:?} should fail")),
                Err(error) => error.to_string(),
            };
            ensure(
                error.contains("invalid --since value"),
                format!("error for {value:?} should mention --since, got {error}"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn since_filter_keeps_sessions_at_or_after_cutoff() -> TestResult {
        let cutoff = DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        let mut recent = CassSessionInfo::new("/tmp/recent.jsonl");
        recent.started_at = Some("2026-04-30T00:00:00Z".to_string());
        let mut old = CassSessionInfo::new("/tmp/old.jsonl");
        old.started_at = Some("2026-03-01T00:00:00Z".to_string());
        let mut modified_fallback = CassSessionInfo::new("/tmp/modified.jsonl");
        modified_fallback.ended_at = Some("2026-04-02T00:00:00Z".to_string());
        let missing_time = CassSessionInfo::new("/tmp/missing.jsonl");

        let filtered = filter_sessions_since(
            vec![recent, old, modified_fallback, missing_time],
            Some(cutoff),
        )
        .map_err(|error| error.to_string())?;
        let paths: Vec<&str> = filtered
            .iter()
            .map(|session| session.source_path.as_str())
            .collect();

        ensure_equal(
            &paths,
            &vec!["/tmp/recent.jsonl", "/tmp/modified.jsonl"],
            "filtered sessions",
        )
    }

    #[test]
    fn parse_sessions_rejects_malicious_prefix_paths() -> TestResult {
        for path in ["--config=/tmp/evil", "-n", "  --hidden"] {
            let input = format!(
                r#"{{
                  "sessions": [
                    {{
                      "path": {path:?},
                      "workspace": "/tmp/project",
                      "agent": "codex"
                    }}
                  ]
                }}"#
            );

            let error = match parse_sessions_json(input.as_bytes()) {
                Ok(_) => return Err(format!("malicious session path {path:?} should fail")),
                Err(error) => error.to_string(),
            };
            ensure(
                error.contains("session path"),
                format!("error for {path:?} should mention session path, got {error}"),
            )?;
        }
        Ok(())
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
        let report = dry_run_report(
            PathBuf::from("/tmp/work"),
            "cass://x".to_string(),
            Some("2026-04-01T00:00:00Z".to_string()),
            sessions,
        );

        ensure_equal(&report.dry_run, &true, "dry run")?;
        ensure_equal(&report.database_path, &None, "no database path")?;
        ensure_equal(
            &report.since.as_deref(),
            &Some("2026-04-01T00:00:00Z"),
            "since cutoff",
        )?;
        ensure_equal(&report.sessions_discovered, &1, "discovered")?;
        ensure_equal(&report.index_jobs_queued, &0, "dry-run index jobs")?;
        ensure_equal(&report.index_required_action, &None, "dry-run index action")?;
        ensure_equal(
            &report.sessions[0].status,
            &ImportSessionStatus::WouldImport,
            "would import",
        )
    }

    #[test]
    fn report_json_identifies_import_command_and_session_status() -> TestResult {
        let report = CassImportReport {
            schema: IMPORT_CASS_SCHEMA_V1,
            workspace_path: "/tmp/work".to_string(),
            database_path: Some("/tmp/work/.ee/ee.db".to_string()),
            source_id: "cass://x".to_string(),
            ledger_id: Some("imp_abc".to_string()),
            dry_run: false,
            since: Some("2026-04-01T00:00:00Z".to_string()),
            sessions_discovered: 1,
            sessions_imported: 1,
            sessions_skipped: 0,
            spans_imported: 2,
            index_jobs_queued: 1,
            index_required_action: Some(
                "ee index rebuild --workspace /tmp/work --database /tmp/work/.ee/ee.db".to_string(),
            ),
            status: "completed".to_string(),
            sessions: vec![ImportedCassSession {
                source_path: "/tmp/a.jsonl".to_string(),
                session_id: Some("sess_abc".to_string()),
                index_job_id: Some("sidx_abc".to_string()),
                status: ImportSessionStatus::Imported,
                spans_imported: 2,
                message_count: Some(3),
                missing_metadata: Vec::new(),
            }],
        };

        let json = report.data_json();
        ensure_equal(&json["command"], &json!("import cass"), "command")?;
        ensure_equal(&json["schema"], &json!("ee.import.cass.v1"), "schema")?;
        ensure_equal(
            &json["since"],
            &json!("2026-04-01T00:00:00Z"),
            "since cutoff",
        )?;
        ensure_equal(&json["indexJobsQueued"], &json!(1), "index jobs")?;
        ensure_equal(
            &json["indexRequiredAction"],
            &json!("ee index rebuild --workspace /tmp/work --database /tmp/work/.ee/ee.db"),
            "index action",
        )?;
        ensure_equal(&json["sessions"][0]["status"], &json!("imported"), "status")?;
        ensure_equal(
            &json["sessions"][0]["indexJobId"],
            &json!("sidx_abc"),
            "session index job",
        )
    }

    #[test]
    fn search_index_job_input_targets_imported_session_document() -> TestResult {
        let input = search_index_job_input("wsp_abc", "sess_abc");

        ensure_equal(&input.workspace_id.as_str(), &"wsp_abc", "workspace")?;
        ensure_equal(
            &input.job_type,
            &SearchIndexJobType::SingleDocument,
            "job type",
        )?;
        ensure_equal(
            &input.document_source.as_deref(),
            &Some("session"),
            "document source",
        )?;
        ensure_equal(
            &input.document_id.as_deref(),
            &Some("sess_abc"),
            "document id",
        )?;
        ensure_equal(&input.documents_total, &1, "document count")
    }

    #[cfg(unix)]
    #[test]
    fn import_persists_pending_session_index_job() -> TestResult {
        let root = unique_test_dir("queue-index-job")?;
        let bin_dir = root.join("bin");
        let workspace_path = root.join("workspace");
        let session_path = root.join("session.jsonl");
        fs::create_dir_all(&bin_dir).map_err(|error| error.to_string())?;
        fs::create_dir_all(&workspace_path).map_err(|error| error.to_string())?;
        fs::write(&session_path, "{}\n").map_err(|error| error.to_string())?;
        let mut bin_permissions = fs::metadata(&bin_dir)
            .map_err(|error| error.to_string())?
            .permissions();
        bin_permissions.set_mode(0o755);
        fs::set_permissions(&bin_dir, bin_permissions).map_err(|error| error.to_string())?;

        let cass_binary = bin_dir.join("cass");
        write_fake_cass_binary(&cass_binary, &workspace_path, &session_path)?;
        let database_path = root.join("ee.db");
        let client = CassClient::with_binary(cass_binary).with_timeout(Duration::from_secs(5));
        let options = CassImportOptions {
            workspace_path: workspace_path.clone(),
            database_path: Some(database_path.clone()),
            limit: 1,
            since: None,
            dry_run: false,
            include_spans: true,
        };

        let report = import_cass_sessions(&client, &options).map_err(|error| error.to_string())?;

        ensure_equal(&report.sessions_imported, &1, "sessions imported")?;
        ensure_equal(&report.spans_imported, &1, "spans imported")?;
        ensure_equal(&report.index_jobs_queued, &1, "index jobs queued")?;
        let imported_session = report
            .sessions
            .first()
            .ok_or_else(|| "import report should include imported session".to_string())?;
        let session_id = imported_session
            .session_id
            .as_deref()
            .ok_or_else(|| "imported session should include id".to_string())?;
        let index_job_id = imported_session
            .index_job_id
            .as_deref()
            .ok_or_else(|| "imported session should include index job id".to_string())?;

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let workspace_id = stable_workspace_id(
            &workspace_path
                .canonicalize()
                .map_err(|error| error.to_string())?
                .to_string_lossy(),
        );
        let jobs = connection
            .list_search_index_jobs(&workspace_id, None)
            .map_err(|error| error.to_string())?;
        ensure_equal(&jobs.len(), &1, "stored index jobs")?;
        let job = jobs
            .first()
            .ok_or_else(|| "stored index job should exist".to_string())?;
        ensure_equal(&job.id.as_str(), &index_job_id, "stored index job id")?;
        ensure_equal(&job.status.as_str(), &"pending", "job status")?;
        ensure_equal(
            &job.document_source.as_deref(),
            &Some("session"),
            "job source",
        )?;
        ensure_equal(
            &job.document_id.as_deref(),
            &Some(session_id),
            "job document",
        )
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_session_import_persistence_is_idempotent() -> TestResult {
        let root = unique_test_dir("concurrent-session-import")?;
        let workspace_path = root.join("workspace");
        let database_path = root.join("ee.db");
        let session_path = root.join("session.jsonl");
        fs::create_dir_all(&workspace_path).map_err(|error| error.to_string())?;
        fs::write(&session_path, "{}\n").map_err(|error| error.to_string())?;

        let setup = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        setup.migrate().map_err(|error| error.to_string())?;
        let workspace_id =
            ensure_workspace(&setup, &workspace_path).map_err(|error| error.to_string())?;
        setup.close().map_err(|error| error.to_string())?;

        let barrier = Arc::new(Barrier::new(2));
        let session_source_path = session_path.to_string_lossy().into_owned();
        let mut handles = Vec::new();
        for _ in 0..2 {
            let barrier = Arc::clone(&barrier);
            let database_path = database_path.clone();
            let workspace_id = workspace_id.clone();
            let session_source_path = session_source_path.clone();
            handles.push(std::thread::spawn(
                move || -> Result<SessionImportPersistResult, String> {
                    let connection = DbConnection::open_file(&database_path)
                        .map_err(|error| error.to_string())?;
                    let mut session = CassSessionInfo::new(session_source_path);
                    session.message_count = Some(1);

                    barrier.wait();
                    let result =
                        persist_session_import_if_absent(&connection, &workspace_id, &session, &[])
                            .map_err(|error| error.to_string());
                    let close_result = connection.close().map_err(|error| error.to_string());

                    match (result, close_result) {
                        (Ok(result), Ok(())) => Ok(result),
                        (Err(error), _) | (_, Err(error)) => Err(error),
                    }
                },
            ));
        }

        let mut imported = 0_u32;
        let mut skipped = 0_u32;
        for handle in handles {
            match handle
                .join()
                .map_err(|_| "session import thread panicked".to_string())??
            {
                SessionImportPersistResult::Imported { .. } => {
                    imported = imported.saturating_add(1);
                }
                SessionImportPersistResult::Skipped { .. } => {
                    skipped = skipped.saturating_add(1);
                }
            }
        }

        ensure_equal(&imported, &1, "exactly one import wins")?;
        ensure_equal(&skipped, &1, "exactly one import observes existing session")?;

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let stored = connection
            .get_session_by_cass_id(&workspace_id, &session_source_path)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stored session should exist".to_string())?;
        let jobs = connection
            .list_search_index_jobs(&workspace_id, None)
            .map_err(|error| error.to_string())?;
        ensure_equal(
            &stored.cass_session_id.as_str(),
            &session_source_path.as_str(),
            "stored cass session id",
        )?;
        ensure_equal(&jobs.len(), &1, "one index job is queued")?;
        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_path_hijack_default_binary_before_spawn() -> TestResult {
        let root = unique_test_dir("path-hijack")?;
        let fake_dir = root.join("evil");
        let workspace_path = root.join("workspace");
        let marker = root.join("fake-cass-ran");
        fs::create_dir_all(&fake_dir).map_err(|error| error.to_string())?;
        fs::create_dir_all(&workspace_path).map_err(|error| error.to_string())?;
        let fake_cass = fake_dir.join("cass");
        fs::write(
            &fake_cass,
            format!(
                "#!/bin/sh\nprintf ran > '{}'\nprintf '{{\"sessions\":[]}}\\n'\n",
                marker.display()
            ),
        )
        .map_err(|error| error.to_string())?;
        let mut permissions = fs::metadata(&fake_cass)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_cass, permissions).map_err(|error| error.to_string())?;

        let mut path_entries = vec![fake_dir];
        if let Some(existing_path) = std::env::var_os("PATH") {
            path_entries.extend(std::env::split_paths(&existing_path));
        }
        let hijacked_path =
            std::env::join_paths(path_entries).map_err(|error| error.to_string())?;
        let client = CassClient::new_default()
            .with_extra_env("PATH", hijacked_path)
            .with_timeout(Duration::from_secs(5));
        let options = CassImportOptions {
            workspace_path,
            database_path: None,
            limit: 1,
            since: None,
            dry_run: true,
            include_spans: false,
        };

        let error = match import_cass_sessions(&client, &options) {
            Ok(_) => return Err("PATH hijack import should fail before spawning cass".to_string()),
            Err(error) => error,
        };
        let cass_error = match error {
            CassImportError::Cass(error) => error,
            other => {
                return Err(format!(
                    "PATH hijack should fail as CassError, got {other:?}",
                ));
            }
        };

        ensure_equal(&cass_error.kind_str(), &"invalid_binary", "error kind")?;
        ensure(
            cass_error.to_string().contains("PATH lookup"),
            format!("error should mention PATH lookup, got {cass_error}"),
        )?;
        ensure(!marker.exists(), "fake cass from PATH must not execute")
    }

    #[test]
    fn stable_ids_match_storage_constraints() -> TestResult {
        let workspace_id = stable_workspace_id("/tmp/work");
        let session_id = stable_session_id("/tmp/session.jsonl");
        let evidence_id = stable_evidence_id(&session_id, "span-1");
        let audit_id = stable_cass_redaction_audit_id(&evidence_id);
        let import_id = stable_import_id("cass://sessions?workspace=/tmp/work&limit=10");
        let since_source_id = source_id(Path::new("/tmp/work"), 10, Some("2026-04-01T00:00:00Z"));
        let index_job_id = stable_search_index_job_id(&workspace_id, &session_id);

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
            audit_id.starts_with("audit_") && audit_id.len() == 32,
            "audit id shape",
        )?;
        ensure(
            import_id.starts_with("imp_") && import_id.len() == 30,
            "import id shape",
        )?;
        ensure(
            since_source_id.ends_with("&since=2026-04-01T00:00:00Z"),
            "source id includes since cutoff",
        )?;
        ensure(
            index_job_id.starts_with("sidx_") && index_job_id.len() == 31,
            "search index job id shape",
        )
    }
}
