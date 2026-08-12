//! JSONL import execution (EE-222).
//!
//! The import path consumes EE JSONL export records, validates their schemas,
//! and imports memory records into the local workspace database. Non-memory
//! records are parsed for accounting but are not replayed as durable state in
//! this slice.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::db::{
    CreateAuditInput, CreateMemoryInput, CreateSearchIndexJobInput, CreateWorkspaceInput,
    DatabaseConfig, DbConnection, DbError, SearchIndexJobType, StoredMemory,
};
use crate::models::{
    EXPORT_AGENT_SCHEMA_V1, EXPORT_ARTIFACT_SCHEMA_V1, EXPORT_AUDIT_SCHEMA_V1,
    EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1, EXPORT_LINK_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1, EXPORT_WORKSPACE_SCHEMA_V1, ExportFooter,
    ExportHeader, ExportMemoryRecord, ExportTagRecord, IMPORT_JSONL_SCHEMA_V1, ImportSource,
    MemoryContent, MemoryId, MemoryKind, MemoryLevel, Tag, TrustClass, TrustLevel, UnitScore,
    WorkspaceId,
};
use crate::policy::import_auth::{
    ArtifactContext, EXPORT_ARTIFACT_FAMILY, EXPORT_RECORD_ENCODING_V1, ImportAuthOutcome,
    RecordsRootBuilder, STORE_KEY_NAMESPACE_V1, canonical_record_hash, verify_artifact,
};
use crate::policy::store_auth::{
    MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE, MacDomain, StoreAuthError, StoreAuthRoot,
    workspace_keys_dir,
};

/// Issue code emitted when a native-source artifact claims `human_explicit`
/// trust but its footer does not authenticate under this store's key
/// (ADR 0086 TC-D14). Closes the spoofable `import_source=native` bypass.
pub const UNAUTHENTICATED_NATIVE_IMPORT_TRUST_CODE: &str = "unauthenticated_native_import_trust";
/// JSONL artifacts cannot establish the signed active-member origin required
/// to mint `peer_human_attested`, even when their store-local MAC is valid.
pub const PEER_HUMAN_ATTESTED_IMPORT_PATH_REQUIRED_CODE: &str =
    "peer_human_attested_requires_team_import_path";

const DEFAULT_DB_FILE: &str = "ee.db";
pub(crate) const IMPORT_ACTION: &str = "memory.import.jsonl";

/// Hard cap on the byte length of an `ee import jsonl --source-path` file.
///
/// `import_jsonl_records` previously called `fs::read_to_string(source_path)`
/// directly, which has no upper bound: a multi-GB JSONL file (whether
/// authored maliciously, accumulated from a long-running export, or handed
/// off by another agent) would be slurped into a `String` in one allocation
/// before `parse_jsonl_source` ever ran. That allocation could OOM the
/// process under disk-pressure on the dev host or trip the swap thrashing
/// path described in feedback_hung_subprocess_paralyzes_agent — a benign
/// `ee import jsonl <path>` then becomes a local denial-of-service against
/// the agent that ran it.
///
/// 256 MiB is the same order-of-magnitude as other bulk-import surfaces
/// (e.g. the 4 MiB `.ee/config.toml` reads under `src/core/curate.rs`,
/// `src/config/path_resolver.rs`, etc., scaled up because a JSONL export
/// is bulk material rather than a single config record). At 1 KiB per
/// memory record this is ~262_000 records; at the 65_536-byte memory
/// content CHECK ceiling it is ~4_000 records — generous for ordinary
/// workspace bulk-import. Users with larger exports should stream them in
/// chunks rather than rely on a single mmap-style read.
pub const JSONL_IMPORT_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Options for one `ee import jsonl` run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlImportOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub source_path: PathBuf,
    pub dry_run: bool,
}

/// Stable issue severity for JSONL import diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonlImportIssueSeverity {
    Info,
    Error,
    Warning,
}

impl JsonlImportIssueSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// Validation or import diagnostic for one JSONL record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlImportIssue {
    pub line: Option<u32>,
    pub code: String,
    pub severity: JsonlImportIssueSeverity,
    pub message: String,
}

impl JsonlImportIssue {
    fn info(line: Option<u32>, code: &str, message: impl Into<String>) -> Self {
        Self {
            line,
            code: code.to_owned(),
            severity: JsonlImportIssueSeverity::Info,
            message: message.into(),
        }
    }

    fn error(line: Option<u32>, code: &str, message: impl Into<String>) -> Self {
        Self {
            line,
            code: code.to_owned(),
            severity: JsonlImportIssueSeverity::Error,
            message: message.into(),
        }
    }

    fn warning(line: Option<u32>, code: &str, message: impl Into<String>) -> Self {
        Self {
            line,
            code: code.to_owned(),
            severity: JsonlImportIssueSeverity::Warning,
            message: message.into(),
        }
    }
}

/// Error returned by the narrow JSONL header parser used by import validation
/// and fuzzing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JsonlHeaderParseError {
    EmptyLine,
    InvalidJson { message: String },
    MissingSchema,
    WrongSchema { schema: String },
    InvalidHeader { message: String },
}

impl fmt::Display for JsonlHeaderParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLine => formatter.write_str("JSONL header line is empty"),
            Self::InvalidJson { message } => {
                write!(formatter, "invalid JSONL header JSON: {message}")
            }
            Self::MissingSchema => {
                formatter.write_str("JSONL header is missing a non-empty schema field")
            }
            Self::WrongSchema { schema } => write!(
                formatter,
                "JSONL header schema must be {EXPORT_HEADER_SCHEMA_V1}, got {schema}"
            ),
            Self::InvalidHeader { message } => write!(formatter, "invalid JSONL header: {message}"),
        }
    }
}

/// Parse one JSONL header line.
///
/// This is intentionally smaller than [`import_jsonl_records`]: fuzzing should
/// exercise the record parser directly without opening files or databases.
pub fn parse_jsonl_header(input: &str) -> Result<ExportHeader, JsonlHeaderParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(JsonlHeaderParseError::EmptyLine);
    }

    let value = serde_json::from_str::<JsonValue>(trimmed).map_err(|error| {
        JsonlHeaderParseError::InvalidJson {
            message: error.to_string(),
        }
    })?;
    let schema = value
        .get("schema")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|schema| !schema.is_empty())
        .ok_or(JsonlHeaderParseError::MissingSchema)?;

    if schema != EXPORT_HEADER_SCHEMA_V1 {
        return Err(JsonlHeaderParseError::WrongSchema {
            schema: schema.to_owned(),
        });
    }

    let header = serde_json::from_value::<ExportHeader>(value).map_err(|error| {
        JsonlHeaderParseError::InvalidHeader {
            message: error.to_string(),
        }
    })?;
    validate_export_header_required_fields(&header)
        .map_err(|message| JsonlHeaderParseError::InvalidHeader { message })?;
    Ok(header)
}

/// Summary returned by `ee import jsonl`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlImportReport {
    pub schema: &'static str,
    pub workspace_path: String,
    pub database_path: Option<String>,
    pub source_path: String,
    pub source_id: String,
    pub dry_run: bool,
    pub status: String,
    pub header: Option<JsonlImportHeaderSummary>,
    pub footer: Option<JsonlImportFooterSummary>,
    pub records_total: u32,
    pub memory_records: u32,
    pub tag_records: u32,
    pub ignored_records: u32,
    pub memories_imported: u32,
    pub memories_skipped_duplicate: u32,
    pub tags_imported: u32,
    pub imported_memory_ids: Vec<String>,
    pub issues: Vec<JsonlImportIssue>,
}

impl JsonlImportReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "import jsonl",
            "workspacePath": self.workspace_path,
            "databasePath": self.database_path,
            "sourcePath": redact_jsonl_import_source_ref(&self.source_path),
            "sourceId": redact_jsonl_import_source_ref(&self.source_id),
            "dryRun": self.dry_run,
            "status": self.status,
            "header": self.header.as_ref().map(JsonlImportHeaderSummary::data_json),
            "footer": self.footer.as_ref().map(JsonlImportFooterSummary::data_json),
            "recordsTotal": self.records_total,
            "memoryRecords": self.memory_records,
            "tagRecords": self.tag_records,
            "ignoredRecords": self.ignored_records,
            "memoriesImported": self.memories_imported,
            "memoriesSkippedDuplicate": self.memories_skipped_duplicate,
            "tagsImported": self.tags_imported,
            "importedMemoryIds": self.imported_memory_ids,
            "issues": self.issues.iter().map(|issue| {
                json!({
                    "line": issue.line,
                    "code": issue.code,
                    "severity": issue.severity.as_str(),
                    "message": issue.message,
                })
            }).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{mode}JSONL import {status}: {imported} imported, {skipped} duplicates, {issues} issue(s) from {memories} memory record(s)\n",
            status = self.status,
            imported = self.memories_imported,
            skipped = self.memories_skipped_duplicate,
            issues = self.issues.len(),
            memories = self.memory_records,
        )
    }
}

fn redact_jsonl_import_source_ref(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_jsonl_import_source_path_segments(&secret_redacted)
}

fn redact_jsonl_import_source_path_segments(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some((relative_index, _)) = value[cursor..]
            .char_indices()
            .find(|(_, c)| jsonl_import_source_path_separator(*c))
        else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_index;
        let Some(redaction_start) = jsonl_import_source_path_redaction_start(value, start) else {
            output.push_str(&value[cursor..=start]);
            cursor = start + 1;
            continue;
        };

        output.push_str(&value[cursor..redaction_start]);
        output.push_str("[REDACTED_PATH]");
        cursor = value[redaction_start..]
            .char_indices()
            .find_map(|(index, c)| {
                jsonl_import_source_path_boundary(c).then_some(redaction_start + index)
            })
            .unwrap_or(value.len());
    }
    output
}

fn jsonl_import_source_path_separator(c: char) -> bool {
    matches!(c, '/' | '\\')
}

fn jsonl_import_source_path_redaction_start(value: &str, separator_start: usize) -> Option<usize> {
    let candidate = &value[separator_start..];
    if jsonl_import_source_path_starts_sensitive_unix_segment(candidate)
        || jsonl_import_source_path_starts_sensitive_windows_segment(candidate)
        || jsonl_import_source_path_starts_unc_path(candidate)
    {
        return Some(
            jsonl_import_source_path_windows_drive_start(value, separator_start)
                .unwrap_or(separator_start),
        );
    }
    None
}

fn jsonl_import_source_path_starts_sensitive_unix_segment(value: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/Users/",
        "/Volumes/",
        "/private/",
        "/var/",
        "/tmp/",
        "/home/",
        "/data/",
        "/dp/",
        "/workspace/",
        "/repo/",
        "/etc/",
    ];

    PREFIXES.iter().any(|prefix| value.starts_with(prefix))
}

fn jsonl_import_source_path_starts_sensitive_windows_segment(value: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "\\Users\\",
        "\\Volumes\\",
        "\\private\\",
        "\\var\\",
        "\\tmp\\",
        "\\home\\",
        "\\data\\",
        "\\dp\\",
        "\\workspace\\",
        "\\repo\\",
        "\\etc\\",
    ];

    PREFIXES.iter().any(|prefix| value.starts_with(prefix))
}

fn jsonl_import_source_path_starts_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

fn jsonl_import_source_path_windows_drive_start(
    value: &str,
    separator_start: usize,
) -> Option<usize> {
    if separator_start < 2 {
        return None;
    }
    let bytes = value.as_bytes();
    let drive_start = separator_start - 2;
    if !bytes[drive_start].is_ascii_alphabetic() || bytes[drive_start + 1] != b':' {
        return None;
    }
    if drive_start == 0 {
        return Some(drive_start);
    }
    let previous = value[..drive_start].chars().next_back()?;
    jsonl_import_source_path_start_boundary(previous).then_some(drive_start)
}

fn jsonl_import_source_path_start_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '/' | '\\' | '(' | '[' | '{' | '"' | '\'' | '<' | '=')
}

fn jsonl_import_source_path_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '?' | '#' | '"' | '\'' | ')' | ']' | '}' | ',' | ';' | '<' | '>' | '`'
        )
}

/// Stable subset of header metadata exposed by import reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlImportHeaderSummary {
    pub export_id: String,
    pub format_version: u32,
    pub export_scope: String,
    pub redaction_level: String,
    pub import_source: String,
    pub trust_level: String,
    pub source_schema_version: Option<String>,
    pub checksum_status: String,
}

impl JsonlImportHeaderSummary {
    fn from_header(header: &ExportHeader) -> Self {
        Self {
            export_id: header.export_id.clone(),
            format_version: header.format_version,
            export_scope: header.export_scope.as_str().to_owned(),
            redaction_level: header.redaction_level.as_str().to_owned(),
            import_source: header.import_source.as_str().to_owned(),
            trust_level: header.trust_level.as_str().to_owned(),
            source_schema_version: header.source_schema_version.clone(),
            checksum_status: if header.checksum.is_some() {
                "present_unverified".to_owned()
            } else {
                "absent".to_owned()
            },
        }
    }

    fn data_json(&self) -> JsonValue {
        json!({
            "exportId": self.export_id,
            "formatVersion": self.format_version,
            "exportScope": self.export_scope,
            "redactionLevel": self.redaction_level,
            "importSource": self.import_source,
            "trustLevel": self.trust_level,
            "sourceSchemaVersion": self.source_schema_version,
            "checksumStatus": self.checksum_status,
        })
    }
}

/// Stable subset of footer metadata exposed by import reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlImportFooterSummary {
    pub export_id: String,
    pub total_records: u64,
    pub memory_count: u64,
    pub artifact_count: u64,
    pub tag_count: u64,
    pub success: bool,
}

impl JsonlImportFooterSummary {
    fn from_footer(footer: &ExportFooter) -> Self {
        Self {
            export_id: footer.export_id.clone(),
            total_records: footer.total_records,
            memory_count: footer.memory_count,
            artifact_count: footer.artifact_count,
            tag_count: footer.tag_count,
            success: footer.success,
        }
    }

    fn data_json(&self) -> JsonValue {
        json!({
            "exportId": self.export_id,
            "totalRecords": self.total_records,
            "memoryCount": self.memory_count,
            "artifactCount": self.artifact_count,
            "tagCount": self.tag_count,
            "success": self.success,
        })
    }
}

/// Error produced by JSONL import setup.
#[derive(Debug)]
pub enum JsonlImportError {
    Io { path: PathBuf, message: String },
    Storage(DbError),
}

impl JsonlImportError {
    #[must_use]
    pub const fn repair_hint(&self) -> Option<&'static str> {
        match self {
            Self::Io { .. } => Some("check the JSONL source path and workspace permissions"),
            Self::Storage(_) => {
                Some("ee init --workspace . && ee migrate run --workspace . --json")
            }
        }
    }
}

impl fmt::Display for JsonlImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(formatter, "I/O error at {}: {message}", path.display())
            }
            Self::Storage(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for JsonlImportError {}

impl From<DbError> for JsonlImportError {
    fn from(error: DbError) -> Self {
        Self::Storage(error)
    }
}

struct ParsedJsonlImport {
    header: Option<ExportHeader>,
    footer: Option<ExportFooter>,
    footer_line: Option<u32>,
    memories: Vec<ExportMemoryRecord>,
    tags_by_memory: BTreeMap<String, BTreeSet<String>>,
    tag_lines_by_memory: BTreeMap<String, u32>,
    artifact_records: u32,
    tag_records: u32,
    issues: Vec<JsonlImportIssue>,
    records_total: u32,
    ignored_records: u32,
    /// Ordered digest over the raw memory line bytes as read, matching what
    /// the exporter MAC'd (ADR 0086 TC-D14). Verified against the footer
    /// authentication block before native trust is honored.
    records_root: RecordsRootBuilder,
}

impl ParsedJsonlImport {
    fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == JsonlImportIssueSeverity::Error)
    }
}

struct PreparedMemory {
    id: String,
    input: CreateMemoryInput,
    tombstoned_at: Option<String>,
    tombstoned_reason: Option<String>,
    bayes_posterior: Option<(f64, f64)>,
    /// bd-multiplicity-aware-trust-p0u7g: attempt-family block restored into
    /// the pointer columns and the family ledger after the memory row lands.
    attempt_family: Option<crate::models::ExportAttemptFamilyRecord>,
    details: String,
    tag_count: u32,
}

/// Run one JSONL import operation.
///
/// # Errors
///
/// Returns [`JsonlImportError`] for filesystem setup failures or storage errors.
pub fn import_jsonl_records(
    options: &JsonlImportOptions,
) -> Result<JsonlImportReport, JsonlImportError> {
    let workspace_path = normalize_path(&options.workspace_path);
    ensure_import_source_path_is_regular_file(&options.source_path)?;
    let source_path = normalize_path(&options.source_path);
    let source_id = source_id(&source_path);
    let input = read_jsonl_source_bounded(&source_path)?;

    let parsed = parse_jsonl_source(&input);
    let mut report = report_from_parsed(
        &workspace_path,
        &source_path,
        &source_id,
        options.dry_run,
        &parsed,
    );

    if options.dry_run || parsed.has_errors() {
        return Ok(report);
    }

    let database_path = database_path(options);
    ensure_database_parent(&database_path)?;
    let connection = DbConnection::open(DatabaseConfig::file(database_path.clone()))?;
    connection.migrate()?;
    let workspace_id = ensure_workspace(&connection, &workspace_path)?;

    let native_auth = native_import_auth_state(&parsed, &workspace_path, &workspace_id);
    let prepared = prepare_memories(&parsed, &workspace_id, &native_auth);
    if prepared.has_errors() {
        report.issues.extend(prepared.issues);
        report.status = "rejected".to_owned();
        report.database_path = Some(database_path.to_string_lossy().into_owned());
        return Ok(report);
    }

    // Reimport is an idempotent restore: missing rows import, byte-identical
    // rows no-op, and divergent or tombstone-conflicting rows are preserved
    // untouched with an explicit conflict signal — never overwritten or
    // resurrected (ADR 0086 TC-D14).
    let mut to_insert = Vec::new();
    let mut skipped_duplicate = 0_u32;
    for memory in prepared.memories {
        match connection.get_memory(&memory.id)? {
            Some(existing) => {
                skipped_duplicate = skipped_duplicate.saturating_add(1);
                if let Some(issue) = reimport_conflict_issue(&existing, &memory) {
                    report.issues.push(issue);
                }
            }
            None => to_insert.push(memory),
        }
    }

    connection.with_transaction(|| {
        for memory in &to_insert {
            connection.insert_memory(&memory.id, &memory.input)?;
            if let Some((alpha, beta)) = memory.bayes_posterior {
                connection.update_memory_bayes_posterior(&memory.id, alpha, beta)?;
            }
            // bd-multiplicity-aware-trust-p0u7g: rebuild the family pointer
            // and the attempt-family ledger from the exported block; the
            // ledger keys to the restored row's own logical identity, and the
            // exported origin is preserved for legacy_v094 forensics.
            if let Some(family) = &memory.attempt_family {
                connection.set_memory_attempt_family(
                    &memory.id,
                    &crate::db::MemoryAttemptFamily {
                        family_id: family.family_id.clone(),
                        declared_size: family.declared_size,
                        attempt_index: family.attempt_index,
                        disposition: family.disposition.clone(),
                    },
                )?;
                if let Some(origin) = family.origin.as_deref() {
                    connection.set_attempt_family_origin(
                        &memory.input.workspace_id,
                        &family.family_id,
                        origin,
                    )?;
                }
            }
            if let Some(tombstoned_at) = memory.tombstoned_at.as_deref() {
                connection.restore_imported_memory_tombstone(&memory.id, tombstoned_at)?;
                connection.insert_audit(
                    &crate::db::generate_audit_id(),
                    &CreateAuditInput {
                        workspace_id: Some(memory.input.workspace_id.clone()),
                        actor: Some("ee import jsonl".to_owned()),
                        action: crate::db::audit_actions::MEMORY_TOMBSTONE.to_owned(),
                        target_type: Some("memory".to_owned()),
                        target_id: Some(memory.id.clone()),
                        details: Some(
                            json!({
                                "tombstoned_at": tombstoned_at,
                                "reason": memory.tombstoned_reason.as_deref(),
                                "source": "jsonl_import",
                            })
                            .to_string(),
                        ),
                    },
                )?;
            }
            connection.insert_audit(
                &crate::db::generate_audit_id(),
                &CreateAuditInput {
                    workspace_id: Some(memory.input.workspace_id.clone()),
                    actor: Some("ee import jsonl".to_owned()),
                    action: IMPORT_ACTION.to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(memory.id.clone()),
                    details: Some(memory.details.clone()),
                },
            )?;
            // bd-index-auto-freshness-m5kwf: an import is a real write path.
            // Publish the same durable single-document index work remember
            // publishes, so the post-commit drain converges the derived index
            // instead of stranding generations behind a manual rebuild.
            connection.insert_search_index_job(
                &import_search_index_job_id(&memory.id),
                &CreateSearchIndexJobInput {
                    workspace_id: memory.input.workspace_id.clone(),
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(memory.id.clone()),
                    documents_total: 1,
                },
            )?;
        }
        Ok(())
    })?;

    report.database_path = Some(database_path.to_string_lossy().into_owned());
    report.status = "completed".to_owned();
    report.memories_imported = saturating_len(to_insert.len());
    report.memories_skipped_duplicate = skipped_duplicate;
    report.tags_imported = to_insert.iter().fold(0_u32, |total, memory| {
        total.saturating_add(memory.tag_count)
    });
    report.imported_memory_ids = to_insert.into_iter().map(|memory| memory.id).collect();
    if !report.imported_memory_ids.is_empty() {
        // The rows above are durable; converge the derived index the same way
        // remember and batch remember do. A drain failure downgrades to a
        // truthful non-fatal issue while the durable jobs stay pending and
        // retryable by the next writer (bd-index-auto-freshness-m5kwf).
        let index_dir = workspace_path
            .join(".ee")
            .join(crate::core::index::DEFAULT_INDEX_SUBDIR);
        let publication = crate::core::index::process_pending_index_jobs_coalesced(
            &connection,
            &workspace_id,
            &index_dir,
            None,
        );
        let failure = match publication {
            Ok(job_reports) => {
                let non_completed = job_reports
                    .iter()
                    .find(|job_report| job_report.outcome != "completed")
                    .map(|job_report| {
                        job_report.error.clone().unwrap_or_else(|| {
                            format!("index job outcome was {}", job_report.outcome)
                        })
                    });
                if let Some(failure) = non_completed {
                    Some(failure)
                } else {
                    match crate::core::index::get_index_status(
                        &crate::core::index::IndexStatusOptions {
                            workspace_path: workspace_path.clone(),
                            database_path: Some(database_path.clone()),
                            index_dir: None,
                        },
                    ) {
                        Ok(status)
                            if status.health == crate::core::index::IndexHealth::Ready
                                && status.db_generation.is_some()
                                && status.db_generation == status.index_generation =>
                        {
                            None
                        }
                        Ok(status) => Some(format!(
                            "post-drain index status was {:?} (database generation {:?}, index generation {:?})",
                            status.health, status.db_generation, status.index_generation
                        )),
                        Err(error) => Some(format!(
                            "post-drain index status could not be verified: {error}"
                        )),
                    }
                }
            }
            Err(error) => Some(error.to_string()),
        };
        if let Some(failure) = failure {
            let repair = jsonl_import_index_repair_command(
                &workspace_path,
                options.database_path.as_deref(),
            );
            report.issues.push(JsonlImportIssue {
                line: None,
                code: "import_index_publish_failed".to_owned(),
                severity: JsonlImportIssueSeverity::Warning,
                message: format!(
                    "Imported memories are durable, but automatic publication of durable search-index jobs did not complete: {failure}. Search may omit imported memories until the durable jobs are retried. Run `{repair}`."
                ),
            });
        }
    }
    Ok(report)
}

/// Deterministic import-lane index-job id. Namespaced so a reimport replays
/// the same durable job and can never collide with the remember-lane job id
/// minted for the same memory.
fn import_search_index_job_id(memory_id: &str) -> String {
    let hash = blake3::hash(format!("jsonl_import|{memory_id}").as_bytes())
        .to_hex()
        .to_string();
    format!("sidx_{}", &hash[..26])
}

fn jsonl_import_index_repair_command(
    workspace_path: &Path,
    database_path: Option<&Path>,
) -> String {
    let workspace = jsonl_import_shell_quote_arg(workspace_path.to_string_lossy().as_ref());
    match database_path {
        Some(database_path) => {
            let database = jsonl_import_shell_quote_arg(database_path.to_string_lossy().as_ref());
            format!("ee index rebuild --workspace {workspace} --database {database}")
        }
        None => format!("ee index rebuild --workspace {workspace}"),
    }
}

fn jsonl_import_shell_quote_arg(value: &str) -> String {
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

fn reimport_conflict_issue(
    existing: &StoredMemory,
    incoming: &PreparedMemory,
) -> Option<JsonlImportIssue> {
    let mut divergences = Vec::new();
    if existing.content != incoming.input.content {
        divergences.push("content");
    }
    if existing.level != incoming.input.level {
        divergences.push("level");
    }
    if existing.kind != incoming.input.kind {
        divergences.push("kind");
    }
    if existing.trust_class != incoming.input.trust_class {
        divergences.push("trust_class");
    }
    match (
        existing.tombstoned_at.as_deref(),
        incoming.tombstoned_at.as_deref(),
    ) {
        (Some(_), None) => {
            divergences
                .push("tombstone (existing row is tombstoned; a plain import would resurrect it)");
        }
        (None, Some(_)) => {
            divergences.push("tombstone (import carries a tombstone; the existing row is live)");
        }
        _ => {}
    }
    if divergences.is_empty() {
        return None;
    }
    Some(JsonlImportIssue::warning(
        None,
        "reimport_divergent_existing_row",
        format!(
            "memory `{}` already exists and diverges on {}; the existing row is preserved (reimport never overwrites or resurrects)",
            incoming.id,
            divergences.join(", ")
        ),
    ))
}

fn report_from_parsed(
    workspace_path: &Path,
    source_path: &Path,
    source_id: &str,
    dry_run: bool,
    parsed: &ParsedJsonlImport,
) -> JsonlImportReport {
    let status = if parsed.has_errors() {
        "rejected"
    } else if dry_run {
        "dry_run"
    } else {
        "validated"
    };
    JsonlImportReport {
        schema: IMPORT_JSONL_SCHEMA_V1,
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        database_path: None,
        source_path: source_path.to_string_lossy().into_owned(),
        source_id: source_id.to_owned(),
        dry_run,
        status: status.to_owned(),
        header: parsed
            .header
            .as_ref()
            .map(JsonlImportHeaderSummary::from_header),
        footer: parsed
            .footer
            .as_ref()
            .map(JsonlImportFooterSummary::from_footer),
        records_total: parsed.records_total,
        memory_records: saturating_len(parsed.memories.len()),
        tag_records: parsed.tag_records,
        ignored_records: parsed.ignored_records,
        memories_imported: 0,
        memories_skipped_duplicate: 0,
        tags_imported: 0,
        imported_memory_ids: Vec::new(),
        issues: parsed.issues.clone(),
    }
}

fn parse_jsonl_source(input: &str) -> ParsedJsonlImport {
    let mut parsed = ParsedJsonlImport {
        header: None,
        footer: None,
        footer_line: None,
        memories: Vec::new(),
        tags_by_memory: BTreeMap::new(),
        tag_lines_by_memory: BTreeMap::new(),
        artifact_records: 0,
        tag_records: 0,
        issues: Vec::new(),
        records_total: 0,
        ignored_records: 0,
        records_root: RecordsRootBuilder::new(),
    };
    let mut first_schema: Option<(u32, String)> = None;
    let mut seen_memory_ids = BTreeSet::new();

    for (index, line) in input.lines().enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        parsed.records_total = parsed.records_total.saturating_add(1);

        let value = match serde_json::from_str::<JsonValue>(trimmed) {
            Ok(value) => value,
            Err(error) => {
                parsed.issues.push(JsonlImportIssue::error(
                    Some(line_number),
                    "invalid_json",
                    error.to_string(),
                ));
                continue;
            }
        };
        let Some(schema) = value
            .get("schema")
            .and_then(JsonValue::as_str)
            .filter(|schema| !schema.trim().is_empty())
        else {
            parsed.issues.push(JsonlImportIssue::error(
                Some(line_number),
                "missing_schema",
                "record is missing a non-empty schema field",
            ));
            continue;
        };

        if first_schema.is_none() {
            first_schema = Some((line_number, schema.to_owned()));
        }

        if parsed.footer.is_some() && schema != EXPORT_FOOTER_SCHEMA_V1 {
            parsed.issues.push(JsonlImportIssue::error(
                Some(line_number),
                "footer_not_last",
                "JSONL footer must be the final non-empty record",
            ));
            continue;
        }

        match schema {
            EXPORT_HEADER_SCHEMA_V1 => parse_header_record(&mut parsed, line_number, value),
            EXPORT_MEMORY_SCHEMA_V1 => {
                // Fold the raw trimmed line bytes at this ordinal — the exact
                // bytes the exporter hashed — so tampering, reordering, or
                // truncation diverges the recomputed root from the MAC'd one.
                if let Some(memory_id) = value.get("memory_id").and_then(JsonValue::as_str) {
                    parsed
                        .records_root
                        .push(memory_id, &canonical_record_hash(trimmed.as_bytes()));
                }
                parse_memory_record(&mut parsed, &mut seen_memory_ids, line_number, value);
            }
            EXPORT_TAG_SCHEMA_V1 => parse_tag_record(&mut parsed, line_number, value),
            EXPORT_FOOTER_SCHEMA_V1 => parse_footer_record(&mut parsed, line_number, value),
            EXPORT_ARTIFACT_SCHEMA_V1 => {
                parsed.artifact_records = parsed.artifact_records.saturating_add(1);
                parsed.ignored_records = parsed.ignored_records.saturating_add(1);
            }
            EXPORT_AGENT_SCHEMA_V1
            | EXPORT_AUDIT_SCHEMA_V1
            | EXPORT_LINK_SCHEMA_V1
            | EXPORT_WORKSPACE_SCHEMA_V1 => {
                parsed.ignored_records = parsed.ignored_records.saturating_add(1);
            }
            _ => parsed.issues.push(JsonlImportIssue::error(
                Some(line_number),
                "unsupported_schema",
                format!("unsupported JSONL record schema `{schema}`"),
            )),
        }
    }

    validate_header_and_footer(&mut parsed, first_schema);
    parsed
}

fn parse_header_record(parsed: &mut ParsedJsonlImport, line_number: u32, value: JsonValue) {
    if parsed.header.is_some() {
        parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "duplicate_header",
            "JSONL import accepts exactly one header record",
        ));
        return;
    }
    match serde_json::from_value::<ExportHeader>(value)
        .map_err(|error| error.to_string())
        .and_then(|header| {
            validate_export_header_required_fields(&header)?;
            Ok(header)
        }) {
        Ok(header) => parsed.header = Some(header),
        Err(error) => parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "invalid_header",
            error,
        )),
    }
}

fn validate_export_header_required_fields(header: &ExportHeader) -> Result<(), String> {
    for (field, value) in [
        ("schema", header.schema.as_str()),
        ("created_at", header.created_at.as_str()),
        ("ee_version", header.ee_version.as_str()),
        ("export_id", header.export_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!("header field `{field}` must not be blank"));
        }
    }
    if header.schema != EXPORT_HEADER_SCHEMA_V1 {
        return Err(format!(
            "header field `schema` must be {EXPORT_HEADER_SCHEMA_V1}"
        ));
    }
    Ok(())
}

fn parse_memory_record(
    parsed: &mut ParsedJsonlImport,
    seen_memory_ids: &mut BTreeSet<String>,
    line_number: u32,
    value: JsonValue,
) {
    match serde_json::from_value::<ExportMemoryRecord>(value) {
        Ok(memory) => {
            if !seen_memory_ids.insert(memory.memory_id.clone()) {
                parsed.issues.push(JsonlImportIssue::error(
                    Some(line_number),
                    "duplicate_memory_id",
                    format!("duplicate memory id `{}` in JSONL source", memory.memory_id),
                ));
            }
            if memory.redacted || memory.redaction_reason.is_some() {
                parsed.issues.push(JsonlImportIssue::info(
                    Some(line_number),
                    "redaction_round_trip_marker_preserved",
                    format!(
                        "redaction marker preserved for imported memory `{}`",
                        memory.memory_id
                    ),
                ));
            }
            parsed.memories.push(memory);
        }
        Err(error) => parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "invalid_memory",
            error.to_string(),
        )),
    }
}

fn parse_tag_record(parsed: &mut ParsedJsonlImport, line_number: u32, value: JsonValue) {
    match serde_json::from_value::<ExportTagRecord>(value) {
        Ok(tag) => {
            parsed.tag_records = parsed.tag_records.saturating_add(1);
            match Tag::parse(&tag.tag) {
                Ok(canonical) => {
                    parsed
                        .tag_lines_by_memory
                        .entry(tag.memory_id.clone())
                        .or_insert(line_number);
                    parsed
                        .tags_by_memory
                        .entry(tag.memory_id)
                        .or_default()
                        .insert(canonical.to_string());
                }
                Err(error) => parsed.issues.push(JsonlImportIssue::error(
                    Some(line_number),
                    "invalid_tag",
                    error.to_string(),
                )),
            }
        }
        Err(error) => parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "invalid_tag_record",
            error.to_string(),
        )),
    }
}

fn parse_footer_record(parsed: &mut ParsedJsonlImport, line_number: u32, value: JsonValue) {
    if parsed.footer.is_some() {
        parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "duplicate_footer",
            "JSONL import accepts at most one footer record",
        ));
        return;
    }
    match serde_json::from_value::<ExportFooter>(value)
        .map_err(|error| error.to_string())
        .and_then(|footer| {
            validate_export_footer_required_fields(&footer)?;
            Ok(footer)
        }) {
        Ok(footer) => {
            parsed.footer = Some(footer);
            parsed.footer_line = Some(line_number);
        }
        Err(error) => parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "invalid_footer",
            error.to_string(),
        )),
    }
}

fn validate_export_footer_required_fields(footer: &ExportFooter) -> Result<(), String> {
    for (field, value) in [
        ("schema", footer.schema.as_str()),
        ("export_id", footer.export_id.as_str()),
        ("completed_at", footer.completed_at.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!("footer field `{field}` must not be blank"));
        }
    }
    if footer.schema != EXPORT_FOOTER_SCHEMA_V1 {
        return Err(format!(
            "footer field `schema` must be {EXPORT_FOOTER_SCHEMA_V1}"
        ));
    }
    Ok(())
}

fn validate_header_and_footer(parsed: &mut ParsedJsonlImport, first_schema: Option<(u32, String)>) {
    match &parsed.header {
        Some(header) => {
            if header.format_version != crate::models::EXPORT_FORMAT_VERSION {
                parsed.issues.push(JsonlImportIssue::error(
                    None,
                    "unsupported_format_version",
                    format!(
                        "unsupported JSONL export format version {}",
                        header.format_version
                    ),
                ));
            }
        }
        None => parsed.issues.push(JsonlImportIssue::error(
            None,
            "missing_header",
            "JSONL import requires an ee.export.header.v1 header record",
        )),
    }

    if parsed.footer.is_none() {
        parsed.issues.push(JsonlImportIssue::error(
            None,
            "missing_footer",
            "JSONL import requires an ee.export.footer.v1 footer record",
        ));
    }

    if let Some((line, schema)) = first_schema {
        if schema != EXPORT_HEADER_SCHEMA_V1 {
            parsed.issues.push(JsonlImportIssue::error(
                Some(line),
                "header_not_first",
                "the first non-empty JSONL record must be ee.export.header.v1",
            ));
        }
    }

    let memory_ids = parsed
        .memories
        .iter()
        .map(|memory| memory.memory_id.as_str())
        .collect::<BTreeSet<_>>();
    for memory_id in parsed.tags_by_memory.keys() {
        if !memory_ids.contains(memory_id.as_str()) {
            parsed.issues.push(JsonlImportIssue::error(
                parsed.tag_lines_by_memory.get(memory_id).copied(),
                "orphaned_tag_record",
                format!("tag record references missing memory `{memory_id}`"),
            ));
        }
    }

    if let Some(footer) = &parsed.footer {
        if let Some(header) = &parsed.header
            && footer.export_id != header.export_id
        {
            parsed.issues.push(JsonlImportIssue::error(
                parsed.footer_line,
                "footer_export_id_mismatch",
                format!(
                    "footer export_id `{}` does not match header export_id `{}`",
                    footer.export_id, header.export_id
                ),
            ));
        }
        let parsed_artifact_count = u64::from(parsed.artifact_records);
        let parsed_tag_count = u64::from(parsed.tag_records);
        let parsed_record_count = u64::from(parsed.records_total);
        if footer.total_records != parsed_record_count {
            parsed.issues.push(JsonlImportIssue::warning(
                None,
                "footer_total_records_mismatch",
                format!(
                    "footer total_records {} does not match parsed JSONL records {}",
                    footer.total_records, parsed_record_count
                ),
            ));
        }
        if footer.artifact_count != parsed_artifact_count {
            parsed.issues.push(JsonlImportIssue::warning(
                None,
                "footer_artifact_count_mismatch",
                format!(
                    "footer artifact_count {} does not match parsed artifact records {}",
                    footer.artifact_count, parsed_artifact_count
                ),
            ));
        }
        if footer.tag_count != parsed_tag_count {
            parsed.issues.push(JsonlImportIssue::warning(
                None,
                "footer_tag_count_mismatch",
                format!(
                    "footer tag_count {} does not match parsed tag records {}",
                    footer.tag_count, parsed_tag_count
                ),
            ));
        }
        if !footer.success {
            parsed.issues.push(JsonlImportIssue::warning(
                None,
                "source_export_incomplete",
                "footer marks the source export as unsuccessful",
            ));
        }
        if footer.memory_count != parsed.memories.len() as u64 {
            parsed.issues.push(JsonlImportIssue::warning(
                None,
                "footer_memory_count_mismatch",
                format!(
                    "footer memory_count {} does not match parsed memory records {}",
                    footer.memory_count,
                    parsed.memories.len()
                ),
            ));
        }
    }
}

struct PreparedMemories {
    memories: Vec<PreparedMemory>,
    issues: Vec<JsonlImportIssue>,
}

impl PreparedMemories {
    fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == JsonlImportIssueSeverity::Error)
    }
}

fn prepare_memories(
    parsed: &ParsedJsonlImport,
    workspace_id: &str,
    native_auth: &NativeAuthState,
) -> PreparedMemories {
    let trust_class = trust_class_for_header(parsed.header.as_ref());
    let trust_subclass = trust_subclass_for_header(parsed.header.as_ref());
    let mut memories = Vec::with_capacity(parsed.memories.len());
    let mut issues = Vec::new();

    for memory in &parsed.memories {
        match prepare_memory(
            memory,
            workspace_id,
            trust_class,
            &trust_subclass,
            parsed,
            native_auth,
        ) {
            Ok(prepared) => memories.push(prepared),
            Err(issue) => issues.push(issue),
        }
    }

    PreparedMemories { memories, issues }
}

fn prepare_memory(
    memory: &ExportMemoryRecord,
    workspace_id: &str,
    trust_class: TrustClass,
    trust_subclass: &str,
    parsed: &ParsedJsonlImport,
    native_auth: &NativeAuthState,
) -> Result<PreparedMemory, JsonlImportIssue> {
    let import_memory_id = import_memory_id(memory, parsed)?;
    let import_source = parsed
        .header
        .as_ref()
        .map(|header| header.import_source)
        .unwrap_or(ImportSource::Unknown);
    let trust_class = trust_class_for_memory(memory, trust_class, import_source, native_auth)?;
    let trust_subclass = trust_subclass_for_memory(memory, trust_subclass);
    let level: MemoryLevel = memory.level.parse().map_err(|error| {
        JsonlImportIssue::error(
            None,
            "invalid_memory_level",
            format!("memory `{}` has invalid level: {error}", memory.memory_id),
        )
    })?;
    let kind: MemoryKind = memory.kind.parse().map_err(|error| {
        JsonlImportIssue::error(
            None,
            "invalid_memory_kind",
            format!("memory `{}` has invalid kind: {error}", memory.memory_id),
        )
    })?;
    let content = MemoryContent::parse(&memory.content).map_err(|error| {
        JsonlImportIssue::error(
            None,
            "invalid_memory_content",
            format!("memory `{}` has invalid content: {error}", memory.memory_id),
        )
    })?;
    let redaction_report = crate::policy::redact_secret_like_content(content.as_str());
    if redaction_report.redacted {
        return Err(JsonlImportIssue::error(
            None,
            "memory_contains_secret",
            format!(
                "memory `{}` contains secrets ({}); redact before import",
                memory.memory_id,
                redaction_report.redacted_reasons.join(", ")
            ),
        ));
    }
    let confidence = score_or_default(memory.confidence, trust_class.initial_confidence())
        .map_err(|message| {
            JsonlImportIssue::error(
                None,
                "invalid_memory_confidence",
                format!("memory `{}` {message}", memory.memory_id),
            )
        })?;
    let utility = score_or_default(memory.utility, 0.5).map_err(|message| {
        JsonlImportIssue::error(
            None,
            "invalid_memory_utility",
            format!("memory `{}` {message}", memory.memory_id),
        )
    })?;
    let importance = score_or_default(memory.importance, 0.5).map_err(|message| {
        JsonlImportIssue::error(
            None,
            "invalid_memory_importance",
            format!("memory `{}` {message}", memory.memory_id),
        )
    })?;
    let bayes_posterior = exported_bayes_posterior(memory)?;
    let tags = parsed
        .tags_by_memory
        .get(&memory.memory_id)
        .map(|tags| tags.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let tag_count = saturating_len(tags.len());

    Ok(PreparedMemory {
        id: import_memory_id,
        input: CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: level.as_str().to_owned(),
            kind: kind.as_str().to_owned(),
            content: content.as_str().to_owned(),
            workflow_id: None,
            confidence,
            utility,
            importance,
            provenance_uri: memory.provenance_uri.clone().or_else(|| {
                Some(format!(
                    "jsonl-import://{}",
                    memory.source_agent.as_deref().unwrap_or("unknown")
                ))
            }),
            trust_class: trust_class.as_str().to_owned(),
            trust_subclass,
            tags,
            valid_from: memory.valid_from.clone(),
            valid_to: memory
                .valid_to
                .clone()
                .or_else(|| memory.expires_at.clone()),
        },
        tombstoned_at: memory.tombstoned_at.clone(),
        tombstoned_reason: memory.tombstoned_reason.clone(),
        bayes_posterior,
        attempt_family: memory.attempt_family.clone(),
        details: json!({
            "schema": IMPORT_JSONL_SCHEMA_V1,
            "sourceMemoryId": memory.memory_id,
            "sourceWorkspaceId": memory.workspace_id,
            "sourceCreatedAt": memory.created_at,
            "sourceUpdatedAt": memory.updated_at,
            "sourceTombstonedAt": memory.tombstoned_at.as_deref(),
            "sourceTombstonedReason": memory.tombstoned_reason.as_deref(),
            "sourceValidFrom": memory.valid_from.as_deref(),
            "sourceValidTo": memory.valid_to.clone().or_else(|| memory.expires_at.clone()),
            "redacted": memory.redacted,
            "redactionReason": memory.redaction_reason,
            "sourceGraphFields": source_graph_fields_json(memory),
        })
        .to_string(),
        tag_count,
    })
}

fn exported_bayes_posterior(
    memory: &ExportMemoryRecord,
) -> Result<Option<(f64, f64)>, JsonlImportIssue> {
    match (memory.bayes_alpha, memory.bayes_beta) {
        (Some(alpha), Some(beta)) => {
            if !positive_finite(alpha) || !positive_finite(beta) {
                return Err(JsonlImportIssue::error(
                    None,
                    "invalid_memory_bayes_posterior",
                    format!(
                        "memory `{}` bayes_alpha and bayes_beta must be positive finite values",
                        memory.memory_id
                    ),
                ));
            }
            Ok(Some((alpha, beta)))
        }
        (None, None) => Ok(None),
        _ => Err(JsonlImportIssue::error(
            None,
            "invalid_memory_bayes_posterior",
            format!(
                "memory `{}` must include both bayes_alpha and bayes_beta when importing an exported posterior",
                memory.memory_id
            ),
        )),
    }
}

fn positive_finite(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn source_graph_fields_json(memory: &ExportMemoryRecord) -> JsonValue {
    let mut fields = serde_json::Map::new();
    insert_optional_json(&mut fields, "pagerank_score", memory.pagerank_score);
    insert_optional_json(&mut fields, "betweenness_score", memory.betweenness_score);
    insert_optional_json(&mut fields, "hits_authority", memory.hits_authority);
    insert_optional_json(&mut fields, "hits_hub", memory.hits_hub);
    insert_optional_json(&mut fields, "onion_layer", memory.onion_layer);
    insert_optional_json(&mut fields, "k_truss_max", memory.k_truss_max);
    insert_optional_json(&mut fields, "articulation_point", memory.articulation_point);
    insert_optional_json(&mut fields, "bayes_alpha", memory.bayes_alpha);
    insert_optional_json(&mut fields, "bayes_beta", memory.bayes_beta);
    JsonValue::Object(fields)
}

fn insert_optional_json<T>(
    fields: &mut serde_json::Map<String, JsonValue>,
    key: &str,
    value: Option<T>,
) where
    T: serde::Serialize,
{
    if let Some(value) = value
        && let Ok(json_value) = serde_json::to_value(value)
    {
        fields.insert(key.to_owned(), json_value);
    }
}

fn import_memory_id(
    memory: &ExportMemoryRecord,
    parsed: &ParsedJsonlImport,
) -> Result<String, JsonlImportIssue> {
    match memory.memory_id.parse::<MemoryId>() {
        Ok(_) => Ok(memory.memory_id.clone()),
        Err(_) if source_redacts_identifiers(parsed) => {
            Ok(stable_redacted_memory_id(memory).to_string())
        }
        Err(error) => Err(JsonlImportIssue::error(
            None,
            "invalid_memory_id",
            format!("memory id `{}` is invalid: {error}", memory.memory_id),
        )),
    }
}

fn source_redacts_identifiers(parsed: &ParsedJsonlImport) -> bool {
    parsed
        .header
        .as_ref()
        .is_some_and(|header| header.redaction_level.redacts_identifiers())
}

fn stable_redacted_memory_id(memory: &ExportMemoryRecord) -> MemoryId {
    MemoryId::from_uuid(stable_uuid(&format!(
        "jsonl-redacted-memory:{}:{}:{}:{}",
        memory.memory_id, memory.level, memory.kind, memory.created_at
    )))
}

fn score_or_default(value: Option<f64>, default: f32) -> Result<f32, String> {
    let score = match value {
        Some(score) => {
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err(format!(
                    "score is invalid: value {score} is not finite or outside 0.0..=1.0"
                ));
            }
            score as f32
        }
        None => default,
    };
    UnitScore::parse(score)
        .map(UnitScore::into_inner)
        .map_err(|error| format!("score is invalid: {error}"))
}

/// Whether the artifact authenticates under this store's key for native-trust
/// admission (ADR 0086 TC-D14). Computed once per import, then consulted for
/// every record-level `human_explicit` claim.
#[derive(Clone, Debug, Eq, PartialEq)]
enum NativeAuthState {
    /// The footer MAC verified against the local store key, the local
    /// workspace scope, and the records root recomputed from the received
    /// lines.
    Authenticated,
    /// The artifact carries no valid authentication for this store; the
    /// reason is a secret-free explanation for the refusal message.
    Unauthenticated { reason: String },
    /// The store-local authentication root itself is unavailable. Fail
    /// closed: native trust is refused with
    /// [`MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE`].
    StoreUnavailable { error: StoreAuthError },
}

fn native_import_auth_state(
    parsed: &ParsedJsonlImport,
    workspace_path: &Path,
    local_workspace_id: &str,
) -> NativeAuthState {
    let Some(authentication) = parsed
        .footer
        .as_ref()
        .and_then(|footer| footer.authentication.as_ref())
    else {
        return NativeAuthState::Unauthenticated {
            reason: "the artifact footer carries no store-local authentication block".to_owned(),
        };
    };
    let root = match StoreAuthRoot::open(workspace_keys_dir(workspace_path)) {
        Ok(root) => root,
        Err(error) => return NativeAuthState::StoreUnavailable { error },
    };
    let context = ArtifactContext {
        artifact_family: EXPORT_ARTIFACT_FAMILY,
        record_encoding_version: EXPORT_RECORD_ENCODING_V1,
        source_key_namespace: STORE_KEY_NAMESPACE_V1,
        workspace_scope: local_workspace_id,
    };
    match verify_artifact(
        &root,
        MacDomain::NativeImportRecordsRoot,
        &context,
        authentication,
        &parsed.records_root.finalize(),
        parsed.records_root.count(),
    ) {
        Ok(ImportAuthOutcome::Authenticated { .. }) => NativeAuthState::Authenticated,
        Ok(ImportAuthOutcome::RecordsMismatch) => NativeAuthState::Unauthenticated {
            reason: "the received records disagree with the MAC-authenticated records root/count \
                     (tampered, reordered, truncated, or padded)"
                .to_owned(),
        },
        Ok(ImportAuthOutcome::MacMismatch) => NativeAuthState::Unauthenticated {
            reason: "the footer MAC does not verify under this store's key and this workspace's \
                     binding context (foreign workspace, surface, or edited header)"
                .to_owned(),
        },
        Ok(ImportAuthOutcome::KeyOutsideWindow) => NativeAuthState::Unauthenticated {
            reason: "the footer names a key outside this store's verification window (foreign \
                     store or rotated-out key)"
                .to_owned(),
        },
        Ok(ImportAuthOutcome::SchemaMismatch) => NativeAuthState::Unauthenticated {
            reason: "the footer authentication block has an unsupported schema".to_owned(),
        },
        Ok(ImportAuthOutcome::Malformed) => NativeAuthState::Unauthenticated {
            reason: "the footer authentication block is malformed".to_owned(),
        },
        Err(error) => NativeAuthState::StoreUnavailable { error },
    }
}

fn trust_class_for_header(header: Option<&ExportHeader>) -> TrustClass {
    let Some(header) = header else {
        return TrustClass::LegacyImport;
    };
    match header.import_source {
        ImportSource::CassImport => TrustClass::CassEvidence,
        ImportSource::LegacyScan | ImportSource::ExternalImport | ImportSource::Unknown => {
            TrustClass::LegacyImport
        }
        ImportSource::Native => match header.trust_level {
            TrustLevel::Validated | TrustLevel::Verified => TrustClass::AgentValidated,
            TrustLevel::Untrusted | TrustLevel::Quarantined => TrustClass::AgentAssertion,
        },
    }
}

fn trust_class_for_memory(
    memory: &ExportMemoryRecord,
    fallback: TrustClass,
    import_source: ImportSource,
    native_auth: &NativeAuthState,
) -> Result<TrustClass, JsonlImportIssue> {
    let Some(raw) = memory.trust_class.as_deref() else {
        return Ok(fallback);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(JsonlImportIssue::error(
            None,
            "invalid_memory_trust_class",
            format!("memory `{}` has blank trust_class", memory.memory_id),
        ));
    }
    let trust_class = TrustClass::from_str(raw).map_err(|error| {
        JsonlImportIssue::error(
            None,
            "invalid_memory_trust_class",
            format!(
                "memory `{}` has invalid trust_class: {error}",
                memory.memory_id
            ),
        )
    })?;
    if trust_class == TrustClass::PeerHumanAttested {
        return Err(JsonlImportIssue::error(
            None,
            PEER_HUMAN_ATTESTED_IMPORT_PATH_REQUIRED_CODE,
            format!(
                "memory `{}` cannot import as peer_human_attested through JSONL; only the signed active-member admission path may assign that local class",
                memory.memory_id
            ),
        ));
    }
    if trust_class == TrustClass::HumanExplicit {
        if import_source.is_external() {
            return Err(JsonlImportIssue::error(
                None,
                "external_import_human_explicit_trust_class",
                format!(
                    "memory `{}` from {} cannot import as human_explicit; use agent_assertion or agent_validated for peer or external material",
                    memory.memory_id,
                    import_source.as_str()
                ),
            ));
        }
        // Native trust must be authenticated, not merely claimed: a spoofable
        // `import_source=native` header no longer admits human_explicit rows
        // (ADR 0086 TC-D14).
        match native_auth {
            NativeAuthState::Authenticated => {}
            NativeAuthState::Unauthenticated { reason } => {
                return Err(JsonlImportIssue::error(
                    None,
                    UNAUTHENTICATED_NATIVE_IMPORT_TRUST_CODE,
                    format!(
                        "memory `{}` claims human_explicit but the artifact does not authenticate under this store: {reason}. Re-export from this workspace (ee backup create / ee export) so the footer carries a valid store-local MAC, or import the rows at agent_validated or lower",
                        memory.memory_id
                    ),
                ));
            }
            NativeAuthState::StoreUnavailable { error } => {
                return Err(JsonlImportIssue::error(
                    None,
                    MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE,
                    format!(
                        "memory `{}` claims human_explicit but the store-local authentication root is unavailable: {} Repair: {}",
                        memory.memory_id,
                        error.message(),
                        error.repair()
                    ),
                ));
            }
        }
    }
    Ok(trust_class)
}

fn trust_subclass_for_memory(memory: &ExportMemoryRecord, fallback: &str) -> Option<String> {
    let record_subclass = memory
        .trust_subclass
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if record_subclass.is_some() {
        return record_subclass;
    }
    if memory
        .trust_class
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return None;
    }
    Some(fallback.to_owned())
}

fn trust_subclass_for_header(header: Option<&ExportHeader>) -> String {
    header.map_or_else(
        || "jsonl:missing-header".to_owned(),
        |header| {
            format!(
                "jsonl:{}:{}",
                header.import_source.as_str(),
                header.trust_level.as_str()
            )
        },
    )
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

fn database_path(options: &JsonlImportOptions) -> PathBuf {
    options.database_path.clone().unwrap_or_else(|| {
        options
            .workspace_path
            .join(crate::config::WORKSPACE_MARKER)
            .join(DEFAULT_DB_FILE)
    })
}

fn ensure_database_parent(path: &Path) -> Result<(), JsonlImportError> {
    ensure_import_database_path_is_safe_for_write(path)?;
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| JsonlImportError::Io {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;
    ensure_import_database_path_is_safe_for_write(path)
}

fn ensure_import_database_path_is_safe_for_write(path: &Path) -> Result<(), JsonlImportError> {
    if let Some(symlink_path) =
        super::path_safety::first_existing_symlink_component(path).map_err(|error| {
            JsonlImportError::Io {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        })?
    {
        return Err(JsonlImportError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to import JSONL records into database through symlinked path component `{}`",
                symlink_path.display()
            ),
        });
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(JsonlImportError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to import JSONL records into non-regular database path `{}`",
                path.display()
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(JsonlImportError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// Read a JSONL import source into a `String` with a hard byte cap.
///
/// Mirrors the bounded-read pattern that `src/cli/mod.rs::read_reflection_result_file`
/// uses for the much smaller `REFLECTION_RESULT_MAX_JSON_BYTES` surface:
/// `fs::metadata` rejects obviously oversized files up front, then the
/// actual read goes through `File::open` + `.take(MAX + 1)` so the
/// allocation is bounded even under TOCTOU growth between the metadata
/// stat and the open. The `+ 1` byte lets the post-read length check
/// distinguish "exactly at the limit" from "grew past the limit during
/// the read" and emit a clear oversize error in the latter case.
///
/// `read_to_string` is preserved (rather than `read_to_end` followed by
/// `String::from_utf8`) because the downstream `parse_jsonl_source`
/// requires UTF-8 input and would otherwise silently coerce or fail
/// later; rejecting invalid UTF-8 here keeps the error close to its
/// cause.
fn read_jsonl_source_bounded(source_path: &Path) -> Result<String, JsonlImportError> {
    use std::io::Read;

    let metadata = fs::metadata(source_path).map_err(|error| JsonlImportError::Io {
        path: source_path.to_path_buf(),
        message: error.to_string(),
    })?;
    if metadata.len() > JSONL_IMPORT_MAX_INPUT_BYTES {
        return Err(JsonlImportError::Io {
            path: source_path.to_path_buf(),
            message: format!(
                "JSONL source is too large: {} bytes exceeds the {} byte limit",
                metadata.len(),
                JSONL_IMPORT_MAX_INPUT_BYTES,
            ),
        });
    }
    let file = fs::File::open(source_path).map_err(|error| JsonlImportError::Io {
        path: source_path.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut input = String::new();
    let mut bounded = file.take(JSONL_IMPORT_MAX_INPUT_BYTES + 1);
    bounded
        .read_to_string(&mut input)
        .map_err(|error| JsonlImportError::Io {
            path: source_path.to_path_buf(),
            message: error.to_string(),
        })?;
    if input.len() as u64 > JSONL_IMPORT_MAX_INPUT_BYTES {
        return Err(JsonlImportError::Io {
            path: source_path.to_path_buf(),
            message: format!(
                "JSONL source is too large: read {} bytes exceeds the {} byte limit",
                input.len(),
                JSONL_IMPORT_MAX_INPUT_BYTES,
            ),
        });
    }
    Ok(input)
}

fn ensure_import_source_path_is_regular_file(path: &Path) -> Result<(), JsonlImportError> {
    if let Some(symlink_path) =
        super::path_safety::first_existing_symlink_component(path).map_err(|error| {
            JsonlImportError::Io {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        })?
    {
        return Err(JsonlImportError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to import JSONL source through symlinked path component `{}`",
                symlink_path.display()
            ),
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| JsonlImportError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    if !metadata.file_type().is_file() {
        return Err(JsonlImportError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to import JSONL source from non-regular path `{}`",
                path.display()
            ),
        });
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn source_id(source_path: &Path) -> String {
    format!("jsonl://{}", source_path.to_string_lossy())
}

fn stable_workspace_id(path: &str) -> String {
    WorkspaceId::from_uuid(stable_uuid(&format!("workspace:{path}"))).to_string()
}

fn stable_uuid(input: &str) -> Uuid {
    let hash = blake3::hash(input.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    Uuid::from_bytes(bytes)
}

fn saturating_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T>(actual: T, expected: T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn unauthenticated() -> NativeAuthState {
        NativeAuthState::Unauthenticated {
            reason: "test artifact without authentication".to_owned(),
        }
    }

    fn authenticated() -> NativeAuthState {
        NativeAuthState::Authenticated
    }

    fn sample_jsonl() -> String {
        [
            r#"{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-04-30T00:00:00Z","workspace_id":"wsp_01234567890123456789012345","workspace_path":"/source","export_scope":"memories","redaction_level":"none","record_count":3,"ee_version":"0.1.0","hostname":null,"export_id":"exp-001","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}"#,
            r#"{"schema":"ee.export.memory.v1","memory_id":"mem_01234567890123456789012345","workspace_id":"wsp_01234567890123456789012345","level":"procedural","kind":"rule","content":"Run cargo fmt --check before release.","importance":0.8,"confidence":0.9,"utility":0.7,"created_at":"2026-04-30T00:00:00Z","updated_at":null,"expires_at":null,"source_agent":"MistySalmon","provenance_uri":"ee-export://fixture","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}"#,
            r#"{"schema":"ee.export.tag.v1","memory_id":"mem_01234567890123456789012345","tag":"Release","created_at":"2026-04-30T00:00:00Z"}"#,
            r#"{"schema":"ee.export.footer.v1","export_id":"exp-001","completed_at":"2026-04-30T00:01:00Z","total_records":4,"memory_count":1,"link_count":0,"tag_count":1,"audit_count":0,"checksum":null,"success":true,"error_message":null}"#,
        ]
        .join("\n")
    }

    fn sample_jsonl_with_graph_fields() -> String {
        sample_jsonl().replace(
            r#""utility":0.7,"created_at""#,
            r#""utility":0.7,"pagerank_score":0.12,"betweenness_score":0.34,"hits_authority":0.56,"hits_hub":0.78,"onion_layer":3,"k_truss_max":4,"articulation_point":true,"bayes_alpha":2.5,"bayes_beta":1.5,"created_at""#,
        )
    }

    fn import_report_fixture(source_path: &str, source_id: &str) -> JsonlImportReport {
        JsonlImportReport {
            schema: IMPORT_JSONL_SCHEMA_V1,
            workspace_path: "/workspace/project".to_owned(),
            database_path: None,
            source_path: source_path.to_owned(),
            source_id: source_id.to_owned(),
            dry_run: true,
            status: "dry_run".to_owned(),
            header: None,
            footer: None,
            records_total: 0,
            memory_records: 0,
            tag_records: 0,
            ignored_records: 0,
            memories_imported: 0,
            memories_skipped_duplicate: 0,
            tags_imported: 0,
            imported_memory_ids: Vec::new(),
            issues: Vec::new(),
        }
    }

    #[test]
    fn parse_jsonl_header_accepts_header_record_only() -> TestResult {
        let header_line = sample_jsonl()
            .lines()
            .next()
            .ok_or_else(|| "sample JSONL must include a header line".to_string())?
            .to_string();
        let header = parse_jsonl_header(&header_line).map_err(|error| error.to_string())?;

        ensure(header.export_id, "exp-001".to_string(), "export id")?;
        ensure(
            parse_jsonl_header(r#"{"schema":"ee.export.memory.v1"}"#),
            Err(JsonlHeaderParseError::WrongSchema {
                schema: "ee.export.memory.v1".to_string(),
            }),
            "wrong schema",
        )
    }

    #[test]
    fn parse_jsonl_header_rejects_blank_required_fields() -> TestResult {
        let header_line = sample_jsonl()
            .lines()
            .next()
            .ok_or_else(|| "sample JSONL must include a header line".to_string())?
            .replace(
                "\"created_at\":\"2026-04-30T00:00:00Z\"",
                "\"created_at\":\"   \"",
            );

        let error = match parse_jsonl_header(&header_line) {
            Ok(_) => return Err("blank created_at must reject header".to_string()),
            Err(error) => error,
        };
        ensure(
            error,
            JsonlHeaderParseError::InvalidHeader {
                message: "header field `created_at` must not be blank".to_string(),
            },
            "blank created_at",
        )
    }

    #[test]
    fn parse_jsonl_source_collects_header_memory_and_tags() -> TestResult {
        let parsed = parse_jsonl_source(&sample_jsonl());

        ensure(parsed.has_errors(), false, "has errors")?;
        ensure(parsed.header.is_some(), true, "header parsed")?;
        ensure(parsed.footer.is_some(), true, "footer parsed")?;
        ensure(parsed.memories.len(), 1, "memory count")?;
        ensure(
            parsed
                .tags_by_memory
                .get("mem_01234567890123456789012345")
                .map(BTreeSet::len),
            Some(1),
            "tag count",
        )
    }

    #[test]
    fn parse_jsonl_source_reports_invalid_blank_header() -> TestResult {
        let input = sample_jsonl()
            .replace("\"ee_version\":\"0.1.0\"", "\"ee_version\":\"\"")
            .replace("\"export_id\":\"exp-001\"", "\"export_id\":\"   \"");
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), true, "has errors")?;
        ensure(parsed.header.is_none(), true, "invalid header omitted")?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line == Some(1)
                    && issue.code == "invalid_header"
                    && issue
                        .message
                        .contains("header field `ee_version` must not be blank")
            }),
            true,
            "invalid header issue",
        )
    }

    #[test]
    fn parse_jsonl_source_rejects_missing_header() -> TestResult {
        let parsed = parse_jsonl_source(
            r#"{"schema":"ee.export.memory.v1","memory_id":"mem_01234567890123456789012345","workspace_id":"wsp_01234567890123456789012345","level":"procedural","kind":"rule","content":"content","importance":0.8,"confidence":0.9,"utility":0.7,"created_at":"2026-04-30T00:00:00Z","updated_at":null,"expires_at":null,"source_agent":null,"provenance_uri":null,"superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}"#,
        );

        ensure(parsed.has_errors(), true, "has errors")?;
        ensure(
            parsed
                .issues
                .iter()
                .any(|issue| issue.code == "missing_header"),
            true,
            "missing header issue",
        )
    }

    #[test]
    fn parse_jsonl_source_rejects_missing_footer() -> TestResult {
        let input = sample_jsonl()
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), true, "has errors")?;
        ensure(parsed.footer.is_none(), true, "footer absent")?;
        ensure(
            parsed
                .issues
                .iter()
                .any(|issue| issue.code == "missing_footer"),
            true,
            "missing footer issue",
        )
    }

    #[test]
    fn parse_jsonl_source_rejects_blank_footer_required_fields() -> TestResult {
        let input = sample_jsonl().replace(
            "\"completed_at\":\"2026-04-30T00:01:00Z\"",
            "\"completed_at\":\"  \"",
        );
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), true, "has errors")?;
        ensure(parsed.footer.is_none(), true, "invalid footer omitted")?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line == Some(4)
                    && issue.code == "invalid_footer"
                    && issue
                        .message
                        .contains("footer field `completed_at` must not be blank")
            }),
            true,
            "invalid footer issue",
        )
    }

    #[test]
    fn parse_jsonl_source_rejects_footer_export_id_mismatch() -> TestResult {
        let input = sample_jsonl().replace(
            "\"schema\":\"ee.export.footer.v1\",\"export_id\":\"exp-001\"",
            "\"schema\":\"ee.export.footer.v1\",\"export_id\":\"exp-other\"",
        );
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), true, "has errors")?;
        ensure(parsed.footer.is_some(), true, "valid footer parsed")?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line == Some(4)
                    && issue.code == "footer_export_id_mismatch"
                    && issue.message.contains("exp-other")
                    && issue.message.contains("exp-001")
            }),
            true,
            "footer mismatch issue",
        )
    }

    #[test]
    fn parse_jsonl_source_rejects_records_after_footer() -> TestResult {
        let mut lines = sample_jsonl()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let footer = lines
            .pop()
            .ok_or_else(|| "sample JSONL must include a footer".to_string())?;
        let trailing_memory = lines
            .get(1)
            .cloned()
            .ok_or_else(|| "sample JSONL must include a memory record".to_string())?
            .replace(
                "mem_01234567890123456789012345",
                "mem_22222222222222222222222222",
            );
        lines.push(footer);
        lines.push(trailing_memory);
        let parsed = parse_jsonl_source(&lines.join("\n"));

        ensure(parsed.has_errors(), true, "has errors")?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line == Some(5)
                    && issue.code == "footer_not_last"
                    && issue.message.contains("final")
            }),
            true,
            "footer-not-last issue",
        )?;
        ensure(parsed.memories.len(), 1, "trailing memory ignored")
    }

    #[test]
    fn parse_jsonl_source_rejects_orphaned_tag_records() -> TestResult {
        let input = sample_jsonl().replace(
            "\"schema\":\"ee.export.tag.v1\",\"memory_id\":\"mem_01234567890123456789012345\"",
            "\"schema\":\"ee.export.tag.v1\",\"memory_id\":\"mem_99999999999999999999999999\"",
        );
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), true, "has errors")?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line == Some(3)
                    && issue.code == "orphaned_tag_record"
                    && issue.message.contains("mem_99999999999999999999999999")
            }),
            true,
            "orphaned tag issue",
        )
    }

    #[test]
    fn import_report_json_redacts_sensitive_source_refs() -> TestResult {
        let report = import_report_fixture(
            "/Users/alice/private/export.jsonl?api_key=redaction-fixture",
            "jsonl:///Users/alice/private/export.jsonl?api_key=redaction-fixture",
        );
        let json = report.data_json();
        let rendered = json.to_string();

        assert!(
            rendered.contains("[REDACTED_PATH]"),
            "source refs should redact path-like values: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED:"),
            "source refs should redact secret-like values: {rendered}"
        );
        assert!(
            !rendered.contains("/Users/alice") && !rendered.contains("redaction-fixture"),
            "source refs leaked sensitive material: {rendered}"
        );
        ensure(
            report.source_path,
            "/Users/alice/private/export.jsonl?api_key=redaction-fixture".to_owned(),
            "raw report source_path remains available internally",
        )
    }

    #[test]
    fn import_report_json_redacts_windows_source_refs() -> TestResult {
        let report = import_report_fixture(
            r"C:\Users\Alice\private\export.jsonl?api_key=redaction-fixture",
            r"jsonl://C:\Users\Alice\private\export.jsonl?api_key=redaction-fixture",
        );
        let json = report.data_json();
        let rendered = json.to_string();

        assert!(
            rendered.contains("[REDACTED_PATH]"),
            "source refs should redact Windows path-like values: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED:"),
            "source refs should redact secret-like values: {rendered}"
        );
        assert!(
            !rendered.contains("C:\\Users")
                && !rendered.contains("Alice")
                && !rendered.contains("redaction-fixture"),
            "source refs leaked sensitive Windows material: {rendered}"
        );
        ensure(
            report.source_path,
            r"C:\Users\Alice\private\export.jsonl?api_key=redaction-fixture".to_owned(),
            "raw Windows report source_path remains available internally",
        )
    }

    #[test]
    fn import_report_json_redacts_unc_source_refs() -> TestResult {
        let report = import_report_fixture(
            r"\\fileserver\share\team\export.jsonl",
            r"jsonl://\\fileserver\share\team\export.jsonl",
        );
        let json = report.data_json();
        let rendered = json.to_string();

        assert!(
            rendered.contains("[REDACTED_PATH]"),
            "source refs should redact UNC path-like values: {rendered}"
        );
        assert!(
            !rendered.contains("fileserver") && !rendered.contains("share"),
            "source refs leaked UNC material: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn import_report_json_preserves_safe_source_refs() -> TestResult {
        let report =
            import_report_fixture("fixtures/export.jsonl", "jsonl://fixtures/export.jsonl");
        let json = report.data_json();

        ensure(
            json["sourcePath"].as_str(),
            Some("fixtures/export.jsonl"),
            "safe sourcePath",
        )?;
        ensure(
            json["sourceId"].as_str(),
            Some("jsonl://fixtures/export.jsonl"),
            "safe sourceId",
        )
    }

    #[test]
    fn parse_jsonl_source_warns_on_footer_tag_count_mismatch() -> TestResult {
        let input = sample_jsonl().replace("\"tag_count\":1", "\"tag_count\":2");
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), false, "warning only")?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line.is_none()
                    && issue.code == "footer_tag_count_mismatch"
                    && issue.severity == JsonlImportIssueSeverity::Warning
            }),
            true,
            "tag count warning",
        )
    }

    #[test]
    fn parse_jsonl_source_warns_on_footer_artifact_count_mismatch() -> TestResult {
        let artifact_line = r#"{"schema":"ee.export.artifact.v1"}"#;
        let input = sample_jsonl()
            .replace(
                r#"{"schema":"ee.export.footer.v1""#,
                &format!("{artifact_line}\n{{\"schema\":\"ee.export.footer.v1\""),
            )
            .replace("\"total_records\":4", "\"total_records\":5")
            .replace(
                "\"memory_count\":1,\"link_count\"",
                "\"memory_count\":1,\"artifact_count\":2,\"link_count\"",
            );
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), false, "warning only")?;
        ensure(parsed.artifact_records, 1, "raw artifact records")?;
        ensure(
            parsed.ignored_records,
            1,
            "artifact row remains ignored for import",
        )?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line.is_none()
                    && issue.code == "footer_artifact_count_mismatch"
                    && issue.severity == JsonlImportIssueSeverity::Warning
            }),
            true,
            "artifact count warning",
        )
    }

    #[test]
    fn parse_jsonl_source_counts_duplicate_tag_records_separately() -> TestResult {
        let tag_line = r#"{"schema":"ee.export.tag.v1","memory_id":"mem_01234567890123456789012345","tag":"Release","created_at":"2026-04-30T00:00:00Z"}"#;
        let input = sample_jsonl()
            .replace(tag_line, &format!("{tag_line}\n{tag_line}"))
            .replace("\"total_records\":4", "\"total_records\":5")
            .replace("\"tag_count\":1", "\"tag_count\":2");
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), false, "duplicate tag record is valid")?;
        ensure(parsed.tag_records, 2, "raw tag records")?;
        ensure(
            parsed
                .tags_by_memory
                .get("mem_01234567890123456789012345")
                .map(BTreeSet::len),
            Some(1),
            "deduplicated stored tags",
        )?;
        ensure(
            parsed
                .issues
                .iter()
                .any(|issue| issue.code == "footer_tag_count_mismatch"),
            false,
            "footer tag count should compare raw tag records",
        )?;

        let report = report_from_parsed(
            Path::new("/workspace"),
            Path::new("export.jsonl"),
            "jsonl://export.jsonl",
            true,
            &parsed,
        );
        ensure(report.tag_records, 2, "reported tag records")?;

        let prepared = prepare_memories(
            &parsed,
            "wsp_01234567890123456789012345",
            &unauthenticated(),
        );
        ensure(prepared.has_errors(), false, "prepared has no errors")?;
        let memory = prepared
            .memories
            .first()
            .ok_or_else(|| "prepared memory missing".to_owned())?;
        ensure(memory.tag_count, 1, "storage tag count stays deduplicated")
    }

    #[test]
    fn parse_jsonl_source_warns_on_footer_total_records_mismatch() -> TestResult {
        let input = sample_jsonl().replace("\"total_records\":4", "\"total_records\":99");
        let parsed = parse_jsonl_source(&input);

        ensure(parsed.has_errors(), false, "warning only")?;
        ensure(
            parsed.issues.iter().any(|issue| {
                issue.line.is_none()
                    && issue.code == "footer_total_records_mismatch"
                    && issue.severity == JsonlImportIssueSeverity::Warning
                    && issue.message.contains("99")
                    && issue.message.contains("4")
            }),
            true,
            "total record count warning",
        )
    }

    #[test]
    fn prepare_memories_validates_scores() -> TestResult {
        let input = sample_jsonl().replace(r#""confidence":0.9"#, r#""confidence":1.5"#);
        let parsed = parse_jsonl_source(&input);
        let prepared = prepare_memories(
            &parsed,
            "wsp_01234567890123456789012345",
            &unauthenticated(),
        );

        ensure(prepared.has_errors(), true, "prepared has errors")?;
        ensure(
            prepared
                .issues
                .iter()
                .any(|issue| issue.code == "invalid_memory_confidence"),
            true,
            "invalid confidence issue",
        )
    }

    #[test]
    fn prepare_memories_rejects_scores_that_round_into_range_after_narrowing() -> TestResult {
        let input =
            sample_jsonl().replace(r#""confidence":0.9"#, r#""confidence":1.0000000000000002"#);
        let parsed = parse_jsonl_source(&input);
        let prepared = prepare_memories(
            &parsed,
            "wsp_01234567890123456789012345",
            &unauthenticated(),
        );

        ensure(prepared.has_errors(), true, "prepared has errors")?;
        ensure(
            prepared
                .issues
                .iter()
                .any(|issue| issue.code == "invalid_memory_confidence"),
            true,
            "rounded invalid confidence issue",
        )
    }

    #[test]
    fn prepare_memories_preserves_record_trust_metadata() -> TestResult {
        let input = sample_jsonl().replace(
            r#""utility":0.7,"created_at""#,
            r#""utility":0.7,"trust_class":"human_explicit","trust_subclass":"project-rule","created_at""#,
        );
        let parsed = parse_jsonl_source(&input);
        // Record-level human_explicit on a native artifact requires the
        // artifact to authenticate (TC-D14); this test covers preservation,
        // not the gate, so it models the authenticated case.
        let prepared =
            prepare_memories(&parsed, "wsp_01234567890123456789012345", &authenticated());

        ensure(prepared.has_errors(), false, "prepared has no errors")?;
        let memory = prepared
            .memories
            .first()
            .ok_or_else(|| "prepared memory missing".to_string())?;
        ensure(
            memory.input.trust_class.as_str(),
            "human_explicit",
            "record trust_class overrides header",
        )?;
        ensure(
            memory.input.trust_subclass.as_deref(),
            Some("project-rule"),
            "record trust_subclass overrides header",
        )
    }

    #[test]
    fn prepare_memories_preserves_missing_record_trust_subclass() -> TestResult {
        let input = sample_jsonl().replace(
            r#""utility":0.7,"created_at""#,
            r#""utility":0.7,"trust_class":"human_explicit","created_at""#,
        );
        let parsed = parse_jsonl_source(&input);
        let prepared =
            prepare_memories(&parsed, "wsp_01234567890123456789012345", &authenticated());

        ensure(prepared.has_errors(), false, "prepared has no errors")?;
        let memory = prepared
            .memories
            .first()
            .ok_or_else(|| "prepared memory missing".to_string())?;
        ensure(
            memory.input.trust_class.as_str(),
            "human_explicit",
            "record trust_class overrides header",
        )?;
        ensure(
            memory.input.trust_subclass.as_deref(),
            None,
            "missing record trust_subclass stays absent",
        )
    }

    #[test]
    fn prepare_memories_rejects_external_human_explicit_trust_override() -> TestResult {
        let input = sample_jsonl()
            .replace(
                r#""import_source":"native""#,
                r#""import_source":"external_import""#,
            )
            .replace(
                r#""utility":0.7,"created_at""#,
                r#""utility":0.7,"trust_class":"human_explicit","created_at""#,
            );
        let parsed = parse_jsonl_source(&input);
        let prepared = prepare_memories(
            &parsed,
            "wsp_01234567890123456789012345",
            &unauthenticated(),
        );

        ensure(prepared.has_errors(), true, "prepared has errors")?;
        ensure(prepared.memories.len(), 0, "external human memory blocked")?;
        ensure(
            prepared.issues.iter().any(|issue| {
                issue.code == "external_import_human_explicit_trust_class"
                    && issue.message.contains("external_import")
                    && issue.message.contains("agent_assertion")
            }),
            true,
            "external human_explicit issue",
        )
    }

    #[test]
    fn authenticated_jsonl_cannot_mint_peer_human_attested() -> TestResult {
        let input = sample_jsonl().replace(
            r#""utility":0.7,"created_at""#,
            r#""utility":0.7,"trust_class":"peer_human_attested","created_at""#,
        );
        let parsed = parse_jsonl_source(&input);
        let prepared =
            prepare_memories(&parsed, "wsp_01234567890123456789012345", &authenticated());

        ensure(prepared.has_errors(), true, "prepared has errors")?;
        ensure(prepared.memories.len(), 0, "peer attestation row blocked")?;
        ensure(
            prepared.issues.iter().any(|issue| {
                issue.code == PEER_HUMAN_ATTESTED_IMPORT_PATH_REQUIRED_CODE
                    && issue
                        .message
                        .contains("signed active-member admission path")
            }),
            true,
            "peer attestation requires team import issue",
        )
    }

    #[test]
    fn prepare_memories_preserves_lifecycle_metadata() -> TestResult {
        let input = sample_jsonl().replace(
            r#""updated_at":null,"expires_at":null"#,
            r#""updated_at":null,"tombstoned_at":"2026-05-02T00:00:00Z","tombstoned_reason":"superseded by newer release rule","valid_from":"2026-05-01T00:00:00Z","expires_at":"2026-06-01T00:00:00Z""#,
        );
        let parsed = parse_jsonl_source(&input);
        let prepared = prepare_memories(
            &parsed,
            "wsp_01234567890123456789012345",
            &unauthenticated(),
        );
        ensure(prepared.has_errors(), false, "prepared has no errors")?;
        let memory = prepared
            .memories
            .first()
            .ok_or_else(|| "prepared memory missing".to_string())?;

        ensure(
            memory.tombstoned_at.as_deref(),
            Some("2026-05-02T00:00:00Z"),
            "tombstoned_at",
        )?;
        ensure(
            memory.tombstoned_reason.as_deref(),
            Some("superseded by newer release rule"),
            "tombstoned_reason",
        )?;
        ensure(
            memory.input.valid_from.as_deref(),
            Some("2026-05-01T00:00:00Z"),
            "valid_from",
        )?;
        ensure(
            memory.input.valid_to.as_deref(),
            Some("2026-06-01T00:00:00Z"),
            "valid_to fallback from expires_at",
        )
    }

    #[test]
    fn prepare_memories_preserves_export_graph_fields_in_audit_details() -> TestResult {
        let input = sample_jsonl_with_graph_fields();
        let parsed = parse_jsonl_source(&input);
        let prepared = prepare_memories(
            &parsed,
            "wsp_01234567890123456789012345",
            &unauthenticated(),
        );
        ensure(prepared.has_errors(), false, "prepared has no errors")?;
        let memory = prepared
            .memories
            .first()
            .ok_or_else(|| "prepared memory missing".to_string())?;
        ensure(memory.bayes_posterior, Some((2.5, 1.5)), "bayes posterior")?;

        let details: JsonValue =
            serde_json::from_str(&memory.details).map_err(|error| error.to_string())?;
        let graph_fields = details
            .get("sourceGraphFields")
            .ok_or_else(|| format!("missing sourceGraphFields: {details}"))?;
        ensure(
            graph_fields
                .get("pagerank_score")
                .and_then(JsonValue::as_f64),
            Some(0.12),
            "pagerank_score",
        )?;
        ensure(
            graph_fields
                .get("betweenness_score")
                .and_then(JsonValue::as_f64),
            Some(0.34),
            "betweenness_score",
        )?;
        ensure(
            graph_fields
                .get("hits_authority")
                .and_then(JsonValue::as_f64),
            Some(0.56),
            "hits_authority",
        )?;
        ensure(
            graph_fields.get("hits_hub").and_then(JsonValue::as_f64),
            Some(0.78),
            "hits_hub",
        )?;
        ensure(
            graph_fields.get("onion_layer").and_then(JsonValue::as_u64),
            Some(3),
            "onion_layer",
        )?;
        ensure(
            graph_fields.get("k_truss_max").and_then(JsonValue::as_u64),
            Some(4),
            "k_truss_max",
        )?;
        ensure(
            graph_fields
                .get("articulation_point")
                .and_then(JsonValue::as_bool),
            Some(true),
            "articulation_point",
        )
    }

    #[test]
    fn prepare_memories_rejects_partial_bayes_posterior() -> TestResult {
        let input = sample_jsonl().replace(
            r#""utility":0.7,"created_at""#,
            r#""utility":0.7,"bayes_alpha":2.5,"created_at""#,
        );
        let parsed = parse_jsonl_source(&input);
        let prepared = prepare_memories(
            &parsed,
            "wsp_01234567890123456789012345",
            &unauthenticated(),
        );

        ensure(prepared.has_errors(), true, "prepared has errors")?;
        ensure(
            prepared
                .issues
                .iter()
                .any(|issue| issue.code == "invalid_memory_bayes_posterior"),
            true,
            "partial bayes posterior issue",
        )
    }

    #[test]
    fn import_jsonl_restores_exported_bayes_posterior() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, sample_jsonl_with_graph_fields()).map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        ensure(report.status.as_str(), "completed", "import status")?;
        ensure(report.memories_imported, 1, "memories imported")?;

        let connection =
            DbConnection::open(DatabaseConfig::file(database_path(&JsonlImportOptions {
                workspace_path: workspace,
                database_path: None,
                source_path: PathBuf::new(),
                dry_run: false,
            })))
            .map_err(|error| error.to_string())?;
        let posterior = connection
            .get_memory_bayes_posterior("mem_01234567890123456789012345")
            .map_err(|error| error.to_string())?;
        ensure(posterior, Some((2.5, 1.5)), "restored posterior")
    }

    #[test]
    fn import_jsonl_leaves_index_fresh_and_content_searchable() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, sample_jsonl()).map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        ensure(report.status.as_str(), "completed", "import status")?;
        ensure(report.memories_imported, 1, "memories imported")?;
        ensure(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "import_index_publish_failed"),
            false,
            "import drain publishes without a failure issue",
        )?;

        let status =
            crate::core::index::get_index_status(&crate::core::index::IndexStatusOptions {
                workspace_path: workspace.clone(),
                database_path: None,
                index_dir: None,
            })
            .map_err(|error| format!("index status: {error:?}"))?;
        ensure(
            status.health,
            crate::core::index::IndexHealth::Ready,
            "index ready after import without rebuild",
        )?;
        ensure(
            status.db_generation.is_some(),
            true,
            "db generation present",
        )?;
        ensure(
            status.db_generation == status.index_generation,
            true,
            "import leaves database and index generations equal",
        )?;

        let connection =
            DbConnection::open(DatabaseConfig::file(database_path(&JsonlImportOptions {
                workspace_path: workspace.clone(),
                database_path: None,
                source_path: PathBuf::new(),
                dry_run: false,
            })))
            .map_err(|error| error.to_string())?;
        let workspace_id =
            ensure_workspace(&connection, &workspace).map_err(|error| error.to_string())?;
        let pending = connection
            .list_pending_search_index_jobs(&workspace_id, None)
            .map_err(|error| error.to_string())?;
        ensure(pending.len(), 0, "pending index jobs after import drain")?;

        let search = crate::core::search::run_search_with_filters(
            &crate::core::search::SearchOptions {
                workspace_path: workspace,
                database_path: None,
                index_dir: None,
                query: "cargo fmt release".to_owned(),
                limit: 5,
                speed: crate::search::SpeedMode::Instant,
                explain: false,
                as_of: None,
                include_tombstoned: false,
                include_expired: false,
                include_future: false,
                include_stale: false,
                relevance_floor: Some(0.0),
                dedup_mode: crate::core::search::SearchDedupMode::DocId,
                source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
                strict_source_mode: true,
                memory_scope: crate::models::MemoryScope::Workspace,
                strict_scope: false,
            },
            None,
            &[],
        )
        .map_err(|error| format!("post-import search: {error:?}"))?;
        ensure(
            search
                .results
                .iter()
                .any(|hit| hit.doc_id == "mem_01234567890123456789012345"),
            true,
            "imported memory searchable immediately without rebuild",
        )?;
        ensure(
            search
                .degraded
                .iter()
                .any(|entry| entry.code == "search_index_stale"),
            false,
            "no stale advisory after import drain",
        )
    }

    #[cfg(unix)]
    #[test]
    fn import_jsonl_survives_noncompleted_index_publication_with_retryable_job() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace with spaces");
        fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, sample_jsonl()).map_err(|error| error.to_string())?;

        let index_dir = workspace
            .join(".ee")
            .join(crate::core::index::DEFAULT_INDEX_SUBDIR);
        let blocked_target = workspace.join(".ee").join("index-publish-blocker");
        fs::create_dir_all(&blocked_target).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&blocked_target, &index_dir)
            .map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        ensure(
            report.status.as_str(),
            "completed",
            "source import remains completed",
        )?;
        ensure(
            report.memories_imported,
            1,
            "source memory remains imported",
        )?;
        let issue = report
            .issues
            .iter()
            .find(|issue| issue.code == "import_index_publish_failed")
            .ok_or("missing truthful import index publication issue")?;
        let expected_repair = format!(
            "ee index rebuild --workspace {}",
            jsonl_import_shell_quote_arg(workspace.to_string_lossy().as_ref())
        );
        ensure(
            issue
                .message
                .contains("automatic publication of durable search-index jobs did not complete")
                && issue.message.contains("Search may omit imported memories")
                && issue.message.contains(&format!("Run `{expected_repair}`")),
            true,
            "publication issue carries exact failure truth and shell-safe repair",
        )?;

        let connection = DbConnection::open(DatabaseConfig::file(workspace.join(".ee/ee.db")))
            .map_err(|error| error.to_string())?;
        ensure(
            connection
                .get_memory("mem_01234567890123456789012345")
                .map_err(|error| error.to_string())?
                .is_some(),
            true,
            "source-of-truth imported memory survives publication failure",
        )?;
        let workspace_id =
            ensure_workspace(&connection, &workspace).map_err(|error| error.to_string())?;
        let jobs = connection
            .list_search_index_jobs(&workspace_id, None)
            .map_err(|error| error.to_string())?;
        ensure(
            jobs.iter().any(|job| {
                matches!(
                    job.status_enum(),
                    Some(
                        crate::db::SearchIndexJobStatus::Pending
                            | crate::db::SearchIndexJobStatus::Failed
                    )
                )
            }),
            true,
            "noncompleted publication leaves durable retryable index work",
        )
    }

    fn human_explicit_jsonl() -> String {
        sample_jsonl().replace(
            r#""utility":0.7,"created_at""#,
            r#""utility":0.7,"trust_class":"human_explicit","created_at""#,
        )
    }

    /// Replace the sample footer with one carrying `authentication`, MAC'd by
    /// the store at `workspace` over the artifact's memory lines, bound to
    /// `workspace_scope`.
    fn authenticate_sample(
        artifact: &str,
        workspace: &Path,
        workspace_scope: &str,
    ) -> Result<String, String> {
        use crate::policy::import_auth::authenticate_artifact;

        let root = StoreAuthRoot::open_or_create(workspace_keys_dir(workspace))
            .map_err(|error| error.message())?;
        let mut builder = RecordsRootBuilder::new();
        for line in artifact.lines() {
            let value: JsonValue =
                serde_json::from_str(line.trim()).map_err(|error| error.to_string())?;
            if value.get("schema").and_then(JsonValue::as_str) == Some(EXPORT_MEMORY_SCHEMA_V1) {
                let memory_id = value
                    .get("memory_id")
                    .and_then(JsonValue::as_str)
                    .ok_or("memory line without memory_id")?;
                builder.push(memory_id, &canonical_record_hash(line.trim().as_bytes()));
            }
        }
        let header = authenticate_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &ArtifactContext {
                artifact_family: EXPORT_ARTIFACT_FAMILY,
                record_encoding_version: EXPORT_RECORD_ENCODING_V1,
                source_key_namespace: STORE_KEY_NAMESPACE_V1,
                workspace_scope,
            },
            &builder.finalize(),
            builder.count(),
        )
        .map_err(|error| error.message())?;
        let authentication = serde_json::to_string(&header).map_err(|error| error.to_string())?;
        Ok(artifact.replace(
            r#""error_message":null}"#,
            &format!(r#""error_message":null,"authentication":{authentication}}}"#),
        ))
    }

    /// Workspace fixture for authenticated-import tests: canonical path, a
    /// migrated DB, and the workspace id `ee import jsonl` will resolve.
    fn authenticated_import_workspace(
        tempdir: &tempfile::TempDir,
    ) -> Result<(PathBuf, String), String> {
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(workspace.join(crate::config::WORKSPACE_MARKER))
            .map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let connection = DbConnection::open(DatabaseConfig::file(
            workspace
                .join(crate::config::WORKSPACE_MARKER)
                .join(DEFAULT_DB_FILE),
        ))
        .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id =
            ensure_workspace(&connection, &workspace).map_err(|error| error.to_string())?;
        Ok((workspace, workspace_id))
    }

    #[test]
    fn native_human_explicit_without_authentication_is_refused() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (workspace, _workspace_id) = authenticated_import_workspace(&tempdir)?;
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, human_explicit_jsonl()).map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;

        ensure(report.status.as_str(), "rejected", "import status")?;
        ensure(report.memories_imported, 0, "memories imported")?;
        ensure(
            report
                .issues
                .iter()
                .any(|issue| issue.code == UNAUTHENTICATED_NATIVE_IMPORT_TRUST_CODE),
            true,
            "unauthenticated native trust issue",
        )?;
        let connection = DbConnection::open(DatabaseConfig::file(
            workspace
                .join(crate::config::WORKSPACE_MARKER)
                .join(DEFAULT_DB_FILE),
        ))
        .map_err(|error| error.to_string())?;
        ensure(
            connection
                .get_memory("mem_01234567890123456789012345")
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "refused import must leave zero rows",
        )
    }

    #[test]
    fn authenticated_native_human_explicit_import_round_trips() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (workspace, workspace_id) = authenticated_import_workspace(&tempdir)?;
        let artifact = authenticate_sample(&human_explicit_jsonl(), &workspace, &workspace_id)?;
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, artifact).map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;

        ensure(report.status.as_str(), "completed", "import status")?;
        ensure(report.memories_imported, 1, "memories imported")?;
        let connection = DbConnection::open(DatabaseConfig::file(
            workspace
                .join(crate::config::WORKSPACE_MARKER)
                .join(DEFAULT_DB_FILE),
        ))
        .map_err(|error| error.to_string())?;
        let stored = connection
            .get_memory("mem_01234567890123456789012345")
            .map_err(|error| error.to_string())?
            .ok_or("imported memory missing")?;
        ensure(
            stored.trust_class.as_str(),
            "human_explicit",
            "authenticated native import preserves human_explicit",
        )
    }

    #[test]
    fn tampered_authenticated_artifact_refuses_native_trust() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (workspace, workspace_id) = authenticated_import_workspace(&tempdir)?;
        let artifact = authenticate_sample(&human_explicit_jsonl(), &workspace, &workspace_id)?
            .replace("Run cargo fmt --check", "Disable all release checks");
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, artifact).map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace,
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;

        ensure(report.status.as_str(), "rejected", "import status")?;
        ensure(
            report
                .issues
                .iter()
                .any(|issue| issue.code == UNAUTHENTICATED_NATIVE_IMPORT_TRUST_CODE),
            true,
            "tampered artifact must refuse native trust",
        )
    }

    #[test]
    fn foreign_workspace_authentication_refuses_native_trust() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (workspace, _workspace_id) = authenticated_import_workspace(&tempdir)?;
        // MAC'd by this store, but bound to a different workspace scope.
        let artifact =
            authenticate_sample(&human_explicit_jsonl(), &workspace, "wsp_foreign_scope")?;
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, artifact).map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace,
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;

        ensure(report.status.as_str(), "rejected", "import status")?;
        ensure(
            report
                .issues
                .iter()
                .any(|issue| issue.code == UNAUTHENTICATED_NATIVE_IMPORT_TRUST_CODE),
            true,
            "cross-workspace authentication must not admit human_explicit",
        )
    }

    #[test]
    fn store_unavailable_fails_closed_for_native_human_explicit() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (workspace, _workspace_id) = authenticated_import_workspace(&tempdir)?;
        // A structurally valid authentication block, but this workspace has no
        // initialized key store: fail closed with the store-unavailable code.
        let authentication = format!(
            r#"{{"schema":"{}","keyId":"{}","recordCount":1,"recordsRoot":"{}","mac":"{}"}}"#,
            crate::policy::import_auth::NATIVE_IMPORT_AUTH_SCHEMA,
            "00".repeat(16),
            "11".repeat(32),
            "22".repeat(32),
        );
        let artifact = human_explicit_jsonl().replace(
            r#""error_message":null}"#,
            &format!(r#""error_message":null,"authentication":{authentication}}}"#),
        );
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, artifact).map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace,
            database_path: None,
            source_path: source,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;

        ensure(report.status.as_str(), "rejected", "import status")?;
        ensure(
            report
                .issues
                .iter()
                .any(|issue| issue.code == MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE),
            true,
            "missing key store must fail closed for native human_explicit",
        )
    }

    #[test]
    fn reimport_preserves_existing_row_and_flags_divergence() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let source = tempdir.path().join("source.jsonl");
        fs::write(&source, sample_jsonl()).map_err(|error| error.to_string())?;
        let options = JsonlImportOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            source_path: source.clone(),
            dry_run: false,
        };
        let first = import_jsonl_records(&options).map_err(|error| error.to_string())?;
        ensure(first.memories_imported, 1, "first import")?;

        // Byte-identical reimport: pure no-op, no conflict signal.
        let second = import_jsonl_records(&options).map_err(|error| error.to_string())?;
        ensure(second.status.as_str(), "completed", "identical reimport")?;
        ensure(second.memories_skipped_duplicate, 1, "identical skip")?;
        ensure(
            second
                .issues
                .iter()
                .any(|issue| issue.code == "reimport_divergent_existing_row"),
            false,
            "identical reimport must not flag a conflict",
        )?;

        // Divergent reimport: same id, edited content — preserved + flagged.
        fs::write(
            &source,
            sample_jsonl().replace("Run cargo fmt --check", "Never run cargo fmt"),
        )
        .map_err(|error| error.to_string())?;
        let third = import_jsonl_records(&options).map_err(|error| error.to_string())?;
        ensure(third.status.as_str(), "completed", "divergent reimport")?;
        ensure(third.memories_skipped_duplicate, 1, "divergent skip")?;
        ensure(
            third
                .issues
                .iter()
                .any(|issue| issue.code == "reimport_divergent_existing_row"),
            true,
            "divergent reimport must flag the preserved conflict",
        )?;
        let connection = DbConnection::open(DatabaseConfig::file(database_path(&options)))
            .map_err(|error| error.to_string())?;
        let stored = connection
            .get_memory("mem_01234567890123456789012345")
            .map_err(|error| error.to_string())?
            .ok_or("memory missing after reimport")?;
        ensure(
            stored.content.contains("Run cargo fmt --check"),
            true,
            "existing row content must be preserved, never overwritten",
        )
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_symlinked_database_parent_before_create() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_path = tempdir.path().join("export.jsonl");
        fs::write(&source_path, sample_jsonl()).map_err(|error| error.to_string())?;

        let real_database_dir = tempdir.path().join("real-db");
        fs::create_dir_all(&real_database_dir).map_err(|error| error.to_string())?;
        let linked_database_dir = tempdir.path().join("linked-db");
        symlink(&real_database_dir, &linked_database_dir).map_err(|error| error.to_string())?;
        let database_path = linked_database_dir.join("ee.db");

        let error = match import_jsonl_records(&JsonlImportOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: Some(database_path),
            source_path,
            dry_run: false,
        }) {
            Ok(report) => {
                return Err(format!(
                    "import should reject symlinked DB path: {report:?}"
                ));
            }
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("symlinked path component"),
            "unexpected error: {error}"
        );
        assert!(
            !real_database_dir.join("ee.db").exists(),
            "import must not create a database through a symlinked parent"
        );
        Ok(())
    }

    #[test]
    fn import_rejects_non_regular_database_path_before_open() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_path = tempdir.path().join("export.jsonl");
        fs::write(&source_path, sample_jsonl()).map_err(|error| error.to_string())?;
        let database_path = tempdir.path().join("workspace").join(".ee").join("ee.db");
        fs::create_dir_all(&database_path).map_err(|error| error.to_string())?;

        let error = match import_jsonl_records(&JsonlImportOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: Some(database_path),
            source_path,
            dry_run: false,
        }) {
            Ok(report) => {
                return Err(format!(
                    "import should reject directory DB path: {report:?}"
                ));
            }
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("non-regular database path"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn import_accepts_canonical_absolute_source_path() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_path = tempdir.path().join("export.jsonl");
        fs::write(&source_path, sample_jsonl()).map_err(|error| error.to_string())?;
        let canonical_source = source_path
            .canonicalize()
            .map_err(|error| error.to_string())?;

        let report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: None,
            source_path: canonical_source,
            dry_run: true,
        })
        .map_err(|error| error.to_string())?;

        ensure(report.status.as_str(), "dry_run", "import status")
    }

    #[test]
    fn database_path_safety_accepts_canonical_absolute_missing_tail() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = tempdir
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let database_path = root.join("workspace").join(".ee").join("ee.db");

        ensure_import_database_path_is_safe_for_write(&database_path)
            .map_err(|error| error.to_string())
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_symlinked_source_path_components() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_source_dir = tempdir.path().join("real-source");
        fs::create_dir_all(&real_source_dir).map_err(|error| error.to_string())?;
        let real_source = real_source_dir.join("export.jsonl");
        fs::write(&real_source, sample_jsonl()).map_err(|error| error.to_string())?;

        let linked_source_dir = tempdir.path().join("linked-source");
        symlink(&real_source_dir, &linked_source_dir).map_err(|error| error.to_string())?;
        let parent_error = match import_jsonl_records(&JsonlImportOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: None,
            source_path: linked_source_dir.join("export.jsonl"),
            dry_run: true,
        }) {
            Ok(_) => return Err("import should reject symlinked source parent".to_owned()),
            Err(error) => error,
        };
        assert!(
            parent_error
                .to_string()
                .contains("symlinked path component"),
            "unexpected error: {parent_error}"
        );

        let linked_source_file = tempdir.path().join("linked-export.jsonl");
        symlink(&real_source, &linked_source_file).map_err(|error| error.to_string())?;
        let file_error = match import_jsonl_records(&JsonlImportOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: None,
            source_path: linked_source_file,
            dry_run: true,
        }) {
            Ok(_) => return Err("import should reject symlinked source file".to_owned()),
            Err(error) => error,
        };
        assert!(
            file_error.to_string().contains("symlinked path component"),
            "unexpected error: {file_error}"
        );
        Ok(())
    }

    #[test]
    fn import_rejects_non_regular_source_path_before_read() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_dir = tempdir.path().join("export.jsonl");
        fs::create_dir_all(&source_dir).map_err(|error| error.to_string())?;

        let error = match import_jsonl_records(&JsonlImportOptions {
            workspace_path: tempdir.path().join("workspace"),
            database_path: None,
            source_path: source_dir,
            dry_run: true,
        }) {
            Ok(_) => return Err("import should reject directory source path".to_owned()),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("non-regular path"),
            "unexpected error: {error}"
        );
        Ok(())
    }
}
