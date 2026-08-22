//! CASS import execution (EE-107).
//!
//! This module is the first executable import slice: discover sessions
//! through CASS' robot JSON surface, optionally persist imported session
//! rows, capture first-line evidence spans, and update the resumable
//! import ledger.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use blake3;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value as JsonValue, json};
use sqlmodel_core::IsolationLevel;
use uuid::Uuid;

use super::process::{CASS_STDOUT_LINE_MAX_BYTES, CassStreamError};
use super::{
    CassAgent, CassClient, CassError, CassExitClass, CassRole, CassSessionInfo, CassSpanKind,
    ImportCursor,
};
use crate::db::{
    CompleteImportLedgerInput, CreateAuditInput, CreateEvidenceSpanInput, CreateImportLedgerInput,
    CreateSearchIndexJobInput, CreateSessionInput, DatabaseConfig, DbConnection, DbError,
    DbOperation, EvidenceProducerKind, SearchIndexJobType,
};
use crate::models::{
    AuditId, CASS_EVIDENCE_SPAN_SCHEMA_V1, CASS_SESSION_SCHEMA_V1, EvidenceId,
    IMPORT_CASS_SCHEMA_V1, IMPORT_LEDGER_CASS_SCHEMA_V1, SessionId,
};

const DEFAULT_DB_FILE: &str = "ee.db";
const DEFAULT_VIEW_CONTEXT: u32 = 4;
const IMPORT_SOURCE_KIND: &str = "cass";
const CASS_REDACTION_AUDIT_SCHEMA_V1: &str = "ee.cass.redaction_audit.v1";
const CASS_REDACTION_AUDIT_ACTION: &str = "cass.evidence.redacted";
const CASS_SUBPROCESS_DIAGNOSTICS_SCHEMA_V1: &str = "ee.cass.subprocess_diagnostics.v1";
/// Upper bound on the total `cass view --json` stdout we will buffer before
/// parsing. Real `cass` emits a single pretty-printed `{...,"lines":[...]}`
/// envelope (see [`parse_view_json_envelope`]), so the per-line read limit
/// (`CASS_STDOUT_LINE_MAX_BYTES`, enforced in `process.rs`) is not by itself a
/// total bound — the whole object can span many lines. Mirror the buffered
/// `run` path's 100 MiB pipe cap so a pathological session can't make us hold
/// unbounded memory.
const CASS_VIEW_STDOUT_TOTAL_MAX_BYTES: usize = 100 * 1024 * 1024;
#[cfg(test)]
const CASS_VIEW_STREAM_MEMORY_BUDGET_BYTES: usize = 10 * 1024 * 1024;

#[cfg(test)]
static CASS_VIEW_STREAM_PEAK_SAMPLE_BYTES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

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

/// Response-affecting degradation emitted when durable CASS import succeeds
/// but its derived search-index publication does not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

impl CassImportDegradation {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
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
    /// Mark every queued import job as published and remove the obsolete
    /// manual-rebuild action.
    pub fn record_index_publish_success(&mut self) {
        self.index_required_action = None;
    }

    /// Preserve the committed import while truthfully reporting that its
    /// derived search index still requires the exact advertised rebuild.
    pub fn record_index_publish_failure(
        &mut self,
        detail: impl Into<String>,
    ) -> CassImportDegradation {
        let repair = self.index_required_action.clone().unwrap_or_else(|| {
            index_required_action(
                Path::new(&self.workspace_path),
                self.database_path.as_deref().map(Path::new),
            )
        });
        self.index_required_action = Some(repair.clone());
        CassImportDegradation {
            code: "cass_import_index_publish_failed",
            severity: "medium",
            message: format!(
                "Search index is stale because CASS import committed successfully, but search-index publication failed: {}; imported rows may be omitted until the index is rebuilt.",
                detail.into()
            ),
            repair,
        }
    }

    /// Render the stable JSON data payload for the response envelope.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let source_id = redact_import_report_source_ref(&self.source_id);
        json!({
            "schema": self.schema,
            "command": "import cass",
            "workspacePath": self.workspace_path,
            "databasePath": self.database_path,
            "sourceId": source_id,
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
                let source_path = redact_import_report_source_ref(&session.source_path);
                json!({
                    "sourcePath": source_path,
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

    /// Render the human summary with an actual post-import publication
    /// degradation.
    #[must_use]
    pub fn human_summary_with_degradation(
        &self,
        degradation: Option<&CassImportDegradation>,
    ) -> String {
        let mut output = self.human_summary();
        if let Some(degradation) = degradation {
            output.push_str(&format!(
                "  Degraded:\n    - {}: {} Repair: {}\n",
                degradation.code, degradation.message, degradation.repair
            ));
        }
        output
    }
}

fn redact_import_report_source_ref(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_import_report_path_like_segments(&secret_redacted)
}

fn redact_import_report_path_like_segments(value: &str) -> String {
    const REDACTED_PATH: &str = "[REDACTED_PATH]";

    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some(start) = value[cursor..]
            .char_indices()
            .map(|(relative, _)| cursor + relative)
            .find(|start| import_report_path_starts_at(value, *start))
        else {
            output.push_str(&value[cursor..]);
            break;
        };

        output.push_str(&value[cursor..start]);
        output.push_str(REDACTED_PATH);
        cursor = value[start..]
            .char_indices()
            .skip(1)
            .find_map(|(index, ch)| import_report_source_path_boundary(ch).then_some(start + index))
            .unwrap_or(value.len());
    }
    output
}

fn import_report_path_starts_at(value: &str, start: usize) -> bool {
    let candidate = &value[start..];
    if candidate
        .get(.."file://".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
    {
        return true;
    }

    let token_boundary_before = value[..start].chars().next_back().is_none_or(|previous| {
        previous.is_whitespace() || matches!(previous, '"' | '\'' | '`' | '(' | '[' | '{' | '=')
    });
    if candidate.starts_with('/') && token_boundary_before {
        return true;
    }

    let bytes = candidate.as_bytes();
    if bytes.len() >= 3
        && token_boundary_before
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        return true;
    }

    let unc_prefix = candidate.starts_with(r"\\") || candidate.starts_with("//");
    unc_prefix && token_boundary_before
}

fn import_report_source_path_boundary(ch: char) -> bool {
    matches!(
        ch,
        '\n' | '\r' | '?' | '#' | '"' | '\'' | '`' | ')' | ']' | '}' | ',' | ';' | '&'
    )
}

fn bounded_public_cass_text(value: &str, max_chars: usize) -> String {
    const TRUNCATED: &str = "[TRUNCATED]";
    let redacted = redact_import_report_source_ref(value);
    if redacted.chars().count() <= max_chars {
        return redacted;
    }
    let mut bounded = redacted.chars().take(max_chars).collect::<String>();
    bounded.push_str(TRUNCATED);
    bounded
}

/// Error produced by CASS import.
#[derive(Debug)]
pub enum CassImportError {
    Cass(CassError),
    CassCommand {
        command: String,
        exit_code: Option<i32>,
        stderr: String,
        timed_out: bool,
        stderr_truncated: bool,
        stdout_line_count: Option<usize>,
        peak_stdout_line_bytes: Option<usize>,
        peak_stdout_buffer_bytes: Option<usize>,
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

    /// Render redaction-safe subprocess supervision diagnostics for failures
    /// where the adapter has structured cleanup evidence.
    #[must_use]
    pub fn subprocess_diagnostics_json(&self) -> Option<JsonValue> {
        match self {
            Self::Cass(CassError::Io { message }) => cass_io_subprocess_diagnostics_json(message),
            Self::CassCommand {
                command,
                exit_code,
                stderr,
                timed_out,
                stderr_truncated,
                stdout_line_count,
                peak_stdout_line_bytes,
                peak_stdout_buffer_bytes,
            } => Some(cass_command_subprocess_diagnostics_json(
                command,
                *exit_code,
                stderr,
                *timed_out,
                *stderr_truncated,
                *stdout_line_count,
                *peak_stdout_line_bytes,
                *peak_stdout_buffer_bytes,
            )),
            _ => None,
        }
    }
}

impl fmt::Display for CassImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cass(error) => {
                formatter.write_str(&bounded_public_cass_text(&error.to_string(), 2_048))
            }
            Self::CassCommand {
                command,
                exit_code,
                stderr,
                ..
            } => {
                let command = bounded_public_cass_text(command, 512);
                let stderr = bounded_public_cass_text(stderr, 2_048);
                write!(
                    formatter,
                    "cass command `{command}` failed with exit {exit_code:?}: {stderr}",
                )
            }
            Self::InvalidJson { source, message } => {
                let message = bounded_public_cass_text(message, 2_048);
                write!(formatter, "invalid CASS {source} JSON: {message}")
            }
            Self::InvalidSince { value, message } => {
                let value = bounded_public_cass_text(value, 512);
                let message = bounded_public_cass_text(message, 2_048);
                write!(formatter, "invalid --since value `{value}`: {message}")
            }
            Self::Io { path, message } => {
                let path = bounded_public_cass_text(path.to_string_lossy().as_ref(), 1_024);
                let message = bounded_public_cass_text(message, 2_048);
                write!(formatter, "I/O error at {path}: {message}")
            }
            Self::Storage(error) => {
                formatter.write_str(&bounded_public_cass_text(&error.to_string(), 2_048))
            }
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

fn cass_io_subprocess_diagnostics_json(message: &str) -> Option<JsonValue> {
    let lower = message.to_lowercase();
    if !lower.contains("cass subprocess") {
        return None;
    }

    if lower.contains("stdout line exceeded") {
        return Some(json!({
            "schema": CASS_SUBPROCESS_DIAGNOSTICS_SCHEMA_V1,
            "outcome": "cap_error",
            "command": null,
            "cap": {
                "stream": "stdout",
                "kind": "line",
                "limitBytes": first_usize_in_text(message),
            },
            "timeout": false,
            "killAttempted": true,
            "reapSucceeded": true,
            "stderrCapture": {
                "truncated": false,
                "captureCapExceeded": false,
            },
            "peakStreamedStdoutLineBufferBytes": null,
            "unavailable": {
                "command": "cap error surfaced before the higher-level command wrapper was built",
                "peakStreamedStdoutLineBufferBytes": "reader returned the cap error before a stream outcome was emitted",
            },
        }));
    }

    if lower.contains("stdout line was not valid utf-8") {
        return Some(json!({
            "schema": CASS_SUBPROCESS_DIAGNOSTICS_SCHEMA_V1,
            "outcome": "invalid_utf8",
            "command": null,
            "cap": null,
            "timeout": false,
            "killAttempted": true,
            "reapSucceeded": true,
            "stderrCapture": {
                "truncated": false,
                "captureCapExceeded": false,
            },
            "peakStreamedStdoutLineBufferBytes": null,
            "unavailable": {
                "command": "UTF-8 error surfaced before the higher-level command wrapper was built",
                "peakStreamedStdoutLineBufferBytes": "reader returned the UTF-8 error before a stream outcome was emitted",
            },
        }));
    }

    if lower.contains("exceeded") && lower.contains("byte capture limit") {
        let stream = if lower.contains(" stderr ") {
            "stderr"
        } else if lower.contains(" stdout ") {
            "stdout"
        } else {
            "unknown"
        };
        return Some(json!({
            "schema": CASS_SUBPROCESS_DIAGNOSTICS_SCHEMA_V1,
            "outcome": "cap_error",
            "command": null,
            "cap": {
                "stream": stream,
                "kind": "capture",
                "limitBytes": first_usize_in_text(message),
            },
            "timeout": false,
            "killAttempted": true,
            "reapSucceeded": true,
            "stderrCapture": {
                "truncated": false,
                "captureCapExceeded": stream == "stderr",
            },
            "peakStreamedStdoutLineBufferBytes": null,
            "unavailable": {
                "command": "capture-cap error surfaced before the higher-level command wrapper was built",
                "peakStreamedStdoutLineBufferBytes": "not available for whole-pipe capture mode",
            },
        }));
    }

    None
}

fn cass_command_subprocess_diagnostics_json(
    command: &str,
    exit_code: Option<i32>,
    stderr: &str,
    timed_out: bool,
    stderr_truncated: bool,
    stdout_line_count: Option<usize>,
    peak_stdout_line_bytes: Option<usize>,
    peak_stdout_buffer_bytes: Option<usize>,
) -> JsonValue {
    let command = bounded_public_cass_text(command, 512);
    json!({
        "schema": CASS_SUBPROCESS_DIAGNOSTICS_SCHEMA_V1,
        "outcome": if timed_out { "timeout" } else { "cass_command_failure" },
        "command": command,
        "exitCode": exit_code,
        "cap": null,
        "timeout": timed_out,
        "killAttempted": timed_out,
        "reapSucceeded": if timed_out { Some(true) } else { None },
        "reapUnavailableReason": if timed_out {
            None
        } else {
            Some("subprocess exited before adapter cleanup was required")
        },
        "stderrCapture": {
            "truncated": stderr_truncated,
            "captureCapExceeded": false,
            "capturedBytes": stderr.len(),
        },
        "stdoutLineCount": stdout_line_count,
        "peakStreamedStdoutLineBytes": peak_stdout_line_bytes,
        "peakStreamedStdoutLineBufferBytes": peak_stdout_buffer_bytes,
    })
}

fn first_usize_in_text(text: &str) -> Option<usize> {
    text.split(|ch: char| !ch.is_ascii_digit())
        .find(|token| !token.is_empty())
        .and_then(|token| token.parse().ok())
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
                let index_job_id = existing_session_index_job_for_reconciliation(
                    &connection,
                    &workspace_id,
                    &existing.id,
                )?;
                if index_job_id.is_some() {
                    index_jobs_queued = index_jobs_queued.saturating_add(1);
                }
                cursor.record_skipped();
                skipped = skipped.saturating_add(1);
                session_reports.push(ImportedCassSession {
                    source_path: session.source_path,
                    session_id: Some(existing.id),
                    index_job_id,
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
                    let index_job_id = existing_session_index_job_for_reconciliation(
                        &connection,
                        &workspace_id,
                        &session_id,
                    )?;
                    if index_job_id.is_some() {
                        index_jobs_queued = index_jobs_queued.saturating_add(1);
                    }
                    cursor.record_skipped();
                    skipped = skipped.saturating_add(1);
                    session_reports.push(ImportedCassSession {
                        source_path: session.source_path,
                        session_id: Some(session_id),
                        index_job_id,
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
    let session_id = stable_session_id(workspace_id, &session.source_path);
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
                    if let Err(rollback_error) = connection.rollback() {
                        tracing::error!(
                            phase = "cass_import_commit_retryable",
                            error = %error,
                            rollback_error = %rollback_error,
                            "failed to rollback import session transaction after commit failure"
                        );
                    }
                    last_retryable_error = Some(error);
                }
                Err(error) => {
                    if let Err(rollback_error) = connection.rollback() {
                        tracing::error!(
                            phase = "cass_import_commit",
                            error = %error,
                            rollback_error = %rollback_error,
                            "failed to rollback import session transaction after commit failure"
                        );
                    }
                    return Err(error);
                }
            },
            Err(error) if import_session_transaction_error_is_retryable(&error) => {
                if let Err(rollback_error) = connection.rollback() {
                    tracing::error!(
                        phase = "cass_import_operation_retryable",
                        error = %error,
                        rollback_error = %rollback_error,
                        "failed to rollback import session transaction after operation failure"
                    );
                }
                last_retryable_error = Some(error);
            }
            Err(error) => {
                if let Err(rollback_error) = connection.rollback() {
                    tracing::error!(
                        phase = "cass_import_operation",
                        error = %error,
                        rollback_error = %rollback_error,
                        "failed to rollback import session transaction after operation failure"
                    );
                }
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
    // `cass view --json` emits ONE pretty-printed `{...,"lines":[...]}` envelope
    // (the first stdout line is just `{`), not a per-line `{"line",..}` JSONL
    // stream. So we cannot `serde_json::from_str` each line in isolation (the
    // bare `{` yields "EOF while parsing an object" and imports zero spans —
    // issue #11). Instead, stream the stdout to keep the per-line 1 MiB read
    // limit and bound total memory, accumulate the raw bytes, then hand the
    // whole buffer to `parse_view_json`, which understands the real envelope
    // shape via `parse_view_json_envelope` (and still tolerates true JSONL).
    let mut buffer = CassViewStdoutBuffer::default();
    let outcome = match invocation.run_stdout_lines(|line| buffer.accept_line(&line)) {
        Ok(outcome) => outcome,
        Err(CassStreamError::Cass(error)) => return Err(CassImportError::Cass(error)),
        Err(CassStreamError::Handler(error)) => return Err(error),
    };
    tracing::debug!(
        command = "cass view",
        stdout_lines = outcome.stdout_line_count(),
        stdout_bytes_seen = outcome.stdout_bytes_seen(),
        peak_stdout_line_bytes = outcome.peak_stdout_line_bytes(),
        peak_stdout_buffer_bytes = outcome.peak_stdout_buffer_bytes(),
        "streamed CASS view stdout"
    );
    ensure_successful_stream_outcome(&outcome, "cass view")?;
    parse_view_json(buffer.as_bytes(), source_path)
}

/// Accumulates `cass view --json` stdout while streaming so the full envelope
/// can be parsed in one pass (see [`view_session_spans`]). Preserves the
/// streaming safety posture: each line is already bounded by the read-side
/// `CASS_STDOUT_LINE_MAX_BYTES` limit, and the running total is capped here at
/// [`CASS_VIEW_STDOUT_TOTAL_MAX_BYTES`] so pathological output cannot pin
/// unbounded memory.
#[derive(Default)]
struct CassViewStdoutBuffer {
    bytes: Vec<u8>,
}

impl CassViewStdoutBuffer {
    fn accept_line(&mut self, line: &str) -> Result<(), CassImportError> {
        // `+ 1` accounts for the newline we re-insert between lines so the
        // reconstructed buffer matches the original multi-line stdout.
        let projected = self
            .bytes
            .len()
            .saturating_add(line.len())
            .saturating_add(1);
        if projected > CASS_VIEW_STDOUT_TOTAL_MAX_BYTES {
            return Err(CassImportError::InvalidJson {
                source: "view",
                message: format!(
                    "view JSON output exceeds {CASS_VIEW_STDOUT_TOTAL_MAX_BYTES} byte limit"
                ),
            });
        }
        if !self.bytes.is_empty() {
            self.bytes.push(b'\n');
        }
        self.bytes.extend_from_slice(line.as_bytes());
        record_cass_view_stream_peak_sample(self.bytes.len());
        Ok(())
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
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
        timed_out: outcome.timed_out(),
        stderr_truncated: false,
        stdout_line_count: None,
        peak_stdout_line_bytes: None,
        peak_stdout_buffer_bytes: None,
    })
}

fn ensure_successful_stream_outcome(
    outcome: &super::process::CassStreamOutcome,
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
        timed_out: outcome.timed_out(),
        stderr_truncated: false,
        stdout_line_count: Some(outcome.stdout_line_count()),
        peak_stdout_line_bytes: Some(outcome.peak_stdout_line_bytes()),
        peak_stdout_buffer_bytes: Some(outcome.peak_stdout_buffer_bytes()),
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
        session.started_at = optional_rfc3339_timestamp(
            item,
            &["started_at", "started"],
            "sessions",
            "session start timestamp",
        )?;
        session.ended_at = optional_rfc3339_timestamp(
            item,
            &["ended_at", "modified"],
            "sessions",
            "session end timestamp",
        )?;
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
    let mut sessions = std::collections::BTreeMap::<
        String,
        (CassSessionInfo, Option<String>, Option<String>),
    >::new();
    for hit in hits {
        let path = required_string(hit, "source_path", "sessions")?;
        validate_reported_session_path(&path)?;
        let created_at = legacy_hit_created_at_rfc3339(hit)?;
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
        if let Some(timestamp) = created_at.as_deref() {
            entry.1 = Some(match entry.1.as_deref() {
                Some(existing) if existing <= timestamp => existing.to_owned(),
                _ => timestamp.to_owned(),
            });
            entry.2 = Some(match entry.2.as_deref() {
                Some(existing) if existing >= timestamp => existing.to_owned(),
                _ => timestamp.to_owned(),
            });
        }
    }

    let mut parsed = Vec::with_capacity(sessions.len());
    for (_, (mut session, started_at, ended_at)) in sessions {
        session.started_at = started_at;
        session.ended_at = ended_at;
        parsed.push(session);
    }
    Ok(parsed)
}

fn legacy_hit_created_at_rfc3339(hit: &JsonValue) -> Result<Option<String>, CassImportError> {
    let Some(value) = hit.get("created_at") else {
        return Ok(None);
    };
    if let Some(timestamp) = value.as_i64() {
        return Ok(millis_to_rfc3339(timestamp));
    }
    if let Some(timestamp) = value
        .as_str()
        .filter(|timestamp| !timestamp.is_empty() && timestamp.trim().len() == timestamp.len())
    {
        let parsed = DateTime::parse_from_rfc3339(timestamp).map_err(|error| {
            CassImportError::InvalidJson {
                source: "sessions",
                message: format!("invalid created_at RFC3339 timestamp `{timestamp}`: {error}"),
            }
        })?;
        return Ok(Some(
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        ));
    }
    Err(CassImportError::InvalidJson {
        source: "sessions",
        message: "created_at must be an integer epoch milliseconds or non-empty RFC3339 timestamp"
            .to_string(),
    })
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

fn optional_rfc3339_timestamp(
    item: &JsonValue,
    fields: &[&'static str],
    source: &'static str,
    label: &'static str,
) -> Result<Option<String>, CassImportError> {
    // Walk the candidate fields in priority order and use the first one that
    // carries a usable timestamp. A present-but-null or empty/whitespace-only
    // value is treated identically to an absent field (`Ok(None)`), so we keep
    // falling through to the next candidate instead of committing to a hole.
    // This is what lets `["ended_at", "modified"]` fall back to `modified`
    // when `ended_at` is null, and vice-versa.
    for field in fields {
        if let Some(timestamp) = optional_rfc3339_timestamp_field(item, field, source, label)? {
            return Ok(Some(timestamp));
        }
    }
    Ok(None)
}

fn optional_rfc3339_timestamp_field(
    item: &JsonValue,
    field: &'static str,
    source: &'static str,
    label: &'static str,
) -> Result<Option<String>, CassImportError> {
    let Some(value) = item.get(field) else {
        return Ok(None);
    };
    // cass emits `"modified": null` for ongoing / never-finalized sessions (the
    // live session never has a finalized end timestamp). serde surfaces JSON
    // `null` as `Some(Value::Null)`, so it slips past a bare presence check and
    // would otherwise reach `as_str() == None` and hard-error — aborting the
    // whole batch import (issue #13). A present-null timestamp carries no
    // information, so treat it exactly like an absent field: the session still
    // imports, just with this timestamp left as `None`.
    if value.is_null() {
        return Ok(None);
    }
    let Some(raw) = value.as_str() else {
        // A non-null, non-string value (number, bool, object, ...) is a genuine
        // type error in the emitter's output — fail loudly rather than guess.
        return Err(CassImportError::InvalidJson {
            source,
            message: format!("{field} must be a string RFC3339 {label}"),
        });
    };
    // Empty or whitespace-only strings likewise carry no timestamp; treat them
    // as absent so a blank `modified` can never zero an otherwise-valid import.
    if raw.trim().is_empty() {
        return Ok(None);
    }
    // A value with real content but surrounding whitespace is malformed: we
    // store timestamps verbatim (they participate in ordering/dedup), so reject
    // padded values rather than silently trimming and changing the stored key.
    if raw.trim().len() != raw.len() {
        return Err(CassImportError::InvalidJson {
            source,
            message: format!("{field} must be a non-empty RFC3339 {label}"),
        });
    }
    DateTime::parse_from_rfc3339(raw).map_err(|error| CassImportError::InvalidJson {
        source,
        message: format!("invalid {field} RFC3339 {label} `{raw}`: {error}"),
    })?;
    Ok(Some(raw.to_owned()))
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
    let text = std::str::from_utf8(input).map_err(|error| CassImportError::InvalidJson {
        source: "view",
        message: error.to_string(),
    })?;

    match parse_view_json_lines(text, source_path) {
        Ok(spans) => Ok(spans),
        Err(error) if cass_view_line_error_is_size_limit(&error) => Err(error),
        Err(line_error) => {
            if let Some(spans) = parse_view_json_envelope(input, source_path)? {
                Ok(spans)
            } else {
                Err(line_error)
            }
        }
    }
}

fn cass_view_line_error_is_size_limit(error: &CassImportError) -> bool {
    matches!(
        error,
        CassImportError::InvalidJson { source: "view", message }
            if message.starts_with("view JSON line exceeds ")
    )
}

fn parse_view_json_envelope(
    input: &[u8],
    source_path: &str,
) -> Result<Option<Vec<CassViewSpanForImport>>, CassImportError> {
    if let Ok(value) = serde_json::from_slice::<JsonValue>(input) {
        if let Some(lines) = value.get("lines").and_then(JsonValue::as_array) {
            let mut collector = CassViewLineCollector::new(source_path);
            for line in lines {
                collector.accept_value(line)?;
            }
            return Ok(Some(collector.into_spans()));
        }
    }

    Ok(None)
}

fn parse_view_json_lines(
    input: &str,
    source_path: &str,
) -> Result<Vec<CassViewSpanForImport>, CassImportError> {
    let mut collector = CassViewLineCollector::new(source_path);
    for line in input.lines() {
        collector.accept_line(line)?;
    }
    Ok(collector.into_spans())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CassViewStreamMemorySample {
    retained_span_bytes: usize,
    peak_sampled_bytes: usize,
}

impl CassViewStreamMemorySample {
    fn record_line(&mut self, line_bytes: usize) {
        self.peak_sampled_bytes = self
            .peak_sampled_bytes
            .max(self.retained_span_bytes.saturating_add(line_bytes));
        record_cass_view_stream_peak_sample(self.peak_sampled_bytes);
    }

    fn record_span(&mut self, span: &CassViewSpanForImport) {
        self.retained_span_bytes = self
            .retained_span_bytes
            .saturating_add(estimated_span_retained_bytes(span));
        self.peak_sampled_bytes = self.peak_sampled_bytes.max(self.retained_span_bytes);
        record_cass_view_stream_peak_sample(self.peak_sampled_bytes);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CassViewLineCollector {
    source_path: String,
    spans: Vec<CassViewSpanForImport>,
    seen_lines: BTreeSet<u32>,
    memory_sample: CassViewStreamMemorySample,
}

impl CassViewLineCollector {
    fn new(source_path: &str) -> Self {
        Self {
            source_path: source_path.to_string(),
            spans: Vec::new(),
            seen_lines: BTreeSet::new(),
            memory_sample: CassViewStreamMemorySample::default(),
        }
    }

    fn accept_line(&mut self, line: &str) -> Result<(), CassImportError> {
        if line.trim().is_empty() {
            return Ok(());
        }
        if line.len() > CASS_STDOUT_LINE_MAX_BYTES {
            return Err(CassImportError::InvalidJson {
                source: "view",
                message: format!("view JSON line exceeds {CASS_STDOUT_LINE_MAX_BYTES} byte limit"),
            });
        }
        self.memory_sample.record_line(line.len());
        let value: JsonValue =
            serde_json::from_str(line).map_err(|error| CassImportError::InvalidJson {
                source: "view",
                message: error.to_string(),
            })?;
        self.accept_value(&value)
    }

    fn accept_value(&mut self, value: &JsonValue) -> Result<(), CassImportError> {
        let span = parse_view_line_value(value, &self.source_path)?;
        if !self.seen_lines.insert(span.start_line) {
            return Err(CassImportError::InvalidJson {
                source: "view",
                message: format!("duplicate line {}", span.start_line),
            });
        }
        self.memory_sample.record_span(&span);
        self.spans.push(span);
        Ok(())
    }

    fn into_spans(self) -> Vec<CassViewSpanForImport> {
        self.spans
    }
}

fn parse_view_line_value(
    line: &JsonValue,
    source_path: &str,
) -> Result<CassViewSpanForImport, CassImportError> {
    let line_number = line
        .get("line")
        .and_then(JsonValue::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| CassImportError::InvalidJson {
            source: "view",
            message: "line entry missing positive numeric line".to_string(),
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
    Ok(CassViewSpanForImport {
        cass_span_id: format!("{source_path}:{line_number}"),
        span_kind,
        start_line: line_number,
        end_line: line_number,
        role,
        // Evidence-span content hashes must be canonical `blake3:<64-hex>` so the
        // derivation-source-package validation (curate::is_canonical_blake3_content_hash)
        // accepts them on the persist path (`ee review session --propose`). `blake3_hex`
        // returns a BARE hex digest, so prefix it here. See issue #10.
        content_hash: format!("blake3:{}", blake3_hex(&raw_excerpt)),
        excerpt: raw_excerpt,
        redacted,
        redacted_reasons,
    })
}

fn estimated_span_retained_bytes(span: &CassViewSpanForImport) -> usize {
    span.cass_span_id
        .len()
        .saturating_add(span.excerpt.len())
        .saturating_add(span.content_hash.len())
        .saturating_add(span.redacted_reasons.iter().map(String::len).sum::<usize>())
        .saturating_add(std::mem::size_of::<CassViewSpanForImport>())
}

#[cfg(test)]
fn record_cass_view_stream_peak_sample(bytes: usize) {
    use std::sync::atomic::Ordering;

    let mut current = CASS_VIEW_STREAM_PEAK_SAMPLE_BYTES.load(Ordering::Relaxed);
    while bytes > current {
        match CASS_VIEW_STREAM_PEAK_SAMPLE_BYTES.compare_exchange_weak(
            current,
            bytes,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[cfg(not(test))]
fn record_cass_view_stream_peak_sample(_bytes: usize) {}

#[cfg(test)]
fn reset_cass_view_stream_peak_sample() {
    CASS_VIEW_STREAM_PEAK_SAMPLE_BYTES.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
fn cass_view_stream_peak_sample_bytes() -> usize {
    CASS_VIEW_STREAM_PEAK_SAMPLE_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

fn classify_line(content: &str) -> (CassSpanKind, Option<CassRole>) {
    let Ok(value) = serde_json::from_str::<JsonValue>(content) else {
        return (CassSpanKind::Message, None);
    };
    let line_type = value
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let span_kind = CassSpanKind::parse_lossy(line_type);
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

/// Recover durable publication work when an identical CASS import observes a
/// session that was committed by an earlier attempt. Completed jobs need no
/// work; pending/running/cancelled/failed jobs are returned to the caller so
/// the ordinary CLI reconciliation pass processes (or truthfully observes)
/// the same deterministic job. A legacy session with no job receives that
/// missing deterministic row under the import transaction's contention-safe
/// retry lane.
fn existing_session_index_job_for_reconciliation(
    connection: &DbConnection,
    workspace_id: &str,
    session_id: &str,
) -> Result<Option<String>, DbError> {
    let index_job_id = stable_search_index_job_id(workspace_id, session_id);
    with_import_session_transaction(connection, || {
        match connection.get_search_index_job(&index_job_id)? {
            Some(job) if job.status_enum() == Some(crate::db::SearchIndexJobStatus::Completed) => {
                Ok(None)
            }
            Some(_) => Ok(Some(index_job_id.clone())),
            None => {
                connection.insert_search_index_job(
                    &index_job_id,
                    &search_index_job_input(workspace_id, session_id),
                )?;
                Ok(Some(index_job_id.clone()))
            }
        }
    })
}

fn ensure_workspace(connection: &DbConnection, workspace_path: &Path) -> Result<String, DbError> {
    crate::core::workspace::ensure_bound_workspace(
        connection,
        &crate::core::workspace::stable_workspace_id(workspace_path),
        &[workspace_path],
    )
    .map_err(|error| DbError::MalformedRow {
        operation: DbOperation::Execute,
        message: error.message(),
    })
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
            cursor_json: Some(import_cursor_json(cursor, error).to_string()),
            imported_session_delta: imported_sessions,
            imported_span_delta: imported_spans,
            error_code: error.map(|err| error_code(err).to_string()),
            error_message: error.map(ToString::to_string),
            completed_at: Some(now),
        },
    )?;
    Ok(())
}

fn import_cursor_json(cursor: &ImportCursor, error: Option<&CassImportError>) -> JsonValue {
    let last_source_path = cursor
        .last_source_path
        .as_deref()
        .map(redact_import_report_source_ref);
    let mut cursor_json = json!({
        "lastSourcePath": last_source_path,
        "lastLine": cursor.last_line,
        "sessionsDiscovered": cursor.sessions_discovered,
        "sessionsImported": cursor.sessions_imported,
        "sessionsSkipped": cursor.sessions_skipped,
        "spansImported": cursor.spans_imported,
        "complete": cursor.is_complete(),
    });
    if let Some(diagnostics) = error.and_then(CassImportError::subprocess_diagnostics_json)
        && let Some(object) = cursor_json.as_object_mut()
    {
        object.insert("subprocessDiagnostics".to_string(), diagnostics);
    }
    cursor_json
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
            // Trim before storing so a whitespace-padded hash like "  abc123  "
            // dedupes against the same hash without the padding. The match
            // guard already trims for the empty-check, but storing the raw
            // value would let one upstream cass emitter that pads with
            // newlines silently fail dedup against another that doesn't.
            hash.trim().to_owned(),
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
        producer_kind: EvidenceProducerKind::CassImport,
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
        inherited_redaction_classes: Vec::new(),
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
                "evidenceSpanId": evidence_id,
                "provenanceUri": format!(
                    "cass-session://{session_id}#L{}-{}",
                    span.start_line,
                    span.end_line
                ),
                "upstreamRefHash": format!(
                    "blake3:{}",
                    blake3::hash(span.cass_span_id.as_bytes()).to_hex()
                ),
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
            // Trim before storing — symmetric with the trim in
            // session_input() above. A whitespace-padded hash from a noisy
            // cass emitter must dedupe against the same hash without the
            // padding; storing the raw value silently breaks dedup.
            value: content_hash.trim().to_owned(),
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
    ensure_database_path_has_no_symlink_components(path)?;
    ensure_database_path_is_regular_or_missing(path)?;
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| CassImportError::Io {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;
    ensure_database_path_has_no_symlink_components(path)?;
    ensure_database_path_is_regular_or_missing(path)
}

fn ensure_database_path_has_no_symlink_components(path: &Path) -> Result<(), CassImportError> {
    match database_path_has_symlink_component(path) {
        Ok(false) => Ok(()),
        Ok(true) => Err(CassImportError::Io {
            path: path.to_path_buf(),
            message: "refusing CASS import database path with symlink component".to_string(),
        }),
        Err(error) => Err(CassImportError::Io {
            path: path.to_path_buf(),
            message: format!("failed to inspect CASS import database path: {error}"),
        }),
    }
}

fn ensure_database_path_is_regular_or_missing(path: &Path) -> Result<(), CassImportError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(CassImportError::Io {
            path: path.to_path_buf(),
            message: "refusing CASS import database path because it is not a regular file"
                .to_string(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CassImportError::Io {
            path: path.to_path_buf(),
            message: format!("failed to inspect CASS import database path: {error}"),
        }),
    }
}

fn database_path_has_symlink_component(path: &Path) -> io::Result<bool> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
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

fn stable_session_id(workspace_id: &str, source_path: &str) -> String {
    SessionId::from_uuid(stable_uuid(&format!(
        "session:{workspace_id}:{source_path}"
    )))
    .to_string()
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
    let workspace_text = workspace_path.to_string_lossy();
    let workspace = shell_quote_command_arg(workspace_text.as_ref());
    let Some(database_path) = database_path else {
        return format!("ee index rebuild --workspace {workspace}");
    };
    let database_text = database_path.to_string_lossy();
    let database = shell_quote_command_arg(database_text.as_ref());
    format!("ee index rebuild --workspace {workspace} --database {database}")
}

fn shell_quote_command_arg(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value.bytes().all(|byte| {
        matches!(
            byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'_'
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'@'
                | b'+'
                | b'='
        )
    }) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
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

    #[test]
    fn subprocess_diagnostics_classifies_stdout_line_cap() -> TestResult {
        let error = CassImportError::Cass(CassError::Io {
            message: format!(
                "cass subprocess stdout line exceeded {CASS_STDOUT_LINE_MAX_BYTES} byte limit"
            ),
        });
        let diagnostics = error
            .subprocess_diagnostics_json()
            .ok_or_else(|| "stdout line cap should produce diagnostics".to_string())?;

        ensure_equal(
            &diagnostics["schema"],
            &json!("ee.cass.subprocess_diagnostics.v1"),
            "diagnostics schema",
        )?;
        ensure_equal(
            &diagnostics["outcome"],
            &json!("cap_error"),
            "diagnostics outcome",
        )?;
        ensure_equal(
            &diagnostics["cap"]["stream"],
            &json!("stdout"),
            "cap stream",
        )?;
        ensure_equal(&diagnostics["cap"]["kind"], &json!("line"), "cap kind")?;
        ensure_equal(&diagnostics["timeout"], &json!(false), "timeout flag")?;
        ensure_equal(
            &diagnostics["killAttempted"],
            &json!(true),
            "kill attempted",
        )?;
        ensure_equal(
            &diagnostics["reapSucceeded"],
            &json!(true),
            "reap succeeded",
        )
    }

    #[test]
    fn subprocess_diagnostics_records_timeout_stream_metrics() -> TestResult {
        let error = CassImportError::CassCommand {
            command: "cass view".to_string(),
            exit_code: None,
            stderr: String::new(),
            timed_out: true,
            stderr_truncated: false,
            stdout_line_count: Some(1),
            peak_stdout_line_bytes: Some(128),
            peak_stdout_buffer_bytes: Some(256),
        };
        let diagnostics = error
            .subprocess_diagnostics_json()
            .ok_or_else(|| "timeout command should produce diagnostics".to_string())?;

        ensure_equal(
            &diagnostics["outcome"],
            &json!("timeout"),
            "diagnostics outcome",
        )?;
        ensure_equal(&diagnostics["command"], &json!("cass view"), "command")?;
        ensure_equal(&diagnostics["timeout"], &json!(true), "timeout flag")?;
        ensure_equal(
            &diagnostics["killAttempted"],
            &json!(true),
            "kill attempted",
        )?;
        ensure_equal(
            &diagnostics["reapSucceeded"],
            &json!(true),
            "reap succeeded",
        )?;
        ensure_equal(
            &diagnostics["stdoutLineCount"],
            &json!(1),
            "stdout line count",
        )?;
        ensure_equal(
            &diagnostics["peakStreamedStdoutLineBufferBytes"],
            &json!(256),
            "peak buffer bytes",
        )
    }

    #[test]
    fn public_cass_source_ref_redaction_covers_unix_windows_unc_and_file_uris() -> TestResult {
        for (raw, forbidden) in [
            ("/Users/Alice/private/session.jsonl", "/Users/Alice"),
            ("/mnt/cass/private/session.jsonl", "/mnt/cass"),
            ("/opt/ee/private/session.jsonl", "/opt/ee"),
            ("/root/.cass/session.jsonl", "/root/.cass"),
            ("/srv/cass/session.jsonl", "/srv/cass"),
            ("/usr/local/share/cass/session.jsonl", "/usr/local"),
            (
                r"C:\Users\Alice\.codex\sessions\session.jsonl",
                r"C:\Users\Alice",
            ),
            (
                r"\\fileserver\profiles\Alice\session.jsonl",
                r"\\fileserver\profiles",
            ),
            (
                "file:///C:/Users/Alice/.codex/sessions/session.jsonl",
                "C:/Users/Alice",
            ),
            (
                "file://fileserver/profiles/Alice/session.jsonl",
                "fileserver/profiles/Alice",
            ),
        ] {
            let redacted = redact_import_report_source_ref(raw);
            ensure(
                redacted.contains("[REDACTED_PATH]"),
                format!("expected path placeholder for {raw:?}, got {redacted:?}"),
            )?;
            ensure(
                !redacted.contains(forbidden),
                format!("forbidden path {forbidden:?} escaped as {redacted:?}"),
            )?;
        }

        ensure_equal(
            &redact_import_report_source_ref("cass://safe-source"),
            &"cass://safe-source".to_owned(),
            "non-file URI remains intact",
        )?;
        ensure_equal(
            &redact_import_report_source_ref("https://example.test/session"),
            &"https://example.test/session".to_owned(),
            "multi-letter URI schemes are not mistaken for drive paths",
        )?;
        ensure_equal(
            &redact_import_report_source_ref(r"source=C:\Users\Alice\session.jsonl"),
            &"source=[REDACTED_PATH]".to_owned(),
            "embedded drive path at a token boundary is redacted",
        )
    }

    #[test]
    fn cass_command_public_error_diagnostics_and_cursor_are_path_secret_safe() -> TestResult {
        let secret = format!("sk_live_{}", "1234567890abcdef1234567890abcdef");
        let command_path = r"C:\Users\Alice\private\session.jsonl";
        let stderr_path = r"\\fileserver\profiles\Alice\private\trace.jsonl";
        let error = CassImportError::CassCommand {
            command: format!(r#"cass view "{command_path}" --api-key {secret}"#),
            exit_code: Some(2),
            stderr: format!(
                "failed to read {stderr_path}\nsecondary source file:///C:/Users/Alice/private/fallback.jsonl token={secret}"
            ),
            timed_out: false,
            stderr_truncated: false,
            stdout_line_count: Some(0),
            peak_stdout_line_bytes: Some(0),
            peak_stdout_buffer_bytes: Some(0),
        };

        let public_message = error.to_string();
        let diagnostics = error
            .subprocess_diagnostics_json()
            .ok_or_else(|| "command failure should expose safe diagnostics".to_owned())?;
        let diagnostics_text =
            serde_json::to_string(&diagnostics).map_err(|error| error.to_string())?;
        for rendered in [&public_message, &diagnostics_text] {
            ensure(
                !rendered.contains(command_path),
                format!("Windows drive path escaped public error surface: {rendered}"),
            )?;
            ensure(
                !rendered.contains(stderr_path),
                format!("UNC path escaped public error surface: {rendered}"),
            )?;
            ensure(
                !rendered.contains("C:/Users/Alice"),
                format!("file URI path escaped public error surface: {rendered}"),
            )?;
            ensure(
                !rendered.contains(&secret),
                format!("secret escaped public error surface: {rendered}"),
            )?;
        }
        ensure(
            public_message.contains("[REDACTED_PATH]"),
            "public error message should retain a path-redaction marker",
        )?;
        ensure(
            diagnostics_text.contains("[REDACTED_PATH]"),
            "diagnostics command should retain a path-redaction marker",
        )?;

        let mut cursor = ImportCursor::new();
        cursor.record_discovered();
        cursor.record_imported(stderr_path);
        let cursor_text = import_cursor_json(&cursor, Some(&error)).to_string();
        ensure(
            !cursor_text.contains(stderr_path) && !cursor_text.contains(&secret),
            format!("import ledger cursor leaked private source material: {cursor_text}"),
        )?;
        ensure(
            cursor_text.contains("[REDACTED_PATH]"),
            "import ledger cursor should retain a path-redaction marker",
        )
    }

    #[test]
    fn failed_import_cursor_includes_subprocess_diagnostics_only_when_available() -> TestResult {
        let cursor = ImportCursor::new();
        let clean_cursor = import_cursor_json(&cursor, None);
        ensure(
            clean_cursor.get("subprocessDiagnostics").is_none(),
            "successful cursor should not include subprocess diagnostics",
        )?;

        let error = CassImportError::Cass(CassError::Io {
            message: "cass subprocess stderr exceeded 104857600 byte capture limit".to_string(),
        });
        let failed_cursor = import_cursor_json(&cursor, Some(&error));
        ensure_equal(
            &failed_cursor["subprocessDiagnostics"]["cap"]["stream"],
            &json!("stderr"),
            "diagnostic cap stream",
        )?;
        ensure_equal(
            &failed_cursor["subprocessDiagnostics"]["stderrCapture"]["captureCapExceeded"],
            &json!(true),
            "stderr cap exceeded",
        )
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
        let target_dir = target_dir
            .canonicalize()
            .map_err(|error| format!("canonicalize CASS import test root: {error}"))?;
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
        write_fake_cass_binary_with_view_lines(path, workspace_path, session_path, 1)
    }

    #[cfg(unix)]
    fn write_fake_cass_binary_with_view_lines(
        path: &Path,
        workspace_path: &Path,
        session_path: &Path,
        view_line_count: u32,
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
        // Emit the SAME pretty-printed `{...,"lines":[...],"total_lines":N}`
        // envelope that real `cass view --json` produces (the first stdout line
        // is just `{`). Emitting per-line JSONL here — which `cass` never does —
        // previously masked issue #11, where the streaming import parsed each
        // stdout line in isolation and choked on the bare `{`.
        let mut lines = Vec::with_capacity(view_line_count as usize);
        for line_number in 1..=view_line_count {
            let content = json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": format!("index me {line_number}"),
                }
            });
            lines.push(json!({
                "line": line_number,
                "content": content.to_string(),
                "highlighted": line_number == 1,
            }));
        }
        let view = serde_json::to_string_pretty(&json!({
            "path": session_path.to_string_lossy(),
            "target_line": 1,
            "context": DEFAULT_VIEW_CONTEXT,
            "lines": lines,
            "total_lines": view_line_count,
        }))
        .map_err(|error| error.to_string())?;
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  sessions) cat <<'EE_CASS_SESSIONS'\n{}\nEE_CASS_SESSIONS\n;;\n  view) cat <<'EE_CASS_VIEW'\n{}\nEE_CASS_VIEW\n;;\n  *) printf 'unexpected cass command: %s\\n' \"$1\" >&2; exit 2;;\nesac\n",
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
    fn null_cass_session_modified_imports_with_none_end(/* issue #13 */) -> TestResult {
        // cass emits `"modified": null` for the live / never-finalized session.
        // A null end timestamp on one session must NOT abort the whole batch:
        // both sessions must parse, the null one with `ended_at = None`.
        let input = br#"{
          "sessions": [
            {
              "path": "/tmp/finished.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "modified": "2026-04-30T00:00:00Z"
            },
            {
              "path": "/tmp/live.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "modified": null
            }
          ]
        }"#;

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&sessions.len(), &2, "both sessions import")?;

        let finished = sessions
            .iter()
            .find(|session| session.source_path.as_str() == "/tmp/finished.jsonl")
            .ok_or_else(|| "missing finished session".to_string())?;
        ensure_equal(
            &finished.ended_at.as_deref(),
            &Some("2026-04-30T00:00:00Z"),
            "finished session keeps its end timestamp",
        )?;

        let live = sessions
            .iter()
            .find(|session| session.source_path.as_str() == "/tmp/live.jsonl")
            .ok_or_else(|| "missing live session".to_string())?;
        ensure_equal(
            &live.ended_at,
            &None::<String>,
            "null modified imports as ended_at = None",
        )?;
        // The fallback content hash records `modified` as missing metadata so the
        // hole stays observable rather than silently swallowed.
        ensure(
            live.missing_metadata
                .iter()
                .any(|field| field == "modified"),
            format!(
                "null modified should be observable in missing_metadata, got {:?}",
                live.missing_metadata
            ),
        )
    }

    #[test]
    fn null_cass_session_started_at_imports_with_none_start(/* issue #13 */) -> TestResult {
        let input = br#"{
          "sessions": [
            {
              "path": "/tmp/live.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "started_at": null,
              "modified": "2026-04-30T00:00:00Z"
            }
          ]
        }"#;

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&sessions.len(), &1, "session imports")?;
        let first = sessions
            .first()
            .ok_or_else(|| "missing parsed session".to_string())?;
        ensure_equal(
            &first.started_at,
            &None::<String>,
            "null started_at imports as None",
        )?;
        ensure_equal(
            &first.ended_at.as_deref(),
            &Some("2026-04-30T00:00:00Z"),
            "end timestamp still parses",
        )
    }

    #[test]
    fn empty_and_fallthrough_cass_session_timestamps(/* issue #13 */) -> TestResult {
        // An empty/whitespace-only end timestamp is treated as absent, and a
        // null leading candidate falls through to the next usable field.
        let input = br#"{
          "sessions": [
            {
              "path": "/tmp/blank.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "modified": "   "
            },
            {
              "path": "/tmp/fallthrough.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "ended_at": null,
              "modified": "2026-05-01T12:00:00Z"
            }
          ]
        }"#;

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&sessions.len(), &2, "both sessions import")?;

        let blank = sessions
            .iter()
            .find(|session| session.source_path.as_str() == "/tmp/blank.jsonl")
            .ok_or_else(|| "missing blank session".to_string())?;
        ensure_equal(
            &blank.ended_at,
            &None::<String>,
            "whitespace-only modified imports as None",
        )?;

        let fallthrough = sessions
            .iter()
            .find(|session| session.source_path.as_str() == "/tmp/fallthrough.jsonl")
            .ok_or_else(|| "missing fallthrough session".to_string())?;
        ensure_equal(
            &fallthrough.ended_at.as_deref(),
            &Some("2026-05-01T12:00:00Z"),
            "null ended_at falls through to modified",
        )
    }

    #[test]
    fn cass_view_span_content_hash_is_canonical_blake3(/* issue #10 */) -> TestResult {
        let excerpt = "assistant: applied the patch and verified the build";
        let line = json!({
            "line": 7,
            "content": excerpt,
        });

        let span = parse_view_line_value(&line, "/tmp/session.jsonl")
            .map_err(|error| error.to_string())?;

        ensure(
            span.content_hash.starts_with("blake3:"),
            format!(
                "evidence-span content_hash must carry the blake3: prefix, got {:?}",
                span.content_hash
            ),
        )?;
        // 7-char prefix + 64 hex chars.
        ensure_equal(
            &span.content_hash.len(),
            &71,
            "canonical content_hash length",
        )?;
        // Lossless: the prefix wraps the bare blake3 hex of the stored excerpt.
        let expected = format!("blake3:{}", blake3_hex(&span.excerpt));
        ensure_equal(
            &span.content_hash,
            &expected,
            "content_hash equals blake3:<hex(excerpt)>",
        )?;
        let bare = span
            .content_hash
            .strip_prefix("blake3:")
            .ok_or_else(|| "content_hash missing blake3: prefix".to_string())?;
        ensure(
            bare.len() == 64 && bare.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "stripped content_hash must be 64 lowercase hex chars",
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
    fn parse_sessions_rejects_invalid_optional_timestamps() -> TestResult {
        for (field, value) in [
            ("started_at", json!("not-a-timestamp")),
            ("started", json!(" 2026-05-07T06:00:01Z")),
            ("ended_at", json!(123)),
            ("modified", json!("2026-99-99T99:99:99Z")),
        ] {
            let input = format!(
                r#"{{
                  "sessions": [
                    {{
                      "path": "/tmp/session.jsonl",
                      "workspace": "/tmp/project",
                      "agent": "codex",
                      "{field}": {value}
                    }}
                  ]
                }}"#
            );

            let error = match parse_sessions_json(input.as_bytes()) {
                Ok(_) => return Err(format!("invalid {field} should fail")),
                Err(error) => error.to_string(),
            };
            ensure(
                error.contains(field) && error.contains("RFC3339"),
                format!("error should mention {field} RFC3339 requirement, got {error}"),
            )?;
        }
        Ok(())
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
    fn parses_legacy_cass_search_hits_with_rfc3339_created_at() -> TestResult {
        let input = br#"{
          "count": 2,
          "hits": [
            {
              "source_path": "/tmp/session.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "created_at": "2026-05-07T06:00:03Z"
            },
            {
              "source_path": "/tmp/session.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "created_at": "2026-05-07T06:00:01Z"
            }
          ]
        }"#;

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        ensure_equal(&sessions.len(), &1, "session count")?;
        let first = sessions
            .first()
            .ok_or_else(|| "missing parsed session".to_string())?;
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
    fn since_filter_keeps_legacy_rfc3339_created_at_sessions() -> TestResult {
        let input = br#"{
          "count": 2,
          "hits": [
            {
              "source_path": "/tmp/recent.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "created_at": "2026-05-07T06:00:01Z"
            },
            {
              "source_path": "/tmp/old.jsonl",
              "workspace": "/tmp/project",
              "agent": "codex",
              "created_at": "2026-03-07T06:00:01Z"
            }
          ]
        }"#;
        let cutoff = DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);

        let sessions = parse_sessions_json(input).map_err(|error| error.to_string())?;
        let filtered =
            filter_sessions_since(sessions, Some(cutoff)).map_err(|error| error.to_string())?;
        let paths: Vec<&str> = filtered
            .iter()
            .map(|session| session.source_path.as_str())
            .collect();

        ensure_equal(
            &paths,
            &vec!["/tmp/recent.jsonl"],
            "legacy RFC3339 since filter",
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
    fn parses_view_line_type_spelling_variants() -> TestResult {
        let input = br#"{
          "path": "/tmp/session.jsonl",
          "lines": [
            {"line": 3, "content": "{\"type\":\"ToolCall\",\"role\":\"assistant\"}"},
            {"line": 4, "content": "{\"type\":\"toolResult\",\"role\":\"tool\"}"},
            {"line": 5, "content": "{\"type\":\"fileHistorySnapshot\"}"},
            {"line": 6, "content": "{\"type\":\"summary\"}"}
          ]
        }"#;

        let spans =
            parse_view_json(input, "/tmp/session.jsonl").map_err(|error| error.to_string())?;
        ensure_equal(
            &spans.iter().map(|span| span.span_kind).collect::<Vec<_>>(),
            &vec![
                CassSpanKind::ToolCall,
                CassSpanKind::ToolResult,
                CassSpanKind::File,
                CassSpanKind::Summary,
            ],
            "line type variants",
        )?;
        ensure_equal(&spans[0].role, &Some(CassRole::Assistant), "tool call role")?;
        ensure_equal(&spans[1].role, &Some(CassRole::Tool), "tool result role")
    }

    #[test]
    fn parse_view_json_rejects_zero_line_numbers() -> TestResult {
        let input = br#"{"line": 0, "content": "zero"}"#;

        let error = match parse_view_json(input, "/tmp/session.jsonl") {
            Ok(_) => return Err("line 0 should fail before span construction".to_string()),
            Err(error) => error.to_string(),
        };
        ensure(
            error.contains("positive numeric line"),
            format!("error should mention positive line numbers, got {error}"),
        )
    }

    #[test]
    fn parse_view_json_rejects_duplicate_line_numbers() -> TestResult {
        let input = br#"{
          "path": "/tmp/session.jsonl",
          "lines": [
            {"line": 3, "content": "first"},
            {"line": 3, "content": "second"}
          ]
        }"#;

        let error = match parse_view_json(input, "/tmp/session.jsonl") {
            Ok(_) => return Err("duplicate line numbers should fail".to_string()),
            Err(error) => error.to_string(),
        };
        ensure(
            error.contains("duplicate line 3"),
            format!("error should mention duplicate line, got {error}"),
        )
    }

    #[test]
    fn parse_view_jsonl_rejects_oversized_lines() -> TestResult {
        let oversized_content = "x".repeat(CASS_STDOUT_LINE_MAX_BYTES);
        let input = json!({
            "line": 1,
            "content": oversized_content,
        })
        .to_string();

        let error = match parse_view_json(input.as_bytes(), "/tmp/session.jsonl") {
            Ok(_) => return Err("oversized CASS view JSONL line should fail".to_string()),
            Err(error) => error.to_string(),
        };
        ensure(
            error.contains("line exceeds"),
            format!("error should mention line limit, got {error}"),
        )
    }

    #[test]
    fn parse_view_json_rejects_single_line_envelope_over_line_cap() -> TestResult {
        let oversized_content = "x".repeat(CASS_STDOUT_LINE_MAX_BYTES);
        let input = json!({
            "lines": [{
                "line": 1,
                "content": oversized_content,
            }],
        })
        .to_string();

        let error = match parse_view_json(input.as_bytes(), "/tmp/session.jsonl") {
            Ok(_) => {
                return Err(
                    "single-line CASS view envelope over the line cap should fail".to_string(),
                );
            }
            Err(error) => error.to_string(),
        };
        ensure(
            error.contains("line exceeds"),
            format!("error should mention line limit, got {error}"),
        )
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
        let mut report = CassImportReport {
            schema: IMPORT_CASS_SCHEMA_V1,
            workspace_path: "/tmp/work".to_string(),
            database_path: Some("/tmp/work/.ee/ee.db".to_string()),
            source_id: "cass://safe-source".to_string(),
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
                source_path: "cass-session://safe-session".to_string(),
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
        ensure_equal(&json["sourceId"], &json!("cass://safe-source"), "source id")?;
        ensure_equal(
            &json["sessions"][0]["sourcePath"],
            &json!("cass-session://safe-session"),
            "safe source path",
        )?;
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
        )?;

        let exact_repair = report
            .index_required_action
            .clone()
            .ok_or_else(|| "fixture rebuild action is missing".to_owned())?;
        let degradation = report.record_index_publish_failure("injected staged publisher failure");
        ensure_equal(
            &report.index_required_action.as_deref(),
            &Some(exact_repair.as_str()),
            "publish failure preserves exact repair",
        )?;
        ensure_equal(
            &degradation.code,
            &"cass_import_index_publish_failed",
            "publish failure degradation code",
        )?;
        ensure(
            degradation
                .message
                .contains("CASS import committed successfully"),
            "publish failure distinguishes committed import from derived-index failure",
        )?;
        let degraded_json = degradation.data_json();
        ensure_equal(
            &degraded_json["severity"],
            &json!("medium"),
            "publish failure degradation severity",
        )?;
        ensure_equal(
            &degraded_json["repair"],
            &json!(exact_repair),
            "publish failure degradation exact repair",
        )?;

        report.record_index_publish_success();
        ensure_equal(
            &report.index_required_action,
            &None,
            "publish success removes rebuild action",
        )?;
        Ok(())
    }

    #[test]
    fn report_json_redacts_sensitive_source_refs() -> TestResult {
        let source_secret = format!("sk_live_{}", "1234567890abcdef");
        let session_token = format!("ghp_{}", "1234567890abcdef1234567890abcdef1234");
        let report = CassImportReport {
            schema: IMPORT_CASS_SCHEMA_V1,
            workspace_path: "/Users/alice/project".to_string(),
            database_path: Some("/Users/alice/project/.ee/ee.db".to_string()),
            source_id: format!(
                "cass://sessions?workspace=/Users/alice/private&api_key={source_secret}"
            ),
            ledger_id: Some("imp_abc".to_string()),
            dry_run: false,
            since: None,
            sessions_discovered: 1,
            sessions_imported: 1,
            sessions_skipped: 0,
            spans_imported: 2,
            index_jobs_queued: 1,
            index_required_action: None,
            status: "completed".to_string(),
            sessions: vec![ImportedCassSession {
                source_path: format!(
                    "file:///Users/alice/.codex/sessions/session.jsonl?token={session_token}"
                ),
                session_id: Some("sess_abc".to_string()),
                index_job_id: None,
                status: ImportSessionStatus::Imported,
                spans_imported: 2,
                message_count: Some(3),
                missing_metadata: Vec::new(),
            }],
        };

        let json = report.data_json();
        let source_id = json["sourceId"]
            .as_str()
            .ok_or_else(|| "expected sourceId string".to_string())?;
        let source_path = json["sessions"][0]["sourcePath"]
            .as_str()
            .ok_or_else(|| "expected session sourcePath string".to_string())?;
        let rendered_source_refs = format!("{source_id}\n{source_path}");

        ensure(
            rendered_source_refs.contains("[REDACTED_PATH]"),
            "sensitive paths should be redacted",
        )?;
        ensure(
            !rendered_source_refs.contains("/Users/alice"),
            "raw user path must not appear in public source refs",
        )?;
        ensure(
            !rendered_source_refs.contains(&source_secret),
            "source id secret must not appear in public import report JSON",
        )?;
        ensure(
            !rendered_source_refs.contains(&session_token),
            "session path token must not appear in public import report JSON",
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
    fn import_persists_and_reschedules_the_same_session_index_job() -> TestResult {
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
        )?;

        let assert_retry_state = |expected_status: crate::db::SearchIndexJobStatus,
                                  label: &str|
         -> TestResult {
            let stored = connection
                .get_search_index_job(index_job_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{label} job row disappeared"))?;
            ensure(
                stored.status_enum() == Some(expected_status),
                format!("{label} fixture state must be durable: {stored:?}"),
            )?;

            let retry =
                import_cass_sessions(&client, &options).map_err(|error| error.to_string())?;
            ensure(
                retry.sessions_imported == 0 && retry.sessions_skipped == 1,
                format!("{label} retry must skip the one durable session: {retry:?}"),
            )?;
            ensure(
                retry.index_jobs_queued == 1,
                format!("{label} retry must schedule reconciliation: {retry:?}"),
            )?;
            let retried_session = retry
                .sessions
                .first()
                .ok_or_else(|| format!("{label} retry report omitted its session"))?;
            ensure(
                retried_session.status == ImportSessionStatus::Skipped
                    && retried_session.session_id.as_deref() == Some(session_id)
                    && retried_session.index_job_id.as_deref() == Some(index_job_id),
                format!(
                    "{label} retry must preserve the exact session and index job IDs: {retried_session:?}"
                ),
            )
        };

        ensure(
            connection
                .cancel_search_index_job(index_job_id)
                .map_err(|error| error.to_string())?,
            "fixture job cancels while pending",
        )?;
        assert_retry_state(
            crate::db::SearchIndexJobStatus::Cancelled,
            "cancelled CASS publication",
        )?;

        ensure(
            connection
                .requeue_cancelled_search_index_jobs(&workspace_id)
                .map_err(|error| error.to_string())?
                == 1,
            "cancelled fixture job requeues in place",
        )?;
        ensure(
            connection
                .start_search_index_job(index_job_id)
                .map_err(|error| error.to_string())?,
            "fixture job enters running state",
        )?;
        let publish_lock = crate::db::AdvisoryLockId::index(&workspace_id);
        let live_holder = format!("index:{}:cass-state-matrix", std::process::id());
        ensure(
            connection
                .acquire_advisory_lock(
                    &publish_lock,
                    &live_holder,
                    Some(600),
                    Some("CASS running-state fixture"),
                )
                .map_err(|error| error.to_string())?
                .is_acquired(),
            "running fixture job has a live publication owner",
        )?;
        assert_retry_state(
            crate::db::SearchIndexJobStatus::Running,
            "live-running CASS publication",
        )?;
        ensure(
            connection
                .requeue_cancelled_search_index_jobs(&workspace_id)
                .map_err(|error| error.to_string())?
                == 0,
            "retry recovery must not steal a live-running CASS publication",
        )?;
        ensure(
            connection
                .release_advisory_lock(&publish_lock, &live_holder)
                .map_err(|error| error.to_string())?,
            "running fixture owner releases its publication lease",
        )?;
        ensure(
            connection
                .requeue_cancelled_search_index_jobs(&workspace_id)
                .map_err(|error| error.to_string())?
                == 1,
            "orphaned running CASS job requeues in place",
        )?;
        ensure(
            connection
                .start_search_index_job(index_job_id)
                .map_err(|error| error.to_string())?,
            "recovered fixture job starts again",
        )?;
        ensure(
            connection
                .fail_search_index_job(index_job_id, "injected publication failure")
                .map_err(|error| error.to_string())?,
            "fixture job fails",
        )?;
        assert_retry_state(
            crate::db::SearchIndexJobStatus::Failed,
            "failed CASS publication",
        )
    }

    #[cfg(unix)]
    #[test]
    fn import_streams_synthetic_1000_doc_view_under_memory_budget() -> TestResult {
        reset_cass_view_stream_peak_sample();
        let root = unique_test_dir("stream-1000-view-lines")?;
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
        write_fake_cass_binary_with_view_lines(&cass_binary, &workspace_path, &session_path, 1000)?;
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
        ensure_equal(&report.spans_imported, &1000, "spans imported")?;
        let peak_sample = cass_view_stream_peak_sample_bytes();
        ensure(
            peak_sample < CASS_VIEW_STREAM_MEMORY_BUDGET_BYTES,
            format!(
                "streamed CASS view import sampled {peak_sample} bytes, budget {CASS_VIEW_STREAM_MEMORY_BUDGET_BYTES}"
            ),
        )?;
        let imported_session = report
            .sessions
            .first()
            .ok_or_else(|| "import report should include imported session".to_string())?;
        let session_id = imported_session
            .session_id
            .as_deref()
            .ok_or_else(|| "imported session should include id".to_string())?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let spans = connection
            .list_evidence_spans_for_session(session_id)
            .map_err(|error| error.to_string())?;
        ensure_equal(&spans.len(), &1000, "stored evidence spans")?;
        ensure_equal(&spans[0].start_line, &1, "first stored line")?;
        ensure_equal(&spans[999].end_line, &1000, "last stored line")
    }

    /// Writes a fake `cass` whose `view` emits the supplied stdout verbatim,
    /// so a test can feed the EXACT multi-line `{...,"lines":[...]}` envelope
    /// that real `cass view --json` produces (whose first stdout line is `{`).
    #[cfg(unix)]
    fn write_fake_cass_binary_with_verbatim_view(
        path: &Path,
        workspace_path: &Path,
        session_path: &Path,
        view_stdout: &str,
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
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  sessions) cat <<'EE_CASS_SESSIONS'\n{}\nEE_CASS_SESSIONS\n;;\n  view) cat <<'EE_CASS_VIEW'\n{}\nEE_CASS_VIEW\n;;\n  *) printf 'unexpected cass command: %s\\n' \"$1\" >&2; exit 2;;\nesac\n",
            sessions, view_stdout
        );
        fs::write(path, script).map_err(|error| error.to_string())?;
        let mut permissions = fs::metadata(path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).map_err(|error| error.to_string())
    }

    /// Regression guard for issue #11: real `cass view --json` emits ONE
    /// pretty-printed `{...,"lines":[...]}` envelope (the first stdout line is
    /// just `{`), so the streaming import must buffer the whole envelope and
    /// parse it via `parse_view_json` — not `serde_json::from_str` each stdout
    /// line. We drive the full streaming import against the SAME envelope
    /// fixture real cass produces and assert non-zero sessions and spans, so a
    /// regression to per-line parsing (which imports zero) is caught.
    #[cfg(unix)]
    #[test]
    fn import_streams_real_view_envelope_fixture_imports_nonzero_spans() -> TestResult {
        let root = unique_test_dir("stream-real-view-envelope")?;
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

        // The committed fixture is the verbatim pretty-printed envelope shape
        // that real `cass view --json` emits (matches cass 0.6.13 stdout).
        let fixture_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cass/v1/view.json");
        let view_stdout = fs::read_to_string(&fixture_path)
            .map_err(|error| format!("read view fixture {}: {error}", fixture_path.display()))?;
        // Sanity: confirm the fixture really is the multi-line pretty envelope
        // (first non-empty stdout line is a bare `{`), i.e. NOT per-line JSONL.
        let first_line = view_stdout
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim();
        ensure_equal(
            &first_line,
            &"{",
            "view fixture must be a pretty envelope whose first line is `{`",
        )?;

        let cass_binary = bin_dir.join("cass");
        write_fake_cass_binary_with_verbatim_view(
            &cass_binary,
            &workspace_path,
            &session_path,
            &view_stdout,
        )?;
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
        // The fixture has three `lines[]` entries; the import must produce a
        // non-zero span count (zero spans is exactly the issue #11 symptom).
        ensure_equal(&report.spans_imported, &3, "spans imported")?;
        let imported_session = report
            .sessions
            .first()
            .ok_or_else(|| "import report should include imported session".to_string())?;
        let session_id = imported_session
            .session_id
            .as_deref()
            .ok_or_else(|| "imported session should include id".to_string())?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let spans = connection
            .list_evidence_spans_for_session(session_id)
            .map_err(|error| error.to_string())?;
        ensure_equal(&spans.len(), &3, "stored evidence spans")?;
        ensure_equal(&spans[0].start_line, &1, "first stored line")?;
        ensure_equal(&spans[2].end_line, &3, "last stored line")
    }

    #[cfg(unix)]
    #[test]
    fn import_database_parent_rejects_symlinked_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("cass-import-db-symlink-parent")?;
        let outside_parent = root.join("outside-db-parent");
        fs::create_dir_all(&outside_parent).map_err(|error| error.to_string())?;
        let linked_parent = root.join("linked-db-parent");
        symlink(&outside_parent, &linked_parent).map_err(|error| error.to_string())?;
        let database_path = linked_parent.join("ee.db");

        let error = ensure_database_parent(&database_path)
            .expect_err("symlinked database parent must be rejected");
        ensure(
            error.to_string().contains("symlink component"),
            format!("unexpected error: {error}"),
        )?;
        ensure(
            !outside_parent.join("ee.db").exists(),
            "CASS import database setup must not follow symlinked parent",
        )
    }

    #[cfg(unix)]
    #[test]
    fn import_database_parent_rejects_symlinked_database_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("cass-import-db-symlink-file")?;
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let outside_database = root.join("outside-ee.db");
        fs::write(&outside_database, b"outside").map_err(|error| error.to_string())?;
        let database_path = root.join("ee.db");
        symlink(&outside_database, &database_path).map_err(|error| error.to_string())?;

        let error = ensure_database_parent(&database_path)
            .expect_err("symlinked database file must be rejected");
        ensure(
            error.to_string().contains("symlink component"),
            format!("unexpected error: {error}"),
        )
    }

    #[cfg(unix)]
    #[test]
    fn import_database_parent_rejects_non_regular_database_path() -> TestResult {
        let root = unique_test_dir("cass-import-db-directory")?;
        let database_path = root.join("ee.db");
        fs::create_dir_all(&database_path).map_err(|error| error.to_string())?;

        let error = ensure_database_parent(&database_path)
            .expect_err("directory database path must be rejected before open");
        ensure(
            error.to_string().contains("not a regular file"),
            format!("unexpected error: {error}"),
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
        let session_id = stable_session_id(&workspace_id, "/tmp/session.jsonl");
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

    #[test]
    fn index_required_action_shell_quotes_unsafe_paths() -> TestResult {
        let action = index_required_action(
            Path::new("/tmp/work dir/it's"),
            Some(Path::new("/tmp/db path/ee$(bad).db")),
        );
        let expected = concat!(
            "ee index rebuild --workspace '/tmp/work dir/it'\\''s' ",
            "--database '/tmp/db path/ee$(bad).db'"
        )
        .to_string();

        ensure_equal(&action, &expected, "quoted index required action")
    }

    #[test]
    fn stable_session_ids_are_workspace_scoped() -> TestResult {
        let source_path = "/tmp/session.jsonl";
        let first_workspace = stable_workspace_id("/tmp/work-a");
        let second_workspace = stable_workspace_id("/tmp/work-b");
        let first_session = stable_session_id(&first_workspace, source_path);
        let second_session = stable_session_id(&second_workspace, source_path);

        ensure(
            first_session != second_session,
            "same cass source path in different workspaces must not collide",
        )?;
        ensure(
            first_session.starts_with("sess_") && first_session.len() == 31,
            "first session id shape",
        )?;
        ensure(
            second_session.starts_with("sess_") && second_session.len() == 31,
            "second session id shape",
        )
    }
}
