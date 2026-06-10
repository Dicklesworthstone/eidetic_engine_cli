//! Append-only agent observation journal (ADR 0062, bd-1pi9m.2).
//!
//! `ee journal append` captures raw observations (failures, surprises,
//! notes) with near-zero ceremony; `ee journal list`/`show` are the
//! read-only inspection surfaces. Entries are deliberately NOT indexed
//! in Frankensearch (ADR 0062 §2) and never advance workspace
//! generations — promotion into the indexed store happens only through
//! distillation (bd-1pi9m.3) and the existing curation machinery.
//!
//! Every `body` and structured string field passes the policy redaction
//! screen BEFORE any byte is persisted (ADR 0062 §3): secrets never
//! reach disk, applied classes land in `redaction_report`, and
//! instruction-like content is stored but graded with the existing
//! `InstructionRisk` vocabulary so distillation can abstain later.

use std::path::{Path, PathBuf};

use crate::core::curate::stable_workspace_id;
use crate::db::{
    CreateJournalEntryInput, CreateWorkspaceInput, DbConnection, JournalEntryListFilter,
    StoredJournalEntry,
};
use crate::models::DomainError;
use crate::policy::{InstructionRisk, detect_instruction_like_content, redact_secret_like_content};

/// Stable schema id for one journal entry payload (ADR 0062 Appendix A).
pub const JOURNAL_ENTRY_SCHEMA_V1: &str = "ee.journal.entry.v1";

/// Degraded code: `[journal] enabled = false` refused the operation.
pub const JOURNAL_DISABLED_CODE: &str = "journal_disabled";
/// Degraded code: oversize body was truncated deterministically.
pub const JOURNAL_ENTRY_TRUNCATED_CODE: &str = "journal_entry_truncated";
/// Degraded code: secret classes were redacted before storage.
pub const JOURNAL_REDACTION_APPLIED_CODE: &str = "journal_redaction_applied";

/// Hard cap on stored body bytes (ADR 0062 §1). Oversize input truncates
/// deterministically at the last char boundary at or below the cap;
/// truncation never errors.
pub const JOURNAL_BODY_MAX_BYTES: usize = 16 * 1024;
/// Structured sidecar per-field cap: `cmd`.
pub const JOURNAL_CMD_MAX_BYTES: usize = 2 * 1024;
/// Structured sidecar per-field cap: `cwd`.
pub const JOURNAL_CWD_MAX_BYTES: usize = 1024;
/// Structured sidecar per-field cap: `stderrTail`.
pub const JOURNAL_STDERR_TAIL_MAX_BYTES: usize = 2 * 1024;
/// Structured sidecar cap: maximum `paths[]` entries.
pub const JOURNAL_PATHS_MAX_ENTRIES: usize = 16;
/// Structured sidecar per-entry cap for `paths[]`.
pub const JOURNAL_PATH_ENTRY_MAX_BYTES: usize = 1024;
/// Total serialized structured sidecar cap.
pub const JOURNAL_STRUCTURED_MAX_BYTES: usize = 8 * 1024;
/// Maximum `session_key` bytes (enforced in code, not in the schema).
pub const JOURNAL_SESSION_KEY_MAX_BYTES: usize = 128;
/// Maximum JSONL lines per `ee journal append --stdin` invocation.
pub const JOURNAL_STDIN_MAX_LINES: usize = 512;
/// Default `[journal] retention_days` when the config key is absent.
/// Enforcement is the explicit `journal-retention` steward job
/// (bd-1pi9m.5); this bead only defines the config surface.
pub const JOURNAL_DEFAULT_RETENTION_DAYS: u64 = 14;

/// Entry kind vocabulary (ADR 0062 §1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalKind {
    Observation,
    CommandFailure,
    Surprise,
    Note,
}

impl JournalKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::CommandFailure => "command_failure",
            Self::Surprise => "surprise",
            Self::Note => "note",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "observation" => Some(Self::Observation),
            "command_failure" => Some(Self::CommandFailure),
            "surprise" => Some(Self::Surprise),
            "note" => Some(Self::Note),
            _ => None,
        }
    }
}

/// Append source vocabulary (ADR 0062 §1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalSource {
    Hook,
    Manual,
    Stdin,
}

impl JournalSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Manual => "manual",
            Self::Stdin => "stdin",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "hook" => Some(Self::Hook),
            "manual" => Some(Self::Manual),
            "stdin" => Some(Self::Stdin),
            _ => None,
        }
    }
}

/// Response-level degraded entry for journal commands. Mirrors
/// `core::outcome::OutcomeDegradation` so agents branch on stable codes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
}

impl JournalDegradation {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
        })
    }
}

fn journal_disabled_degradation() -> JournalDegradation {
    JournalDegradation {
        code: JOURNAL_DISABLED_CODE,
        severity: "info",
        message: "Journal capture is disabled by config ([journal] enabled = false); no journal \
                  rows were read or written. Set [journal] enabled = true in .ee/config.toml to \
                  re-enable."
            .to_owned(),
    }
}

fn journal_truncated_degradation(raw_bytes: usize, stored_bytes: usize) -> JournalDegradation {
    JournalDegradation {
        code: JOURNAL_ENTRY_TRUNCATED_CODE,
        severity: "info",
        message: format!(
            "Journal body exceeded {JOURNAL_BODY_MAX_BYTES} bytes and was truncated \
             deterministically at the last char boundary at or below the cap; stored \
             {stored_bytes} of {raw_bytes} bytes."
        ),
    }
}

fn journal_redaction_degradation(span_count: usize, classes: &[String]) -> JournalDegradation {
    JournalDegradation {
        code: JOURNAL_REDACTION_APPLIED_CODE,
        severity: "info",
        message: format!(
            "Redaction screen replaced {span_count} secret-like span(s) [{}] before persistence; \
             the stored entry contains placeholders only.",
            classes.join(", ")
        ),
    }
}

/// Generate a journal entry id: `jrn_` + UUIDv7 (time-ordered within a
/// process, ADR 0062 §1).
#[must_use]
pub fn generate_journal_entry_id() -> String {
    format!("jrn_{}", uuid::Uuid::now_v7())
}

/// Raw (pre-validation) input for one journal entry. The CLI fills this
/// from flags; the JSONL batch path fills it per line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JournalEntryDraft {
    pub body: String,
    pub kind: Option<String>,
    pub session_key: Option<String>,
    pub cmd: Option<String>,
    pub exit_code: Option<i64>,
    pub cwd: Option<String>,
    pub paths: Vec<String>,
    pub stderr_tail: Option<String>,
}

/// Options shared by the journal append surfaces.
#[derive(Clone, Debug)]
pub struct JournalAppendOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    /// `EE_AGENT_NAME` at append time; the CLI passes
    /// `crate::core::memory_scope::current_agent_name()`.
    pub agent_name: Option<String>,
    pub source: JournalSource,
}

/// Options for `ee journal list`.
#[derive(Clone, Debug)]
pub struct JournalListOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub session_key: Option<String>,
    pub agent_name: Option<String>,
    pub since: Option<String>,
    pub kind: Option<String>,
    pub undistilled_only: bool,
    pub limit: u32,
}

/// Options for `ee journal show <entry-id>`.
#[derive(Clone, Debug)]
pub struct JournalShowOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub entry_id: &'a str,
}

/// Per-line validation failure with a stable, agent-branchable code.
#[derive(Clone, Debug, Eq, PartialEq)]
struct JournalValidationError {
    code: &'static str,
    message: String,
}

impl JournalValidationError {
    fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }
}

/// One journal entry shaped per ADR 0062 Appendix A field names.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalEntryRecord {
    pub entry_id: String,
    pub workspace_id: String,
    pub agent_name: Option<String>,
    pub session_key: Option<String>,
    pub kind: String,
    pub source: String,
    pub body: String,
    pub structured: Option<serde_json::Value>,
    pub redaction_report: serde_json::Value,
    pub instruction_risk: String,
    pub created_at: String,
    pub distilled_at: Option<String>,
    pub tombstoned_at: Option<String>,
}

impl JournalEntryRecord {
    fn from_stored(stored: &StoredJournalEntry) -> Result<Self, DomainError> {
        let structured = stored
            .structured
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Journal entry {} has a corrupt structured sidecar: {error}",
                    stored.entry_id
                ),
                repair: Some("ee doctor".to_owned()),
            })?;
        let redaction_report = serde_json::from_str::<serde_json::Value>(&stored.redaction_report)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Journal entry {} has a corrupt redaction report: {error}",
                    stored.entry_id
                ),
                repair: Some("ee doctor".to_owned()),
            })?;
        Ok(Self {
            entry_id: stored.entry_id.clone(),
            workspace_id: stored.workspace_id.clone(),
            agent_name: stored.agent_name.clone(),
            session_key: stored.session_key.clone(),
            kind: stored.kind.clone(),
            source: stored.source.clone(),
            body: stored.body.clone(),
            structured,
            redaction_report,
            instruction_risk: stored.instruction_risk.clone(),
            created_at: stored.created_at.clone(),
            distilled_at: stored.distilled_at.clone(),
            tombstoned_at: stored.tombstoned_at.clone(),
        })
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": JOURNAL_ENTRY_SCHEMA_V1,
            "entryId": &self.entry_id,
            "workspaceId": &self.workspace_id,
            "agentName": &self.agent_name,
            "sessionKey": &self.session_key,
            "kind": &self.kind,
            "source": &self.source,
            "body": &self.body,
            "structured": &self.structured,
            "redactionReport": &self.redaction_report,
            "instructionRisk": &self.instruction_risk,
            "createdAt": &self.created_at,
            "distilledAt": &self.distilled_at,
            "tombstonedAt": &self.tombstoned_at,
        })
    }
}

/// Result of one `ee journal append` (single-entry surface).
#[derive(Clone, Debug, PartialEq)]
pub struct JournalAppendReport {
    pub version: &'static str,
    /// `stored` or `journal_disabled`.
    pub status: &'static str,
    pub entry: Option<JournalEntryRecord>,
    pub truncated: bool,
    pub redaction_applied: bool,
    pub degraded: Vec<JournalDegradation>,
}

impl JournalAppendReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "journal append",
            "version": self.version,
            "status": self.status,
            "entry": self.entry.as_ref().map(JournalEntryRecord::data_json),
            "truncated": self.truncated,
            "redactionApplied": self.redaction_applied,
            "degraded": self.degraded.iter().map(JournalDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        if let Some(entry) = &self.entry {
            output.push_str("Journal entry stored\n\n");
            output.push_str(&format!("  Entry: {}\n", entry.entry_id));
            output.push_str(&format!("  Kind: {}\n", entry.kind));
            output.push_str(&format!("  Source: {}\n", entry.source));
            if let Some(session_key) = &entry.session_key {
                output.push_str(&format!("  Session: {session_key}\n"));
            }
            if self.truncated {
                output.push_str("  Truncated: body exceeded the 16 KiB cap\n");
            }
            if self.redaction_applied {
                output.push_str("  Redaction: secret-like spans replaced before storage\n");
            }
        } else {
            output.push_str("Journal entry NOT stored\n");
        }
        for degraded in &self.degraded {
            output.push_str(&format!("  [{}] {}\n", degraded.code, degraded.message));
        }
        output
    }
}

/// Per-line outcome for `ee journal append --stdin` (ADR 0062 §4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalBatchLineResult {
    /// 1-based line number in the piped JSONL input.
    pub line: usize,
    /// `stored` or `failed`.
    pub status: &'static str,
    pub entry_id: Option<String>,
    pub error_code: Option<&'static str>,
    pub error_message: Option<String>,
    pub truncated: bool,
    pub redaction_applied: bool,
}

impl JournalBatchLineResult {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "line": self.line,
            "status": self.status,
            "entryId": &self.entry_id,
            "errorCode": &self.error_code,
            "errorMessage": &self.error_message,
            "truncated": self.truncated,
            "redactionApplied": self.redaction_applied,
        })
    }
}

/// Result of one `ee journal append --stdin` batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalBatchReport {
    pub version: &'static str,
    /// `stored` or `journal_disabled`.
    pub status: &'static str,
    pub line_count: usize,
    pub stored_count: usize,
    pub failed_count: usize,
    pub results: Vec<JournalBatchLineResult>,
    pub degraded: Vec<JournalDegradation>,
}

impl JournalBatchReport {
    /// `true` when every supplied line failed (exit 5 per ADR 0062 §4).
    /// Defined on `failed_count` (not `stored_count`) so the
    /// `journal_disabled` refusal — zero stored, zero failed — stays a
    /// success envelope rather than an exit-5 batch failure.
    #[must_use]
    pub const fn all_failed(&self) -> bool {
        self.line_count > 0 && self.failed_count == self.line_count
    }

    #[must_use]
    pub fn results_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.results
                .iter()
                .map(JournalBatchLineResult::data_json)
                .collect(),
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "journal append",
            "version": self.version,
            "status": self.status,
            "lineCount": self.line_count,
            "storedCount": self.stored_count,
            "failedCount": self.failed_count,
            "results": self.results_json(),
            "degraded": self.degraded.iter().map(JournalDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!(
            "Journal batch: {} stored, {} failed ({} lines)\n",
            self.stored_count, self.failed_count, self.line_count
        );
        for result in &self.results {
            match result.status {
                "stored" => output.push_str(&format!(
                    "  line {}: stored {}\n",
                    result.line,
                    result.entry_id.as_deref().unwrap_or("")
                )),
                _ => output.push_str(&format!(
                    "  line {}: failed [{}] {}\n",
                    result.line,
                    result.error_code.unwrap_or("unknown"),
                    result.error_message.as_deref().unwrap_or("")
                )),
            }
        }
        for degraded in &self.degraded {
            output.push_str(&format!("  [{}] {}\n", degraded.code, degraded.message));
        }
        output
    }
}

/// Result of `ee journal list`.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalListReport {
    pub version: &'static str,
    pub workspace_id: String,
    /// `ok` or `journal_disabled`.
    pub status: &'static str,
    pub entries: Vec<JournalEntryRecord>,
    pub degraded: Vec<JournalDegradation>,
}

impl JournalListReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "journal list",
            "version": self.version,
            "status": self.status,
            "workspaceId": &self.workspace_id,
            "entryCount": self.entries.len(),
            "entries": self.entries.iter().map(JournalEntryRecord::data_json).collect::<Vec<_>>(),
            "degraded": self.degraded.iter().map(JournalDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Journal entries ({})\n", self.entries.len());
        for entry in &self.entries {
            output.push_str(&format!(
                "  {} [{}] {} {}\n",
                entry.entry_id,
                entry.kind,
                entry.created_at,
                entry.agent_name.as_deref().unwrap_or("-")
            ));
        }
        for degraded in &self.degraded {
            output.push_str(&format!("  [{}] {}\n", degraded.code, degraded.message));
        }
        output
    }
}

/// Result of `ee journal show <entry-id>`.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalShowReport {
    pub version: &'static str,
    /// `ok` or `journal_disabled`.
    pub status: &'static str,
    pub entry: Option<JournalEntryRecord>,
    pub degraded: Vec<JournalDegradation>,
}

impl JournalShowReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "journal show",
            "version": self.version,
            "status": self.status,
            "entry": self.entry.as_ref().map(JournalEntryRecord::data_json),
            "degraded": self.degraded.iter().map(JournalDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        if let Some(entry) = &self.entry {
            output.push_str(&format!("Journal entry {}\n\n", entry.entry_id));
            output.push_str(&format!("  Kind: {}\n", entry.kind));
            output.push_str(&format!("  Source: {}\n", entry.source));
            output.push_str(&format!("  Created: {}\n", entry.created_at));
            output.push_str(&format!(
                "  Agent: {}\n",
                entry.agent_name.as_deref().unwrap_or("-")
            ));
            output.push_str(&format!(
                "  Session: {}\n",
                entry.session_key.as_deref().unwrap_or("-")
            ));
            output.push_str(&format!("  Instruction risk: {}\n", entry.instruction_risk));
            output.push_str(&format!(
                "  Distilled: {}\n",
                entry.distilled_at.as_deref().unwrap_or("-")
            ));
            output.push_str(&format!("\n{}\n", entry.body));
        }
        for degraded in &self.degraded {
            output.push_str(&format!("  [{}] {}\n", degraded.code, degraded.message));
        }
        output
    }
}

/// Whether `[journal]` capture is enabled for the workspace
/// (default true when the key or the config file is absent).
#[must_use]
pub fn journal_capture_enabled(workspace_path: &Path) -> bool {
    crate::config::workspace_config(workspace_path)
        .and_then(|config| config.journal.enabled)
        .unwrap_or(true)
}

/// Effective `[journal] retention_days` (enforced by bd-1pi9m.5).
#[must_use]
pub fn journal_retention_days(workspace_path: &Path) -> u64 {
    crate::config::workspace_config(workspace_path)
        .and_then(|config| config.journal.retention_days)
        .unwrap_or(JOURNAL_DEFAULT_RETENTION_DAYS)
}

/// Append one journal entry (the `ee journal append "<text>"` surface).
pub fn append_journal_entry(
    options: &JournalAppendOptions<'_>,
    draft: &JournalEntryDraft,
) -> Result<JournalAppendReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    if !journal_capture_enabled(&workspace_path) {
        return Ok(JournalAppendReport {
            version: env!("CARGO_PKG_VERSION"),
            status: JOURNAL_DISABLED_CODE,
            entry: None,
            truncated: false,
            redaction_applied: false,
            degraded: vec![journal_disabled_degradation()],
        });
    }

    let prepared = prepare_journal_entry(draft).map_err(|error| DomainError::Usage {
        message: format!("{} ({})", error.message, error.code),
        repair: Some("ee journal append --help".to_owned()),
    })?;

    let database_path = effective_database_path(&workspace_path, options.database_path);
    let connection = open_journal_database(&database_path)?;
    let workspace_id = stable_workspace_id(&workspace_path);
    ensure_workspace(&connection, &workspace_id, &workspace_path)?;

    let stored = persist_prepared_entry(&connection, &workspace_id, options, &prepared)?;
    let entry = JournalEntryRecord::from_stored(&stored)?;
    let mut degraded = Vec::new();
    if prepared.truncated {
        degraded.push(journal_truncated_degradation(
            prepared.raw_body_bytes,
            stored.body.len(),
        ));
    }
    if prepared.redaction_applied {
        degraded.push(journal_redaction_degradation(
            prepared.redaction_span_count,
            &prepared.redaction_classes,
        ));
    }

    Ok(JournalAppendReport {
        version: env!("CARGO_PKG_VERSION"),
        status: "stored",
        entry: Some(entry),
        truncated: prepared.truncated,
        redaction_applied: prepared.redaction_applied,
        degraded,
    })
}

/// Append a JSONL batch (the `ee journal append --stdin` surface).
///
/// Each line is validated and persisted independently — one poisoned
/// line cannot roll back the rest of a session flush (ADR 0062 §4).
pub fn append_journal_entries_stdin(
    options: &JournalAppendOptions<'_>,
    input: &str,
) -> Result<JournalBatchReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    let lines: Vec<&str> = input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Err(DomainError::Usage {
            message: "journal append --stdin requires at least one JSONL line".to_owned(),
            repair: Some(
                "printf '%s\\n' '{\"body\":\"...\"}' | ee journal append --stdin --json".to_owned(),
            ),
        });
    }
    if lines.len() > JOURNAL_STDIN_MAX_LINES {
        return Err(DomainError::Usage {
            message: format!(
                "journal append --stdin accepts at most {JOURNAL_STDIN_MAX_LINES} lines per \
                 invocation; got {}",
                lines.len()
            ),
            repair: Some("split the JSONL input into smaller batches".to_owned()),
        });
    }

    if !journal_capture_enabled(&workspace_path) {
        return Ok(JournalBatchReport {
            version: env!("CARGO_PKG_VERSION"),
            status: JOURNAL_DISABLED_CODE,
            line_count: lines.len(),
            stored_count: 0,
            failed_count: 0,
            results: Vec::new(),
            degraded: vec![journal_disabled_degradation()],
        });
    }

    let database_path = effective_database_path(&workspace_path, options.database_path);
    let connection = open_journal_database(&database_path)?;
    let workspace_id = stable_workspace_id(&workspace_path);
    ensure_workspace(&connection, &workspace_id, &workspace_path)?;

    let mut results = Vec::with_capacity(lines.len());
    let mut stored_count = 0_usize;
    let mut truncated_any = false;
    let mut redacted_any = false;
    let mut redaction_span_total = 0_usize;
    let mut redaction_classes: Vec<String> = Vec::new();
    let mut max_raw_body = 0_usize;
    let mut min_stored_body = JOURNAL_BODY_MAX_BYTES;

    for (index, line) in lines.iter().enumerate() {
        let line_number = index + 1;
        let outcome = parse_journal_line(line)
            .and_then(|draft| prepare_journal_entry(&draft))
            .and_then(|prepared| {
                // Per-line independent persistence: each insert is its own
                // implicit transaction, so a storage failure on this line
                // reports here without rolling back earlier lines.
                persist_prepared_entry(&connection, &workspace_id, options, &prepared)
                    .map(|stored| (prepared, stored))
                    .map_err(|error| {
                        JournalValidationError::new("journal_storage_failed", error.to_string())
                    })
            });
        match outcome {
            Ok((prepared, stored)) => {
                stored_count += 1;
                if prepared.truncated {
                    truncated_any = true;
                    max_raw_body = max_raw_body.max(prepared.raw_body_bytes);
                    min_stored_body = min_stored_body.min(stored.body.len());
                }
                if prepared.redaction_applied {
                    redacted_any = true;
                    redaction_span_total += prepared.redaction_span_count;
                    for class in &prepared.redaction_classes {
                        if !redaction_classes.contains(class) {
                            redaction_classes.push(class.clone());
                        }
                    }
                }
                results.push(JournalBatchLineResult {
                    line: line_number,
                    status: "stored",
                    entry_id: Some(stored.entry_id),
                    error_code: None,
                    error_message: None,
                    truncated: prepared.truncated,
                    redaction_applied: prepared.redaction_applied,
                });
            }
            Err(error) => {
                results.push(JournalBatchLineResult {
                    line: line_number,
                    status: "failed",
                    entry_id: None,
                    error_code: Some(error.code),
                    error_message: Some(error.message),
                    truncated: false,
                    redaction_applied: false,
                });
            }
        }
    }

    let mut degraded = Vec::new();
    if truncated_any {
        degraded.push(journal_truncated_degradation(max_raw_body, min_stored_body));
    }
    if redacted_any {
        redaction_classes.sort_unstable();
        degraded.push(journal_redaction_degradation(
            redaction_span_total,
            &redaction_classes,
        ));
    }

    Ok(JournalBatchReport {
        version: env!("CARGO_PKG_VERSION"),
        status: "stored",
        line_count: lines.len(),
        stored_count,
        failed_count: lines.len() - stored_count,
        results,
        degraded,
    })
}

/// List journal entries newest-first with optional filters (ADR 0062 §2).
pub fn list_journal_entries(
    options: &JournalListOptions<'_>,
) -> Result<JournalListReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    let workspace_id = stable_workspace_id(&workspace_path);
    if !journal_capture_enabled(&workspace_path) {
        return Ok(JournalListReport {
            version: env!("CARGO_PKG_VERSION"),
            status: JOURNAL_DISABLED_CODE,
            workspace_id,
            entries: Vec::new(),
            degraded: vec![journal_disabled_degradation()],
        });
    }
    if let Some(since) = options.since.as_deref() {
        validate_rfc3339("--since", since)?;
    }
    if let Some(kind) = options.kind.as_deref()
        && JournalKind::parse(kind).is_none()
    {
        return Err(journal_kind_usage_error(kind));
    }

    let database_path = effective_database_path(&workspace_path, options.database_path);
    let connection = open_journal_database(&database_path)?;
    let filter = JournalEntryListFilter {
        session_key: options.session_key.clone(),
        agent_name: options.agent_name.clone(),
        since: options.since.clone(),
        kind: options.kind.as_deref().map(str::trim).map(str::to_owned),
        undistilled_only: options.undistilled_only,
        limit: options.limit,
    };
    let stored = connection
        .list_journal_entries(&workspace_id, &filter)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list journal entries: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    let entries = stored
        .iter()
        .map(JournalEntryRecord::from_stored)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(JournalListReport {
        version: env!("CARGO_PKG_VERSION"),
        status: "ok",
        workspace_id,
        entries,
        degraded: Vec::new(),
    })
}

/// Show one journal entry by id (full record incl. `structured` and
/// `redaction_report`).
pub fn show_journal_entry(
    options: &JournalShowOptions<'_>,
) -> Result<JournalShowReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path)?;
    if !journal_capture_enabled(&workspace_path) {
        return Ok(JournalShowReport {
            version: env!("CARGO_PKG_VERSION"),
            status: JOURNAL_DISABLED_CODE,
            entry: None,
            degraded: vec![journal_disabled_degradation()],
        });
    }
    let entry_id = options.entry_id.trim();
    if entry_id.is_empty() {
        return Err(DomainError::Usage {
            message: "journal show requires an entry id".to_owned(),
            repair: Some("ee journal list --workspace . --json".to_owned()),
        });
    }

    let database_path = effective_database_path(&workspace_path, options.database_path);
    let connection = open_journal_database(&database_path)?;
    let stored = connection
        .get_journal_entry(entry_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read journal entry: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "journal entry".to_owned(),
            id: entry_id.to_owned(),
            repair: Some("ee journal list --workspace . --json".to_owned()),
        })?;

    Ok(JournalShowReport {
        version: env!("CARGO_PKG_VERSION"),
        status: "ok",
        entry: Some(JournalEntryRecord::from_stored(&stored)?),
        degraded: Vec::new(),
    })
}

/// A validated, screened, persistence-ready entry.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedJournalEntry {
    kind: JournalKind,
    session_key: Option<String>,
    body: String,
    structured_json: Option<String>,
    redaction_report_json: String,
    instruction_risk: InstructionRisk,
    truncated: bool,
    redaction_applied: bool,
    redaction_span_count: usize,
    redaction_classes: Vec<String>,
    raw_body_bytes: usize,
}

/// One screened text field. Mirrors the canonical
/// `crate::policy::screen_external_text_for_ingestion` sequence (redact
/// first, then grade instruction-likeness on the redacted text) while
/// preserving the per-span match count that the combined screen report
/// does not expose.
struct ScreenedJournalText {
    content: String,
    classes: Vec<&'static str>,
    span_count: usize,
    instruction_risk: InstructionRisk,
}

fn screen_journal_text(raw: &str) -> ScreenedJournalText {
    let redaction = redact_secret_like_content(raw);
    let instruction = detect_instruction_like_content(&redaction.content);
    ScreenedJournalText {
        classes: redaction.redacted_reasons,
        span_count: redaction.matches.len(),
        instruction_risk: instruction.risk,
        content: redaction.content,
    }
}

/// Accumulates redaction classes, span counts, and the worst instruction
/// risk across every screened field of one entry.
struct JournalScreenAccumulator {
    span_count: usize,
    classes: Vec<&'static str>,
    instruction_risk: InstructionRisk,
}

impl Default for JournalScreenAccumulator {
    fn default() -> Self {
        Self {
            span_count: 0,
            classes: Vec::new(),
            instruction_risk: InstructionRisk::None,
        }
    }
}

impl JournalScreenAccumulator {
    fn screen(&mut self, raw: &str) -> String {
        let screened = screen_journal_text(raw);
        self.span_count += screened.span_count;
        self.classes.extend(screened.classes);
        self.instruction_risk = self.instruction_risk.max(screened.instruction_risk);
        screened.content
    }
}

/// Deterministic truncation at the last char boundary at or below
/// `max_bytes` (ADR 0062 §1). Never errors.
fn truncate_at_char_boundary(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

fn journal_kind_usage_error(raw: &str) -> DomainError {
    DomainError::Usage {
        message: format!(
            "unknown journal kind `{raw}`; expected observation, command_failure, surprise, or \
             note"
        ),
        repair: Some("ee journal append --help".to_owned()),
    }
}

fn validate_rfc3339(flag: &str, raw: &str) -> Result<(), DomainError> {
    chrono::DateTime::parse_from_rfc3339(raw.trim()).map_err(|error| DomainError::Usage {
        message: format!("{flag} must be an RFC 3339 timestamp: {error}"),
        repair: Some("ee journal list --since 2026-06-10T00:00:00Z --json".to_owned()),
    })?;
    Ok(())
}

fn validate_field_bytes(
    code: &'static str,
    field: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), JournalValidationError> {
    if value.len() > max_bytes {
        return Err(JournalValidationError::new(
            code,
            format!(
                "{field} exceeds the {max_bytes}-byte cap ({} bytes)",
                value.len()
            ),
        ));
    }
    Ok(())
}

fn prepare_journal_entry(
    draft: &JournalEntryDraft,
) -> Result<PreparedJournalEntry, JournalValidationError> {
    if draft.body.trim().is_empty() {
        return Err(JournalValidationError::new(
            "journal_body_required",
            "journal entry body must not be empty".to_owned(),
        ));
    }

    let kind = match draft.kind.as_deref() {
        Some(raw) => JournalKind::parse(raw).ok_or_else(|| {
            JournalValidationError::new(
                "journal_kind_invalid",
                format!(
                    "unknown journal kind `{raw}`; expected observation, command_failure, \
                     surprise, or note"
                ),
            )
        })?,
        // Default kind: command_failure when machine fields are present,
        // note otherwise (bd-1pi9m.2 behavior contract).
        None if draft.cmd.is_some() || draft.exit_code.is_some() => JournalKind::CommandFailure,
        None => JournalKind::Note,
    };

    let session_key = draft
        .session_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned);
    if let Some(key) = session_key.as_deref() {
        validate_field_bytes(
            "journal_session_key_too_long",
            "session key",
            key,
            JOURNAL_SESSION_KEY_MAX_BYTES,
        )?;
    }
    if let Some(cmd) = draft.cmd.as_deref() {
        validate_field_bytes("journal_cmd_too_large", "cmd", cmd, JOURNAL_CMD_MAX_BYTES)?;
    }
    if let Some(cwd) = draft.cwd.as_deref() {
        validate_field_bytes("journal_cwd_too_large", "cwd", cwd, JOURNAL_CWD_MAX_BYTES)?;
    }
    if let Some(stderr_tail) = draft.stderr_tail.as_deref() {
        validate_field_bytes(
            "journal_stderr_tail_too_large",
            "stderr tail",
            stderr_tail,
            JOURNAL_STDERR_TAIL_MAX_BYTES,
        )?;
    }
    if draft.paths.len() > JOURNAL_PATHS_MAX_ENTRIES {
        return Err(JournalValidationError::new(
            "journal_paths_too_many",
            format!(
                "paths[] accepts at most {JOURNAL_PATHS_MAX_ENTRIES} entries; got {}",
                draft.paths.len()
            ),
        ));
    }
    for path in &draft.paths {
        validate_field_bytes(
            "journal_path_entry_too_large",
            "paths[] entry",
            path,
            JOURNAL_PATH_ENTRY_MAX_BYTES,
        )?;
    }

    // Screen before storage (ADR 0062 §3): truncate the raw body to its
    // hard cap, then redact body and every structured string field. The
    // stored bytes are always the REDACTED content.
    let raw_body_bytes = draft.body.len();
    let capped_body = truncate_at_char_boundary(&draft.body, JOURNAL_BODY_MAX_BYTES);
    let mut truncated = capped_body.len() < raw_body_bytes;

    let mut accumulator = JournalScreenAccumulator::default();

    let screened_body = accumulator.screen(capped_body);
    // Redaction placeholders can be longer than the secret they replace;
    // re-truncate so the stored body never exceeds the hard cap.
    let body = truncate_at_char_boundary(&screened_body, JOURNAL_BODY_MAX_BYTES);
    truncated = truncated || body.len() < screened_body.len();
    let body = body.to_owned();

    let cmd = draft.cmd.as_deref().map(|value| accumulator.screen(value));
    let cwd = draft.cwd.as_deref().map(|value| accumulator.screen(value));
    let stderr_tail = draft
        .stderr_tail
        .as_deref()
        .map(|value| accumulator.screen(value));
    let paths: Vec<String> = draft
        .paths
        .iter()
        .map(|path| accumulator.screen(path))
        .collect();

    let structured_json = if cmd.is_none()
        && draft.exit_code.is_none()
        && cwd.is_none()
        && paths.is_empty()
        && stderr_tail.is_none()
    {
        None
    } else {
        let structured = serde_json::json!({
            "cmd": cmd,
            "exitCode": draft.exit_code,
            "cwd": cwd,
            "paths": if paths.is_empty() { serde_json::Value::Null } else { serde_json::json!(paths) },
            "stderrTail": stderr_tail,
        });
        let serialized = structured.to_string();
        if serialized.len() > JOURNAL_STRUCTURED_MAX_BYTES {
            return Err(JournalValidationError::new(
                "journal_structured_too_large",
                format!(
                    "serialized structured sidecar exceeds the {JOURNAL_STRUCTURED_MAX_BYTES}-byte \
                     cap ({} bytes)",
                    serialized.len()
                ),
            ));
        }
        Some(serialized)
    };

    let mut classes = accumulator.classes;
    classes.sort_unstable();
    classes.dedup();
    let span_count = accumulator.span_count;
    let redaction_classes: Vec<String> = classes.iter().map(|class| (*class).to_owned()).collect();
    let redaction_applied = span_count > 0 || !redaction_classes.is_empty();
    let redaction_report_json = serde_json::json!({
        "classesApplied": redaction_classes,
        "spanCount": span_count,
    })
    .to_string();

    Ok(PreparedJournalEntry {
        kind,
        session_key,
        body,
        structured_json,
        redaction_report_json,
        instruction_risk: accumulator.instruction_risk,
        truncated,
        redaction_applied,
        redaction_span_count: span_count,
        redaction_classes,
        raw_body_bytes,
    })
}

fn parse_journal_line(line: &str) -> Result<JournalEntryDraft, JournalValidationError> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
        JournalValidationError::new(
            "journal_invalid_json",
            format!("invalid JSONL line: {error}"),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        JournalValidationError::new(
            "journal_invalid_json",
            "each JSONL line must be one entry object".to_owned(),
        )
    })?;

    let string_field = |key: &str| -> Result<Option<String>, JournalValidationError> {
        match object.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(JournalValidationError::new(
                "journal_invalid_json",
                format!("`{key}` must be a string"),
            )),
        }
    };

    let body = string_field("body")?.ok_or_else(|| {
        JournalValidationError::new(
            "journal_body_required",
            "JSONL entry is missing the required `body` string".to_owned(),
        )
    })?;
    let exit_code = match object.get("exitCode") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(number)) => Some(number.as_i64().ok_or_else(|| {
            JournalValidationError::new(
                "journal_invalid_json",
                "`exitCode` must be an integer".to_owned(),
            )
        })?),
        Some(_) => {
            return Err(JournalValidationError::new(
                "journal_invalid_json",
                "`exitCode` must be an integer".to_owned(),
            ));
        }
    };
    let paths = match object.get("paths") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    JournalValidationError::new(
                        "journal_invalid_json",
                        "`paths` must be an array of strings".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(JournalValidationError::new(
                "journal_invalid_json",
                "`paths` must be an array of strings".to_owned(),
            ));
        }
    };

    Ok(JournalEntryDraft {
        body,
        kind: string_field("kind")?,
        session_key: string_field("sessionKey")?,
        cmd: string_field("cmd")?,
        exit_code,
        cwd: string_field("cwd")?,
        paths,
        stderr_tail: string_field("stderrTail")?,
    })
}

fn persist_prepared_entry(
    connection: &DbConnection,
    workspace_id: &str,
    options: &JournalAppendOptions<'_>,
    prepared: &PreparedJournalEntry,
) -> Result<StoredJournalEntry, DomainError> {
    let input = CreateJournalEntryInput {
        entry_id: generate_journal_entry_id(),
        workspace_id: workspace_id.to_owned(),
        agent_name: options.agent_name.clone(),
        session_key: prepared.session_key.clone(),
        kind: prepared.kind.as_str().to_owned(),
        source: options.source.as_str().to_owned(),
        body: prepared.body.clone(),
        structured: prepared.structured_json.clone(),
        redaction_report: prepared.redaction_report_json.clone(),
        instruction_risk: prepared.instruction_risk.as_str().to_owned(),
    };
    connection
        .insert_journal_entry(&input)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to store journal entry: {error}"),
            repair: Some("ee doctor".to_owned()),
        })
}

fn effective_database_path(workspace_path: &Path, database_path: Option<&Path>) -> PathBuf {
    database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"))
}

fn resolve_workspace_path(path: &Path) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    absolute
        .canonicalize()
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        })
}

fn open_journal_database(database_path: &Path) -> Result<DbConnection, DomainError> {
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Database not found at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_owned()),
        });
    }
    let connection =
        DbConnection::open_file(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;
    connection
        .migrate()
        .map_err(|error| DomainError::MigrationRequired {
            message: format!("Failed to migrate journal database: {error}"),
            repair: Some("ee migrate run --workspace . --json".to_owned()),
        })?;
    Ok(connection)
}

fn ensure_workspace(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
) -> Result<(), DomainError> {
    let path = workspace_path.to_string_lossy().into_owned();
    if connection
        .get_workspace_by_path(&path)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
        .is_some()
    {
        return Ok(());
    }

    let input = CreateWorkspaceInput {
        path: path.clone(),
        name: workspace_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    };
    match connection.insert_workspace(workspace_id, &input) {
        Ok(()) => Ok(()),
        // Another writer can race the workspace insert; losing the race is
        // success as long as the row exists afterwards.
        Err(error) => {
            if connection
                .get_workspace_by_path(&path)
                .map_err(|query_error| DomainError::Storage {
                    message: format!("Failed to query raced workspace: {query_error}"),
                    repair: Some("ee doctor".to_owned()),
                })?
                .is_some()
            {
                Ok(())
            } else {
                Err(DomainError::Storage {
                    message: format!("Failed to register workspace: {error}"),
                    repair: Some("ee doctor".to_owned()),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{
        JOURNAL_BODY_MAX_BYTES, JOURNAL_DISABLED_CODE, JOURNAL_ENTRY_TRUNCATED_CODE,
        JOURNAL_REDACTION_APPLIED_CODE, JOURNAL_STDIN_MAX_LINES, JournalAppendOptions,
        JournalEntryDraft, JournalKind, JournalListOptions, JournalShowOptions, JournalSource,
        append_journal_entries_stdin, append_journal_entry, generate_journal_entry_id,
        journal_retention_days, list_journal_entries, show_journal_entry,
        truncate_at_char_boundary,
    };
    use crate::db::{DbConnection, JournalEntryListFilter};
    use crate::models::DomainError;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, context: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(format!("assertion failed: {context}"))
        }
    }

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

    /// Initialized workspace dir + migrated DB at `<ws>/.ee/ee.db`.
    fn seed_journal_workspace(
        prefix: &str,
    ) -> Result<(tempfile::TempDir, PathBuf, PathBuf), String> {
        let temp_root = std::env::temp_dir();
        let temp_root = if temp_root.exists() {
            temp_root
        } else {
            PathBuf::from("/tmp")
        };
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(temp_root)
            .map_err(|error| error.to_string())?;
        let workspace_path = dir
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let database_path = workspace_path.join(".ee").join("ee.db");
        fs::create_dir_all(workspace_path.join(".ee")).map_err(|error| error.to_string())?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
        Ok((dir, workspace_path, database_path))
    }

    fn append_options<'a>(
        workspace_path: &'a std::path::Path,
        agent_name: Option<&str>,
        source: JournalSource,
    ) -> JournalAppendOptions<'a> {
        JournalAppendOptions {
            workspace_path,
            database_path: None,
            agent_name: agent_name.map(str::to_owned),
            source,
        }
    }

    fn body_draft(body: &str) -> JournalEntryDraft {
        JournalEntryDraft {
            body: body.to_owned(),
            ..JournalEntryDraft::default()
        }
    }

    #[test]
    fn generate_journal_entry_id_has_prefix_and_is_unique() -> TestResult {
        let first = generate_journal_entry_id();
        let second = generate_journal_entry_id();
        ensure(first.starts_with("jrn_"), "id carries the jrn_ prefix")?;
        ensure(first != second, "ids are unique within a process")
    }

    #[test]
    fn redaction_runs_before_persistence_and_raw_secret_never_reaches_disk() -> TestResult {
        let secret = "sk-ant-api03-FAKEFAKE";
        let (_dir, workspace_path, database_path) = seed_journal_workspace("jrn-redact")?;
        let options = append_options(&workspace_path, None, JournalSource::Manual);
        let report = append_journal_entry(
            &options,
            &body_draft(&format!("deploy failed with API_KEY={secret} retry later")),
        )
        .map_err(|error| error.to_string())?;

        let entry = report.entry.as_ref().ok_or("entry must be present")?;
        ensure(
            !entry.body.contains(secret),
            "stored body must not contain the raw secret",
        )?;
        ensure(report.redaction_applied, "redaction flag is set")?;
        ensure(
            report
                .degraded
                .iter()
                .any(|d| d.code == JOURNAL_REDACTION_APPLIED_CODE),
            "journal_redaction_applied degraded entry is emitted",
        )?;
        let report_value = &entry.redaction_report;
        ensure(
            report_value
                .get("classesApplied")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|classes| !classes.is_empty()),
            "redaction report lists applied classes",
        )?;
        ensure(
            report_value
                .get("spanCount")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|count| count > 0),
            "redaction report counts spans",
        )?;

        // The raw secret must be absent from the database file bytes.
        let db_bytes = fs::read(&database_path).map_err(|error| error.to_string())?;
        let needle = secret.as_bytes();
        ensure(
            !db_bytes
                .windows(needle.len())
                .any(|window| window == needle),
            "raw secret bytes must never reach the DB file",
        )
    }

    #[test]
    fn stdin_lines_validate_and_persist_independently() -> TestResult {
        let (_dir, workspace_path, database_path) = seed_journal_workspace("jrn-batch")?;
        let options = append_options(&workspace_path, None, JournalSource::Stdin);
        let input = concat!(
            "{\"body\":\"line one observation\",\"kind\":\"observation\"}\n",
            "{\"kind\":\"note\"}\n",
            "{\"body\":\"line three failure\",\"cmd\":\"cargo test\",\"exitCode\":101}\n",
        );
        let report =
            append_journal_entries_stdin(&options, input).map_err(|error| error.to_string())?;

        ensure_equal(&report.line_count, &3, "line count")?;
        ensure_equal(&report.stored_count, &2, "stored count")?;
        ensure_equal(&report.failed_count, &1, "failed count")?;
        ensure(
            !report.all_failed(),
            "exit-0 path: at least one line landed",
        )?;
        ensure_equal(&report.results[0].status, &"stored", "line 1 stored")?;
        ensure_equal(&report.results[1].status, &"failed", "line 2 failed")?;
        ensure_equal(
            &report.results[1].error_code,
            &Some("journal_body_required"),
            "line 2 error code",
        )?;
        ensure_equal(&report.results[2].status, &"stored", "line 3 stored")?;

        // Line 3 had machine fields and no explicit kind -> command_failure.
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let workspace_id = crate::core::curate::stable_workspace_id(&workspace_path);
        let stored = connection
            .list_journal_entries(
                &workspace_id,
                &JournalEntryListFilter {
                    limit: 10,
                    ..JournalEntryListFilter::default()
                },
            )
            .map_err(|error| error.to_string())?;
        ensure_equal(&stored.len(), &2, "exactly the two valid lines persisted")?;
        ensure(
            stored.iter().any(|entry| entry.kind == "command_failure"),
            "cmd/exitCode line defaults to command_failure kind",
        )?;
        ensure(
            stored.iter().all(|entry| entry.source == "stdin"),
            "batch entries carry source=stdin",
        )
    }

    #[test]
    fn stdin_all_failed_reports_zero_stored() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-allfail")?;
        let options = append_options(&workspace_path, None, JournalSource::Stdin);
        let report = append_journal_entries_stdin(&options, "not json at all\n")
            .map_err(|error| error.to_string())?;
        ensure_equal(&report.stored_count, &0, "nothing stored")?;
        ensure(report.all_failed(), "all_failed drives the exit-5 path")?;
        ensure_equal(
            &report.results[0].error_code,
            &Some("journal_invalid_json"),
            "invalid JSON error code",
        )
    }

    #[test]
    fn empty_body_is_a_usage_error() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-empty")?;
        let options = append_options(&workspace_path, None, JournalSource::Manual);
        let error = append_journal_entry(&options, &body_draft("   "))
            .err()
            .ok_or("empty body must be rejected")?;
        ensure(
            matches!(error, DomainError::Usage { .. }),
            "empty body maps to a usage error",
        )
    }

    #[test]
    fn body_bounds_store_at_cap_and_truncate_one_byte_over() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-bounds")?;
        let options = append_options(&workspace_path, None, JournalSource::Manual);

        let at_cap = "a".repeat(JOURNAL_BODY_MAX_BYTES);
        let report = append_journal_entry(&options, &body_draft(&at_cap))
            .map_err(|error| error.to_string())?;
        let entry = report.entry.as_ref().ok_or("entry must be present")?;
        ensure_equal(
            &entry.body.len(),
            &JOURNAL_BODY_MAX_BYTES,
            "body exactly at the cap is stored unmodified",
        )?;
        ensure(!report.truncated, "at-cap body is not truncated")?;
        ensure(
            report.degraded.is_empty(),
            "at-cap body emits no degraded entries",
        )?;

        let over_cap = "a".repeat(JOURNAL_BODY_MAX_BYTES + 1);
        let report = append_journal_entry(&options, &body_draft(&over_cap))
            .map_err(|error| error.to_string())?;
        let entry = report.entry.as_ref().ok_or("entry must be present")?;
        ensure_equal(
            &entry.body.len(),
            &JOURNAL_BODY_MAX_BYTES,
            "one byte over truncates to the cap",
        )?;
        ensure(report.truncated, "over-cap body is truncated")?;
        ensure(
            report
                .degraded
                .iter()
                .any(|d| d.code == JOURNAL_ENTRY_TRUNCATED_CODE),
            "truncation emits journal_entry_truncated",
        )
    }

    #[test]
    fn stdin_rejects_more_than_the_line_cap() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-cap")?;
        let options = append_options(&workspace_path, None, JournalSource::Stdin);
        let input = "{\"body\":\"x\"}\n".repeat(JOURNAL_STDIN_MAX_LINES + 1);
        let error = append_journal_entries_stdin(&options, &input)
            .err()
            .ok_or("line cap must be a hard usage error")?;
        ensure(
            matches!(error, DomainError::Usage { .. }),
            "over-cap batch maps to a usage error",
        )
    }

    #[test]
    fn oversize_sidecar_fields_fail_with_per_field_codes() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-sidecar")?;
        let options = append_options(&workspace_path, None, JournalSource::Stdin);
        let big_cmd = "c".repeat(2 * 1024 + 1);
        let too_many_paths: Vec<String> = (0..17).map(|index| format!("\"p{index}\"")).collect();
        let fat_paths: Vec<String> = (0..9)
            .map(|index| format!("\"{}{}\"", index, "q".repeat(1000)))
            .collect();
        let input = format!(
            "{{\"body\":\"a\",\"cmd\":\"{big_cmd}\"}}\n{{\"body\":\"b\",\"paths\":[{}]}}\n{{\"body\":\"c\",\"paths\":[{}]}}\n",
            too_many_paths.join(","),
            fat_paths.join(","),
        );
        let report =
            append_journal_entries_stdin(&options, &input).map_err(|error| error.to_string())?;
        ensure_equal(
            &report.results[0].error_code,
            &Some("journal_cmd_too_large"),
            "cmd over 2 KiB",
        )?;
        ensure_equal(
            &report.results[1].error_code,
            &Some("journal_paths_too_many"),
            "more than 16 paths",
        )?;
        ensure_equal(
            &report.results[2].error_code,
            &Some("journal_structured_too_large"),
            "serialized sidecar over 8 KiB",
        )
    }

    /// The CLI fills `agent_name` from `EE_AGENT_NAME` via
    /// `crate::core::memory_scope::current_agent_name()`; per repo
    /// convention in-process tests never mutate the process environment,
    /// so attribution is asserted through the same options seam.
    #[test]
    fn agent_name_attribution_lands_on_the_stored_row() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-agent")?;
        let named = append_options(&workspace_path, Some("agent-zeta"), JournalSource::Hook);
        let report = append_journal_entry(&named, &body_draft("cargo check failed"))
            .map_err(|error| error.to_string())?;
        let entry = report.entry.as_ref().ok_or("entry must be present")?;
        ensure_equal(
            &entry.agent_name.as_deref(),
            &Some("agent-zeta"),
            "EE_AGENT_NAME attribution",
        )?;
        ensure_equal(&entry.source.as_str(), &"hook", "hook source is recorded")?;

        let anonymous = append_options(&workspace_path, None, JournalSource::Manual);
        let report = append_journal_entry(&anonymous, &body_draft("anonymous note"))
            .map_err(|error| error.to_string())?;
        let entry = report.entry.as_ref().ok_or("entry must be present")?;
        ensure_equal(
            &entry.agent_name,
            &None,
            "absent EE_AGENT_NAME stores NULL agent_name",
        )
    }

    #[test]
    fn oversize_multibyte_truncation_is_deterministic_and_char_aligned() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-multibyte")?;
        let options = append_options(&workspace_path, None, JournalSource::Manual);
        // 3-byte char: the cap (16384) is not a multiple of 3, so the cut
        // must back up to a char boundary below the cap.
        let body = "\u{20ac}".repeat(JOURNAL_BODY_MAX_BYTES / 3 + 8);
        let first = append_journal_entry(&options, &body_draft(&body))
            .map_err(|error| error.to_string())?;
        let second = append_journal_entry(&options, &body_draft(&body))
            .map_err(|error| error.to_string())?;
        let first_body = &first.entry.as_ref().ok_or("first entry")?.body;
        let second_body = &second.entry.as_ref().ok_or("second entry")?.body;
        ensure_equal(
            &first_body.as_bytes(),
            &second_body.as_bytes(),
            "same oversize input truncates byte-identically",
        )?;
        ensure(
            first_body.len() <= JOURNAL_BODY_MAX_BYTES,
            "stored body respects the cap",
        )?;
        ensure(
            first_body.len() % 3 == 0,
            "cut lands on a 3-byte char boundary",
        )?;
        ensure(first.truncated, "oversize body reports truncation")
    }

    #[test]
    fn truncate_at_char_boundary_is_a_no_op_under_the_cap() -> TestResult {
        ensure_equal(
            &truncate_at_char_boundary("abc", 16),
            &"abc",
            "under-cap input is unchanged",
        )
    }

    #[test]
    fn disabled_journal_refuses_cleanly_without_writing_rows() -> TestResult {
        let (_dir, workspace_path, database_path) = seed_journal_workspace("jrn-disabled")?;
        fs::write(
            workspace_path.join(".ee").join("config.toml"),
            "[journal]\nenabled = false\n",
        )
        .map_err(|error| error.to_string())?;

        let options = append_options(&workspace_path, None, JournalSource::Manual);
        let report = append_journal_entry(&options, &body_draft("should not land"))
            .map_err(|error| error.to_string())?;
        ensure_equal(
            &report.status,
            &JOURNAL_DISABLED_CODE,
            "append reports journal_disabled",
        )?;
        ensure(report.entry.is_none(), "no entry payload when disabled")?;
        ensure(
            report
                .degraded
                .iter()
                .any(|d| d.code == JOURNAL_DISABLED_CODE),
            "journal_disabled degraded entry is emitted",
        )?;

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let workspace_id = crate::core::curate::stable_workspace_id(&workspace_path);
        let stored = connection
            .list_journal_entries(
                &workspace_id,
                &JournalEntryListFilter {
                    limit: 10,
                    ..JournalEntryListFilter::default()
                },
            )
            .map_err(|error| error.to_string())?;
        ensure_equal(&stored.len(), &0, "no row was written while disabled")?;

        let list = list_journal_entries(&JournalListOptions {
            workspace_path: &workspace_path,
            database_path: None,
            session_key: None,
            agent_name: None,
            since: None,
            kind: None,
            undistilled_only: false,
            limit: 10,
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(
            &list.status,
            &JOURNAL_DISABLED_CODE,
            "list reports journal_disabled",
        )?;

        let batch = append_journal_entries_stdin(&options, "{\"body\":\"x\"}\n")
            .map_err(|error| error.to_string())?;
        ensure_equal(
            &batch.status,
            &JOURNAL_DISABLED_CODE,
            "batch reports journal_disabled",
        )?;
        ensure(
            !batch.all_failed(),
            "disabled batch must not take the exit-5 all-failed path",
        )?;

        ensure_equal(
            &journal_retention_days(&workspace_path),
            &14,
            "retention default survives a partial [journal] table",
        )
    }

    #[test]
    fn list_filters_and_show_round_trip() -> TestResult {
        let (_dir, workspace_path, _database_path) = seed_journal_workspace("jrn-list")?;
        let alpha = append_options(&workspace_path, Some("agent-alpha"), JournalSource::Manual);
        let beta = append_options(&workspace_path, Some("agent-beta"), JournalSource::Manual);
        let first = append_journal_entry(
            &alpha,
            &JournalEntryDraft {
                body: "alpha surprise".to_owned(),
                kind: Some(JournalKind::Surprise.as_str().to_owned()),
                session_key: Some("sess-1".to_owned()),
                ..JournalEntryDraft::default()
            },
        )
        .map_err(|error| error.to_string())?;
        append_journal_entry(&beta, &body_draft("beta note")).map_err(|error| error.to_string())?;

        let list = list_journal_entries(&JournalListOptions {
            workspace_path: &workspace_path,
            database_path: None,
            session_key: None,
            agent_name: Some("agent-alpha".to_owned()),
            since: None,
            kind: Some("surprise".to_owned()),
            undistilled_only: true,
            limit: 10,
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(&list.entries.len(), &1, "agent+kind filter narrows to one")?;
        ensure_equal(
            &list.entries[0].session_key.as_deref(),
            &Some("sess-1"),
            "filtered entry carries its session key",
        )?;

        let entry_id = first.entry.as_ref().ok_or("first entry")?.entry_id.clone();
        let shown = show_journal_entry(&JournalShowOptions {
            workspace_path: &workspace_path,
            database_path: None,
            entry_id: &entry_id,
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(
            &shown.entry.as_ref().map(|entry| entry.entry_id.clone()),
            &Some(entry_id),
            "show resolves the stored entry",
        )?;

        let missing = show_journal_entry(&JournalShowOptions {
            workspace_path: &workspace_path,
            database_path: None,
            entry_id: "jrn_does_not_exist",
        });
        ensure(
            matches!(missing, Err(DomainError::NotFound { .. })),
            "unknown entry id maps to NotFound",
        )
    }
}
