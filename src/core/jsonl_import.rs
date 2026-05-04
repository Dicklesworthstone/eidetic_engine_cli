//! JSONL import execution (EE-222).
//!
//! The import path consumes EE JSONL export records, validates their schemas,
//! and imports memory records into the local workspace database. Non-memory
//! records are parsed for accounting but are not replayed as durable state in
//! this slice.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::db::{
    CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput, DatabaseConfig, DbConnection,
    DbError,
};
use crate::models::{
    EXPORT_AGENT_SCHEMA_V1, EXPORT_ARTIFACT_SCHEMA_V1, EXPORT_AUDIT_SCHEMA_V1,
    EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1, EXPORT_LINK_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1, EXPORT_WORKSPACE_SCHEMA_V1, ExportFooter,
    ExportHeader, ExportMemoryRecord, ExportTagRecord, IMPORT_JSONL_SCHEMA_V1, ImportSource,
    MemoryContent, MemoryId, MemoryKind, MemoryLevel, Tag, TrustClass, TrustLevel, UnitScore,
    WorkspaceId,
};

const DEFAULT_DB_FILE: &str = "ee.db";
const IMPORT_ACTION: &str = "memory.import.jsonl";

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
    Error,
    Warning,
}

impl JsonlImportIssueSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
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
pub fn parse_jsonl_header_line(input: &str) -> Result<ExportHeader, JsonlHeaderParseError> {
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

    serde_json::from_value::<ExportHeader>(value).map_err(|error| {
        JsonlHeaderParseError::InvalidHeader {
            message: error.to_string(),
        }
    })
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
            "sourcePath": self.source_path,
            "sourceId": self.source_id,
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
    pub tag_count: u64,
    pub success: bool,
}

impl JsonlImportFooterSummary {
    fn from_footer(footer: &ExportFooter) -> Self {
        Self {
            export_id: footer.export_id.clone(),
            total_records: footer.total_records,
            memory_count: footer.memory_count,
            tag_count: footer.tag_count,
            success: footer.success,
        }
    }

    fn data_json(&self) -> JsonValue {
        json!({
            "exportId": self.export_id,
            "totalRecords": self.total_records,
            "memoryCount": self.memory_count,
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
            Self::Storage(_) => Some("ee init --workspace . && ee db migrate --workspace ."),
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
    memories: Vec<ExportMemoryRecord>,
    tags_by_memory: BTreeMap<String, BTreeSet<String>>,
    issues: Vec<JsonlImportIssue>,
    records_total: u32,
    ignored_records: u32,
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
    let source_path = normalize_path(&options.source_path);
    let source_id = source_id(&source_path);
    let input = std::fs::read_to_string(&source_path).map_err(|error| JsonlImportError::Io {
        path: source_path.clone(),
        message: error.to_string(),
    })?;

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

    let prepared = prepare_memories(&parsed, &workspace_id);
    if prepared.has_errors() {
        report.issues.extend(prepared.issues);
        report.status = "rejected".to_owned();
        report.database_path = Some(database_path.to_string_lossy().into_owned());
        return Ok(report);
    }

    let mut to_insert = Vec::new();
    let mut skipped_duplicate = 0_u32;
    for memory in prepared.memories {
        if connection.get_memory(&memory.id)?.is_some() {
            skipped_duplicate = skipped_duplicate.saturating_add(1);
        } else {
            to_insert.push(memory);
        }
    }

    connection.with_transaction(|| {
        for memory in &to_insert {
            connection.insert_memory(&memory.id, &memory.input)?;
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
    Ok(report)
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
        tag_records: parsed.tags_by_memory.values().fold(0_u32, |total, tags| {
            total.saturating_add(saturating_len(tags.len()))
        }),
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
        memories: Vec::new(),
        tags_by_memory: BTreeMap::new(),
        issues: Vec::new(),
        records_total: 0,
        ignored_records: 0,
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

        match schema {
            EXPORT_HEADER_SCHEMA_V1 => parse_header_record(&mut parsed, line_number, value),
            EXPORT_MEMORY_SCHEMA_V1 => {
                parse_memory_record(&mut parsed, &mut seen_memory_ids, line_number, value);
            }
            EXPORT_TAG_SCHEMA_V1 => parse_tag_record(&mut parsed, line_number, value),
            EXPORT_FOOTER_SCHEMA_V1 => parse_footer_record(&mut parsed, line_number, value),
            EXPORT_AGENT_SCHEMA_V1
            | EXPORT_ARTIFACT_SCHEMA_V1
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
    match serde_json::from_value::<ExportHeader>(value) {
        Ok(header) => parsed.header = Some(header),
        Err(error) => parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "invalid_header",
            error.to_string(),
        )),
    }
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
        Ok(tag) => match Tag::parse(&tag.tag) {
            Ok(canonical) => {
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
        },
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
    match serde_json::from_value::<ExportFooter>(value) {
        Ok(footer) => parsed.footer = Some(footer),
        Err(error) => parsed.issues.push(JsonlImportIssue::error(
            Some(line_number),
            "invalid_footer",
            error.to_string(),
        )),
    }
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

    if let Some((line, schema)) = first_schema {
        if schema != EXPORT_HEADER_SCHEMA_V1 {
            parsed.issues.push(JsonlImportIssue::error(
                Some(line),
                "header_not_first",
                "the first non-empty JSONL record must be ee.export.header.v1",
            ));
        }
    }

    if let Some(footer) = &parsed.footer {
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

fn prepare_memories(parsed: &ParsedJsonlImport, workspace_id: &str) -> PreparedMemories {
    let trust_class = trust_class_for_header(parsed.header.as_ref());
    let trust_subclass = trust_subclass_for_header(parsed.header.as_ref());
    let mut memories = Vec::with_capacity(parsed.memories.len());
    let mut issues = Vec::new();

    for memory in &parsed.memories {
        match prepare_memory(memory, workspace_id, trust_class, &trust_subclass, parsed) {
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
) -> Result<PreparedMemory, JsonlImportIssue> {
    let _: MemoryId = memory.memory_id.parse().map_err(|error| {
        JsonlImportIssue::error(
            None,
            "invalid_memory_id",
            format!("memory id `{}` is invalid: {error}", memory.memory_id),
        )
    })?;
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
    let tags = parsed
        .tags_by_memory
        .get(&memory.memory_id)
        .map(|tags| tags.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let tag_count = saturating_len(tags.len());

    Ok(PreparedMemory {
        id: memory.memory_id.clone(),
        input: CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: level.as_str().to_owned(),
            kind: kind.as_str().to_owned(),
            content: content.as_str().to_owned(),
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
            trust_subclass: Some(trust_subclass.to_owned()),
            tags,
            valid_from: None,
            valid_to: None,
        },
        details: json!({
            "schema": IMPORT_JSONL_SCHEMA_V1,
            "sourceMemoryId": memory.memory_id,
            "sourceWorkspaceId": memory.workspace_id,
            "sourceCreatedAt": memory.created_at,
            "sourceUpdatedAt": memory.updated_at,
            "redacted": memory.redacted,
            "redactionReason": memory.redaction_reason,
        })
        .to_string(),
        tag_count,
    })
}

fn score_or_default(value: Option<f64>, default: f32) -> Result<f32, String> {
    let score = value.map_or(default, |score| score as f32);
    UnitScore::parse(score)
        .map(UnitScore::into_inner)
        .map_err(|error| format!("score is invalid: {error}"))
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
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| JsonlImportError::Io {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })
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

    fn sample_jsonl() -> String {
        [
            r#"{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-04-30T00:00:00Z","workspace_id":"wsp_01234567890123456789012345","workspace_path":"/source","export_scope":"memories","redaction_level":"none","record_count":3,"ee_version":"0.1.0","hostname":null,"export_id":"exp-001","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}"#,
            r#"{"schema":"ee.export.memory.v1","memory_id":"mem_01234567890123456789012345","workspace_id":"wsp_01234567890123456789012345","level":"procedural","kind":"rule","content":"Run cargo fmt --check before release.","importance":0.8,"confidence":0.9,"utility":0.7,"created_at":"2026-04-30T00:00:00Z","updated_at":null,"expires_at":null,"source_agent":"MistySalmon","provenance_uri":"ee-export://fixture","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}"#,
            r#"{"schema":"ee.export.tag.v1","memory_id":"mem_01234567890123456789012345","tag":"Release","created_at":"2026-04-30T00:00:00Z"}"#,
            r#"{"schema":"ee.export.footer.v1","export_id":"exp-001","completed_at":"2026-04-30T00:01:00Z","total_records":3,"memory_count":1,"link_count":0,"tag_count":1,"audit_count":0,"checksum":null,"success":true,"error_message":null}"#,
        ]
        .join("\n")
    }

    #[test]
    fn parse_jsonl_header_line_accepts_header_record_only() -> TestResult {
        let header_line = sample_jsonl()
            .lines()
            .next()
            .ok_or_else(|| "sample JSONL must include a header line".to_string())?
            .to_string();
        let header = parse_jsonl_header_line(&header_line).map_err(|error| error.to_string())?;

        ensure(header.export_id, "exp-001".to_string(), "export id")?;
        ensure(
            parse_jsonl_header_line(r#"{"schema":"ee.export.memory.v1"}"#),
            Err(JsonlHeaderParseError::WrongSchema {
                schema: "ee.export.memory.v1".to_string(),
            }),
            "wrong schema",
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
    fn prepare_memories_validates_scores() -> TestResult {
        let input = sample_jsonl().replace(r#""confidence":0.9"#, r#""confidence":1.5"#);
        let parsed = parse_jsonl_source(&input);
        let prepared = prepare_memories(&parsed, "wsp_01234567890123456789012345");

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
}
