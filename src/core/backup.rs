//! Backup creation support (EE-223).
//!
//! This first backup slice writes a side-path backup directory containing a
//! redacted JSONL export plus a manifest with content hashes. It never
//! overwrites an existing backup artifact.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use serde_json::{Value as JsonValue, json};

use crate::config::WORKSPACE_MARKER;
use crate::core::jsonl_import::{JsonlImportOptions, import_jsonl_records};
use crate::db::{
    DatabaseConfig, DbConnection, StoredAuditEntry, StoredGraphSnapshot, StoredMemory,
    StoredMemoryLink, StoredTaskEpisode, audit_actions,
};
use crate::models::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_LIST_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1, BACKUP_MANIFEST_SCHEMA_V2, BACKUP_RESTORE_SCHEMA_V1,
    BACKUP_VERIFY_SCHEMA_V1, BackupId, DomainError, ExportAuditRecord, ExportFooter, ExportHeader,
    ExportLinkRecord, ExportMemoryRecord, ExportScope, ExportTagRecord, ExportWorkspaceRecord,
    ImportSource, RedactionLevel, TrustLevel, jsonl::ExportRecordBuildError,
};
use crate::output::jsonl_export::{ExportStats, JsonlExporter};

const DEFAULT_DB_FILE: &str = "ee.db";
const DEFAULT_BACKUP_DIR: &str = "backups";
const DEFAULT_RESTORE_DIR: &str = "restores";
const RECORDS_FILE: &str = "records.jsonl";
const MANIFEST_FILE: &str = "manifest.json";

/// Options for one `ee backup create` operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupCreateOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub label: Option<String>,
    pub redaction_level: RedactionLevel,
    pub include_derived: bool,
    pub dry_run: bool,
}

/// Options for listing backup manifests under a workspace or explicit root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupListOptions {
    pub workspace_path: PathBuf,
    pub output_dir: Option<PathBuf>,
}

/// Options for inspecting one backup directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupInspectOptions {
    pub backup_path: PathBuf,
}

/// Options for verifying one backup directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupVerifyOptions {
    pub backup_path: PathBuf,
}

/// Options for restoring one backup into an isolated side path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupRestoreOptions {
    pub workspace_path: PathBuf,
    pub backup_path: PathBuf,
    pub side_path: PathBuf,
    pub dry_run: bool,
}

/// Stable report returned by `ee backup create`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupCreateReport {
    pub schema: &'static str,
    pub backup_id: String,
    pub label: Option<String>,
    pub status: String,
    pub dry_run: bool,
    pub workspace_path: String,
    pub workspace_id: String,
    pub database_path: String,
    pub backup_path: String,
    pub manifest_path: String,
    pub records_path: String,
    pub manifest_hash: Option<String>,
    pub records_hash: Option<String>,
    pub redaction_level: RedactionLevel,
    pub export_scope: ExportScope,
    pub include_derived: bool,
    pub total_records: u64,
    pub memory_count: u64,
    pub link_count: u64,
    pub tag_count: u64,
    pub audit_count: u64,
    pub verification_status: String,
    pub artifacts: Vec<BackupArtifactReport>,
    pub derived: Vec<BackupDerivedAssetReport>,
    pub degraded: Vec<BackupDegradation>,
}

impl BackupCreateReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "backup create",
            "backupId": self.backup_id,
            "label": self.label,
            "status": self.status,
            "dryRun": self.dry_run,
            "workspacePath": self.workspace_path,
            "workspaceId": self.workspace_id,
            "databasePath": self.database_path,
            "backupPath": self.backup_path,
            "manifestPath": self.manifest_path,
            "recordsPath": self.records_path,
            "manifestHash": self.manifest_hash,
            "recordsHash": self.records_hash,
            "redactionLevel": self.redaction_level.as_str(),
            "exportScope": self.export_scope.as_str(),
            "includeDerived": self.include_derived,
            "counts": {
                "totalRecords": self.total_records,
                "memoryRecords": self.memory_count,
                "linkRecords": self.link_count,
                "tagRecords": self.tag_count,
                "auditRecords": self.audit_count,
            },
            "verificationStatus": self.verification_status,
            "artifacts": self.artifacts.iter().map(BackupArtifactReport::data_json).collect::<Vec<_>>(),
            "derived": self.derived.iter().map(BackupDerivedAssetReport::data_json).collect::<Vec<_>>(),
            "degraded": self.degraded.iter().map(BackupDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let prefix = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{prefix}backup {status}: {backup_id} ({memories} memories, {audit} audit records)\n  path: {path}\n",
            status = self.status,
            backup_id = self.backup_id,
            memories = self.memory_count,
            audit = self.audit_count,
            path = self.backup_path,
        )
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "BACKUP_CREATE|{}|{}|{}|{}|{}",
            self.backup_id,
            self.status,
            self.memory_count,
            self.audit_count,
            self.verification_status
        )
    }
}

/// Stable counts parsed from a backup manifest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackupCounts {
    pub total_records: u64,
    pub memory_count: u64,
    pub link_count: u64,
    pub tag_count: u64,
    pub audit_count: u64,
}

impl BackupCounts {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "totalRecords": self.total_records,
            "memoryRecords": self.memory_count,
            "linkRecords": self.link_count,
            "tagRecords": self.tag_count,
            "auditRecords": self.audit_count,
        })
    }
}

/// A verification or inspection issue discovered in a backup manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupVerificationIssue {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Option<String>,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

impl BackupVerificationIssue {
    fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: "error".to_owned(),
            message: message.into(),
            path: None,
            expected: None,
            actual: None,
        }
    }

    fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: "warning".to_owned(),
            message: message.into(),
            path: None,
            expected: None,
            actual: None,
        }
    }

    fn high(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: "high".to_owned(),
            message: message.into(),
            path: None,
            expected: None,
            actual: None,
        }
    }

    fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn with_expected_actual(
        mut self,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        self.expected = Some(expected.into());
        self.actual = Some(actual.into());
        self
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "path": self.path,
            "expected": self.expected,
            "actual": self.actual,
        })
    }
}

/// Stable report returned by backup manifest inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupInspectReport {
    pub schema: &'static str,
    pub backup_id: String,
    pub label: Option<String>,
    pub created_at: Option<String>,
    pub ee_version: Option<String>,
    pub backup_path: String,
    pub manifest_path: String,
    pub manifest_hash: String,
    pub workspace_id: Option<String>,
    pub workspace_path: Option<String>,
    pub database_path: Option<String>,
    pub redaction_level: Option<String>,
    pub export_scope: Option<String>,
    pub counts: BackupCounts,
    pub verification_status: Option<String>,
    pub artifacts: Vec<BackupArtifactReport>,
    pub derived: Vec<BackupDerivedAssetReport>,
    pub degraded: Vec<BackupDegradation>,
    pub issues: Vec<BackupVerificationIssue>,
}

impl BackupInspectReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "backup inspect",
            "backupId": self.backup_id,
            "label": self.label,
            "createdAt": self.created_at,
            "eeVersion": self.ee_version,
            "backupPath": self.backup_path,
            "manifestPath": self.manifest_path,
            "manifestHash": self.manifest_hash,
            "workspace": {
                "id": self.workspace_id,
                "path": self.workspace_path,
            },
            "databasePath": self.database_path,
            "redactionLevel": self.redaction_level,
            "exportScope": self.export_scope,
            "counts": self.counts.data_json(),
            "verificationStatus": self.verification_status,
            "artifacts": self.artifacts.iter().map(BackupArtifactReport::data_json).collect::<Vec<_>>(),
            "derived": self.derived.iter().map(BackupDerivedAssetReport::data_json).collect::<Vec<_>>(),
            "degraded": self.degraded.iter().map(BackupDegradation::data_json).collect::<Vec<_>>(),
            "issues": self.issues.iter().map(BackupVerificationIssue::data_json).collect::<Vec<_>>(),
        })
    }
}

/// One entry in a backup list report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupListEntry {
    pub backup_id: String,
    pub label: Option<String>,
    pub created_at: Option<String>,
    pub backup_path: String,
    pub manifest_path: String,
    pub manifest_hash: String,
    pub verification_status: Option<String>,
    pub issue_count: usize,
}

impl BackupListEntry {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "backupId": self.backup_id,
            "label": self.label,
            "createdAt": self.created_at,
            "backupPath": self.backup_path,
            "manifestPath": self.manifest_path,
            "manifestHash": self.manifest_hash,
            "verificationStatus": self.verification_status,
            "issueCount": self.issue_count,
        })
    }
}

/// Stable report returned by backup listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupListReport {
    pub schema: &'static str,
    pub backup_root: String,
    pub backups: Vec<BackupListEntry>,
    pub degraded: Vec<BackupDegradation>,
}

impl BackupListReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "backup list",
            "backupRoot": self.backup_root,
            "backups": self.backups.iter().map(BackupListEntry::data_json).collect::<Vec<_>>(),
            "degraded": self.degraded.iter().map(BackupDegradation::data_json).collect::<Vec<_>>(),
        })
    }
}

/// Stable report returned by backup verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupVerifyReport {
    pub schema: &'static str,
    pub backup_id: String,
    pub status: String,
    pub backup_path: String,
    pub manifest_path: String,
    pub manifest_hash: String,
    pub checked_artifacts: Vec<BackupArtifactReport>,
    pub checked_derived: Vec<BackupDerivedAssetReport>,
    pub issues: Vec<BackupVerificationIssue>,
}

/// Stable report returned by `ee backup restore`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupRestoreReport {
    pub schema: &'static str,
    pub backup_id: String,
    pub status: String,
    pub dry_run: bool,
    pub backup_path: String,
    pub side_path: String,
    pub restore_artifact_dir: String,
    pub source_manifest_path: String,
    pub source_records_path: String,
    pub source_manifest_hash: String,
    pub restored_database_path: String,
    pub import_status: String,
    pub imported_memory_count: u32,
    pub skipped_duplicate_count: u32,
    pub restored_derived: Vec<BackupRestoredDerivedAssetReport>,
    pub issue_count: u32,
    pub next_actions: Vec<String>,
}

impl BackupRestoreReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "backup restore",
            "backupId": self.backup_id,
            "status": self.status,
            "dryRun": self.dry_run,
            "backupPath": self.backup_path,
            "sidePath": self.side_path,
            "restoreArtifactDir": self.restore_artifact_dir,
            "sourceManifestPath": self.source_manifest_path,
            "sourceRecordsPath": self.source_records_path,
            "sourceManifestHash": self.source_manifest_hash,
            "restoredDatabasePath": self.restored_database_path,
            "importStatus": self.import_status,
            "counts": {
                "memoriesImported": self.imported_memory_count,
                "memoriesSkippedDuplicate": self.skipped_duplicate_count,
                "issues": self.issue_count,
            },
            "restoredDerived": self.restored_derived.iter().map(BackupRestoredDerivedAssetReport::data_json).collect::<Vec<_>>(),
            "nextActions": self.next_actions,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let prefix = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{prefix}backup restore {status}: {backup_id}\n  side path: {side_path}\n  restored db: {database}\n  imported memories: {imported} (duplicates: {duplicates})\n",
            status = self.status,
            backup_id = self.backup_id,
            side_path = self.side_path,
            database = self.restored_database_path,
            imported = self.imported_memory_count,
            duplicates = self.skipped_duplicate_count,
        )
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "BACKUP_RESTORE|{}|{}|{}|{}",
            self.backup_id, self.status, self.imported_memory_count, self.issue_count
        )
    }
}

/// One derived asset materialized during `ee backup restore`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupRestoredDerivedAssetReport {
    pub path: String,
    pub kind: String,
    pub restore_path: String,
    pub lab_episode_path: Option<String>,
}

impl BackupRestoredDerivedAssetReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "path": self.path,
            "kind": self.kind,
            "restorePath": self.restore_path,
            "labEpisodePath": self.lab_episode_path,
        })
    }
}

impl BackupVerifyReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "backup verify",
            "backupId": self.backup_id,
            "status": self.status,
            "backupPath": self.backup_path,
            "manifestPath": self.manifest_path,
            "manifestHash": self.manifest_hash,
            "checkedArtifacts": self.checked_artifacts.iter().map(BackupArtifactReport::data_json).collect::<Vec<_>>(),
            "checkedDerived": self.checked_derived.iter().map(BackupDerivedAssetReport::data_json).collect::<Vec<_>>(),
            "issues": self.issues.iter().map(BackupVerificationIssue::data_json).collect::<Vec<_>>(),
        })
    }
}

/// One artifact described by a backup manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupArtifactReport {
    pub path: String,
    pub kind: String,
    pub hash: Option<String>,
    pub size_bytes: Option<u64>,
    pub required: bool,
}

impl BackupArtifactReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "path": self.path,
            "kind": self.kind,
            "hash": self.hash,
            "sizeBytes": self.size_bytes,
            "required": self.required,
        })
    }
}

/// One optional derived asset captured in a backup manifest v2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupDerivedAssetReport {
    pub path: String,
    pub kind: String,
    pub hash: Option<String>,
    pub byte_size: Option<u64>,
    pub captured_at: Option<String>,
    pub episode_id_if_lab: Option<String>,
}

impl BackupDerivedAssetReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "path": self.path,
            "kind": self.kind,
            "hash": self.hash,
            "byteSize": self.byte_size,
            "capturedAt": self.captured_at,
            "episodeIdIfLab": self.episode_id_if_lab,
        })
    }

    #[must_use]
    pub fn manifest_json(&self) -> JsonValue {
        json!({
            "path": self.path,
            "kind": self.kind,
            "hash": self.hash,
            "byte_size": self.byte_size,
            "captured_at": self.captured_at,
            "episode_id_if_lab": self.episode_id_if_lab,
        })
    }
}

/// Honest degradation metadata for assets this slice cannot yet include.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub next_action: String,
}

impl BackupDegradation {
    fn with_severity(
        code: impl Into<String>,
        severity: impl Into<String>,
        message: impl Into<String>,
        next_action: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity: severity.into(),
            message: message.into(),
            next_action: next_action.into(),
        }
    }

    fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        next_action: impl Into<String>,
    ) -> Self {
        Self::with_severity(code, "warning", message, next_action)
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "nextAction": self.next_action,
        })
    }
}

struct BackupExportData {
    workspace: ExportWorkspaceRecord,
    memories: Vec<StoredMemory>,
    tags_by_memory: BTreeMap<String, Vec<String>>,
    links: Vec<StoredMemoryLink>,
    audits: Vec<StoredAuditEntry>,
}

struct BackupDerivedPayload {
    report: BackupDerivedAssetReport,
    bytes: Vec<u8>,
}

/// Create a verified backup directory with redacted JSONL records and a manifest.
///
/// # Errors
///
/// Returns a [`DomainError`] if the workspace database cannot be read or if any
/// backup artifact cannot be created without overwriting existing data.
pub fn create_backup(options: &BackupCreateOptions) -> Result<BackupCreateReport, DomainError> {
    let workspace_path = normalize_path(&options.workspace_path);
    let database_path = database_path(options, &workspace_path);
    if !database_path.is_file() {
        return Err(DomainError::Storage {
            message: format!("database file '{}' does not exist", database_path.display()),
            repair: Some("ee init --workspace .".to_owned()),
        });
    }

    let connection =
        DbConnection::open(DatabaseConfig::file(database_path.clone())).map_err(|error| {
            DomainError::Storage {
                message: error.to_string(),
                repair: Some("ee init --workspace . && ee db migrate --workspace .".to_owned()),
            }
        })?;
    let workspace = load_workspace(&connection, &workspace_path)?;
    let export_data = load_export_data(&connection, workspace)?;
    let backup_id = BackupId::now().to_string();
    let backup_root = backup_root(options, &workspace_path);
    let backup_path = backup_root.join(&backup_id);
    let records_path = backup_path.join(RECORDS_FILE);
    let manifest_path = backup_path.join(MANIFEST_FILE);
    let created_at = Utc::now().to_rfc3339();
    let mut degraded = backup_degradations(&workspace_path, options.include_derived);
    degraded.extend(redaction_pattern_degradations(
        &export_data,
        options.redaction_level,
    ));
    let derived_payloads = if options.include_derived {
        collect_derived_payloads(
            &connection,
            &workspace_path,
            &export_data.workspace.workspace_id,
            &created_at,
            &mut degraded,
        )
    } else {
        Vec::new()
    };
    let derived_reports = derived_payloads
        .iter()
        .map(|payload| payload.report.clone())
        .collect::<Vec<_>>();

    let (records_bytes, stats) = render_records(
        &backup_id,
        &created_at,
        options.redaction_level,
        &export_data,
    )?;

    let planned_records_artifact = BackupArtifactReport {
        path: RECORDS_FILE.to_owned(),
        kind: "jsonl_export".to_owned(),
        hash: if options.dry_run {
            None
        } else {
            Some(hash_bytes(&records_bytes))
        },
        size_bytes: if options.dry_run {
            None
        } else {
            Some(records_bytes.len() as u64)
        },
        required: true,
    };

    let mut report = BackupCreateReport {
        schema: BACKUP_CREATE_SCHEMA_V1,
        backup_id: backup_id.clone(),
        label: normalized_label(options.label.as_deref()),
        status: if options.dry_run {
            "dry_run".to_owned()
        } else {
            "completed".to_owned()
        },
        dry_run: options.dry_run,
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        workspace_id: export_data.workspace.workspace_id.clone(),
        database_path: database_path.to_string_lossy().into_owned(),
        backup_path: backup_path.to_string_lossy().into_owned(),
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        records_path: records_path.to_string_lossy().into_owned(),
        manifest_hash: None,
        records_hash: planned_records_artifact.hash.clone(),
        redaction_level: options.redaction_level,
        export_scope: ExportScope::All,
        include_derived: options.include_derived,
        total_records: stats.total_records,
        memory_count: stats.memory_count,
        link_count: stats.link_count,
        tag_count: stats.tag_count,
        audit_count: stats.audit_count,
        verification_status: if options.dry_run {
            "not_checked".to_owned()
        } else {
            "verified".to_owned()
        },
        artifacts: vec![planned_records_artifact],
        derived: derived_reports,
        degraded,
    };

    let manifest_json = manifest_json(&report, &created_at, None);
    if options.dry_run {
        report.artifacts.push(BackupArtifactReport {
            path: MANIFEST_FILE.to_owned(),
            kind: "manifest".to_owned(),
            hash: None,
            size_bytes: None,
            required: true,
        });
        return Ok(report);
    }

    ensure_backup_directory(&backup_root, &backup_path)?;
    write_new_file(&records_path, &records_bytes)?;
    for payload in &derived_payloads {
        write_new_relative_file(&backup_path, &payload.report.path, &payload.bytes)?;
        tracing::info!(
            target: "ee::backup",
            event = "backup_create_derived_included",
            backup_id = %backup_id,
            kind = %payload.report.kind,
            path = %payload.report.path,
            hash = %payload.report.hash.as_deref().unwrap_or("unknown"),
            byte_size = payload.report.byte_size.unwrap_or(0),
            episode_id_if_lab = %payload.report.episode_id_if_lab.as_deref().unwrap_or(""),
            "backup derived asset included"
        );
    }
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest_json).map_err(|error| DomainError::Storage {
            message: format!("failed to render backup manifest JSON: {error}"),
            repair: Some("retry backup creation with a new label or output directory".to_owned()),
        })?;
    let mut manifest_bytes_with_newline = manifest_bytes;
    manifest_bytes_with_newline.push(b'\n');
    write_new_file(&manifest_path, &manifest_bytes_with_newline)?;

    let records_hash = hash_file(&records_path)?;
    let manifest_hash = hash_file(&manifest_path)?;
    let records_size = file_size(&records_path)?;
    let manifest_size = file_size(&manifest_path)?;

    report.records_hash = Some(records_hash.clone());
    report.manifest_hash = Some(manifest_hash.clone());
    report.artifacts = vec![
        BackupArtifactReport {
            path: RECORDS_FILE.to_owned(),
            kind: "jsonl_export".to_owned(),
            hash: Some(records_hash),
            size_bytes: Some(records_size),
            required: true,
        },
        BackupArtifactReport {
            path: MANIFEST_FILE.to_owned(),
            kind: "manifest".to_owned(),
            hash: Some(manifest_hash),
            size_bytes: Some(manifest_size),
            required: true,
        },
    ];

    Ok(report)
}

/// List backup manifests under a backup root.
///
/// # Errors
///
/// Returns a [`DomainError`] if the backup root exists but cannot be read.
pub fn list_backups(options: &BackupListOptions) -> Result<BackupListReport, DomainError> {
    let workspace_path = normalize_path(&options.workspace_path);
    let backup_root = backup_root_from(options.output_dir.as_deref(), &workspace_path);
    let mut degraded = Vec::new();
    let mut backups = Vec::new();

    if backup_root.exists() {
        if !backup_root.is_dir() {
            return Err(DomainError::Storage {
                message: format!("backup root '{}' is not a directory", backup_root.display()),
                repair: Some("choose a directory with --output-dir".to_owned()),
            });
        }

        let mut backup_paths = fs::read_dir(&backup_root)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to list backup root '{}': {error}",
                    backup_root.display()
                ),
                repair: Some("choose a readable --output-dir".to_owned()),
            })?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to read backup root '{}': {error}",
                    backup_root.display()
                ),
                repair: Some("choose a readable --output-dir".to_owned()),
            })?;
        backup_paths.sort();

        for backup_path in backup_paths.into_iter().filter(|path| path.is_dir()) {
            let manifest_path = backup_path.join(MANIFEST_FILE);
            if !manifest_path.is_file() {
                degraded.push(BackupDegradation::warning(
                    "backup_manifest_missing",
                    format!(
                        "backup directory '{}' has no manifest.json",
                        backup_path.display()
                    ),
                    "run ee backup inspect on the directory or remove it manually after review",
                ));
                continue;
            }

            match inspect_backup(&BackupInspectOptions {
                backup_path: backup_path.clone(),
            }) {
                Ok(report) => backups.push(BackupListEntry {
                    backup_id: report.backup_id,
                    label: report.label,
                    created_at: report.created_at,
                    backup_path: report.backup_path,
                    manifest_path: report.manifest_path,
                    manifest_hash: report.manifest_hash,
                    verification_status: report.verification_status,
                    issue_count: report.issues.len(),
                }),
                Err(error) => degraded.push(BackupDegradation::warning(
                    "backup_manifest_unreadable",
                    format!(
                        "backup directory '{}' could not be inspected: {}",
                        backup_path.display(),
                        error.message()
                    ),
                    "run ee backup inspect on the directory for a focused diagnostic",
                )),
            }
        }
    }

    backups.sort_by(|left, right| left.backup_id.cmp(&right.backup_id));
    Ok(BackupListReport {
        schema: BACKUP_LIST_SCHEMA_V1,
        backup_root: backup_root.to_string_lossy().into_owned(),
        backups,
        degraded,
    })
}

/// Inspect one backup manifest without checking artifact hashes.
///
/// # Errors
///
/// Returns a [`DomainError`] if the manifest cannot be read or parsed as JSON.
pub fn inspect_backup(options: &BackupInspectOptions) -> Result<BackupInspectReport, DomainError> {
    let backup_path = normalize_path(&options.backup_path);
    let manifest_path = backup_path.join(MANIFEST_FILE);
    if backup_relative_path_has_symlink_component(&backup_path, Path::new(MANIFEST_FILE))? {
        return Err(DomainError::Storage {
            message: format!(
                "backup manifest path '{}' traverses a symbolic link",
                manifest_path.display()
            ),
            repair: Some("choose a self-contained backup directory".to_owned()),
        });
    }
    if !manifest_path.is_file() {
        return Err(DomainError::NotFound {
            resource: "backup manifest".to_owned(),
            id: manifest_path.to_string_lossy().into_owned(),
            repair: Some("choose a backup directory containing manifest.json".to_owned()),
        });
    }

    let manifest_bytes = fs::read(&manifest_path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to read backup manifest '{}': {error}",
            manifest_path.display()
        ),
        repair: Some("inspect filesystem permissions and retry".to_owned()),
    })?;
    let manifest_hash = hash_bytes(&manifest_bytes);
    let manifest = serde_json::from_slice::<JsonValue>(&manifest_bytes).map_err(|error| {
        DomainError::Storage {
            message: format!(
                "failed to parse backup manifest '{}': {error}",
                manifest_path.display()
            ),
            repair: Some("restore from another backup or recreate this backup".to_owned()),
        }
    })?;

    Ok(inspect_manifest(
        &backup_path,
        &manifest_path,
        &manifest_hash,
        &manifest,
    ))
}

/// Verify one backup manifest and all required artifacts it references.
///
/// # Errors
///
/// Returns a [`DomainError`] if the manifest cannot be inspected.
pub fn verify_backup(options: &BackupVerifyOptions) -> Result<BackupVerifyReport, DomainError> {
    let backup_path = normalize_path(&options.backup_path);
    let inspect = inspect_backup(&BackupInspectOptions {
        backup_path: backup_path.clone(),
    })?;
    let mut issues = inspect.issues;
    let mut checked_artifacts = Vec::new();
    let mut checked_derived = Vec::new();

    for artifact in &inspect.artifacts {
        let Some(path) = safe_artifact_path(&backup_path, &artifact.path, &mut issues) else {
            continue;
        };
        if !path.is_file() {
            issues.push(
                BackupVerificationIssue::error(
                    "artifact_missing",
                    "required backup artifact is missing",
                )
                .with_path(artifact.path.clone()),
            );
            continue;
        }

        let actual_size = file_size(&path)?;
        if let Some(expected_size) = artifact.size_bytes
            && actual_size != expected_size
        {
            issues.push(
                BackupVerificationIssue::error(
                    "artifact_size_mismatch",
                    "backup artifact size does not match manifest",
                )
                .with_path(artifact.path.clone())
                .with_expected_actual(expected_size.to_string(), actual_size.to_string()),
            );
        }

        let actual_hash = hash_file(&path)?;
        if let Some(expected_hash) = &artifact.hash
            && &actual_hash != expected_hash
        {
            issues.push(
                BackupVerificationIssue::error(
                    "artifact_hash_mismatch",
                    "backup artifact hash does not match manifest",
                )
                .with_path(artifact.path.clone())
                .with_expected_actual(expected_hash.clone(), actual_hash.clone()),
            );
        }

        checked_artifacts.push(BackupArtifactReport {
            path: artifact.path.clone(),
            kind: artifact.kind.clone(),
            hash: Some(actual_hash),
            size_bytes: Some(actual_size),
            required: artifact.required,
        });
    }

    for derived in &inspect.derived {
        let Some(path) = safe_artifact_path(&backup_path, &derived.path, &mut issues) else {
            continue;
        };
        if !path.is_file() {
            issues.push(
                BackupVerificationIssue::high(
                    "derived_asset_missing",
                    "derived backup asset is missing",
                )
                .with_path(derived.path.clone()),
            );
            continue;
        }

        let actual_size = file_size(&path)?;
        if let Some(expected_size) = derived.byte_size
            && actual_size != expected_size
        {
            tracing::warn!(
                target: "ee::backup",
                event = "backup_derived_corrupt",
                kind = %derived.kind,
                path = %derived.path,
                mismatch = "byte_size",
                expected = expected_size,
                observed = actual_size,
                "backup derived asset byte size mismatch"
            );
            issues.push(
                BackupVerificationIssue::high(
                    "derived_asset_corrupt",
                    "derived backup asset size does not match manifest",
                )
                .with_path(derived.path.clone())
                .with_expected_actual(expected_size.to_string(), actual_size.to_string()),
            );
        }

        let actual_hash = hash_file(&path)?;
        if let Some(expected_hash) = &derived.hash
            && &actual_hash != expected_hash
        {
            tracing::warn!(
                target: "ee::backup",
                event = "backup_derived_corrupt",
                kind = %derived.kind,
                path = %derived.path,
                mismatch = "hash",
                expected_hash = %expected_hash,
                observed_hash = %actual_hash,
                "backup derived asset hash mismatch"
            );
            issues.push(
                BackupVerificationIssue::high(
                    "derived_asset_corrupt",
                    "derived backup asset hash does not match manifest",
                )
                .with_path(derived.path.clone())
                .with_expected_actual(expected_hash.clone(), actual_hash.clone()),
            );
        }

        if derived.kind == "wal_holds" {
            inspect_wal_holds_for_orphans(&path, &derived.path, &mut issues);
        }

        checked_derived.push(BackupDerivedAssetReport {
            path: derived.path.clone(),
            kind: derived.kind.clone(),
            hash: Some(actual_hash),
            byte_size: Some(actual_size),
            captured_at: derived.captured_at.clone(),
            episode_id_if_lab: derived.episode_id_if_lab.clone(),
        });
    }

    let status = if issues.iter().any(backup_verification_issue_is_blocking) {
        "failed"
    } else if issues.is_empty() {
        "verified"
    } else {
        "degraded"
    };
    Ok(BackupVerifyReport {
        schema: BACKUP_VERIFY_SCHEMA_V1,
        backup_id: inspect.backup_id,
        status: status.to_owned(),
        backup_path: inspect.backup_path,
        manifest_path: inspect.manifest_path,
        manifest_hash: inspect.manifest_hash,
        checked_artifacts,
        checked_derived,
        issues,
    })
}

/// Restore one verified backup into an isolated side path.
///
/// # Errors
///
/// Returns a [`DomainError`] if the backup cannot be verified, the side path is
/// not isolated, or JSONL records cannot be imported into the restored database.
pub fn restore_backup_to_side_path(
    options: &BackupRestoreOptions,
) -> Result<BackupRestoreReport, DomainError> {
    let workspace_path = normalize_path(&options.workspace_path);
    let backup_path = normalize_path(&options.backup_path);
    let side_path = normalize_path(&options.side_path);
    if workspace_path == side_path {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "side path '{}' must differ from source workspace '{}'",
                side_path.display(),
                workspace_path.display()
            ),
            repair: Some("choose a separate --side-path target".to_owned()),
        });
    }

    let inspect = inspect_backup(&BackupInspectOptions {
        backup_path: backup_path.clone(),
    })?;
    let verify = verify_backup(&BackupVerifyOptions {
        backup_path: backup_path.clone(),
    })?;
    if verify
        .issues
        .iter()
        .any(backup_verification_issue_is_blocking)
    {
        return Err(DomainError::Import {
            message: format!(
                "backup '{}' failed integrity verification with {} issue(s)",
                inspect.backup_id,
                verify.issues.len()
            ),
            repair: Some("run ee backup verify <id-or-path> --json and repair issues".to_owned()),
        });
    }

    let source_records_path = backup_artifact_path(&backup_path, &inspect, RECORDS_FILE)?;
    let source_manifest_path = backup_path.join(MANIFEST_FILE);
    let restore_artifact_dir = side_path
        .join(WORKSPACE_MARKER)
        .join(DEFAULT_RESTORE_DIR)
        .join(&inspect.backup_id);
    let restore_records_path = restore_artifact_dir.join(RECORDS_FILE);
    let restore_manifest_path = restore_artifact_dir.join(MANIFEST_FILE);
    let restored_database_path = side_path.join(WORKSPACE_MARKER).join(DEFAULT_DB_FILE);
    let next_actions = vec![
        format!("ee backup inspect {} --json", inspect.backup_id),
        format!(
            "ee search \"<query>\" --workspace {} --json",
            side_path.to_string_lossy()
        ),
    ];

    if options.dry_run {
        return Ok(BackupRestoreReport {
            schema: BACKUP_RESTORE_SCHEMA_V1,
            backup_id: inspect.backup_id,
            status: "dry_run".to_owned(),
            dry_run: true,
            backup_path: backup_path.to_string_lossy().into_owned(),
            side_path: side_path.to_string_lossy().into_owned(),
            restore_artifact_dir: restore_artifact_dir.to_string_lossy().into_owned(),
            source_manifest_path: source_manifest_path.to_string_lossy().into_owned(),
            source_records_path: source_records_path.to_string_lossy().into_owned(),
            source_manifest_hash: inspect.manifest_hash,
            restored_database_path: restored_database_path.to_string_lossy().into_owned(),
            import_status: "dry_run".to_owned(),
            imported_memory_count: 0,
            skipped_duplicate_count: 0,
            restored_derived: Vec::new(),
            issue_count: u32::try_from(verify.issues.len()).unwrap_or(u32::MAX),
            next_actions,
        });
    }

    ensure_side_path_is_isolated(&side_path)?;
    fs::create_dir_all(&restore_artifact_dir).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to create restore artifact directory '{}': {error}",
            restore_artifact_dir.display()
        ),
        repair: Some("choose a writable --side-path".to_owned()),
    })?;

    let manifest_bytes = fs::read(&source_manifest_path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to read backup manifest '{}': {error}",
            source_manifest_path.display()
        ),
        repair: Some("verify the backup directory and retry restore".to_owned()),
    })?;
    write_new_file(&restore_manifest_path, &manifest_bytes)?;

    let records_bytes = fs::read(&source_records_path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to read backup records '{}': {error}",
            source_records_path.display()
        ),
        repair: Some("verify the backup records artifact and retry restore".to_owned()),
    })?;
    write_new_file(&restore_records_path, &records_bytes)?;
    let restored_derived = copy_derived_artifacts_to_restore(
        &backup_path,
        &restore_artifact_dir,
        &side_path,
        &inspect,
    )?;

    let import_report = import_jsonl_records(&JsonlImportOptions {
        workspace_path: side_path.clone(),
        database_path: Some(restored_database_path.clone()),
        source_path: restore_records_path,
        dry_run: false,
    })
    .map_err(|error| DomainError::Import {
        message: format!(
            "failed importing backup '{}' records into side path '{}': {error}",
            inspect.backup_id,
            side_path.display()
        ),
        repair: Some(
            "inspect the copied records.jsonl and retry with a fresh --side-path".to_owned(),
        ),
    })?;
    let restore_issue_count = import_report
        .issues
        .len()
        .saturating_add(verify.issues.len());
    let restore_status = if import_report.status == "completed" && verify.issues.is_empty() {
        "completed"
    } else {
        "degraded"
    };

    Ok(BackupRestoreReport {
        schema: BACKUP_RESTORE_SCHEMA_V1,
        backup_id: inspect.backup_id,
        status: restore_status.to_owned(),
        dry_run: false,
        backup_path: backup_path.to_string_lossy().into_owned(),
        side_path: side_path.to_string_lossy().into_owned(),
        restore_artifact_dir: restore_artifact_dir.to_string_lossy().into_owned(),
        source_manifest_path: source_manifest_path.to_string_lossy().into_owned(),
        source_records_path: source_records_path.to_string_lossy().into_owned(),
        source_manifest_hash: inspect.manifest_hash,
        restored_database_path: restored_database_path.to_string_lossy().into_owned(),
        import_status: import_report.status.clone(),
        imported_memory_count: import_report.memories_imported,
        skipped_duplicate_count: import_report.memories_skipped_duplicate,
        restored_derived,
        issue_count: u32::try_from(restore_issue_count).unwrap_or(u32::MAX),
        next_actions,
    })
}

fn backup_artifact_path(
    backup_path: &Path,
    inspect: &BackupInspectReport,
    expected_path: &str,
) -> Result<PathBuf, DomainError> {
    let artifact = inspect
        .artifacts
        .iter()
        .find(|artifact| artifact.path == expected_path)
        .ok_or_else(|| DomainError::Import {
            message: format!(
                "backup '{}' is missing required artifact '{}'",
                inspect.backup_id, expected_path
            ),
            repair: Some("recreate the backup using ee backup create".to_owned()),
        })?;

    let mut issues = Vec::new();
    let Some(path) = safe_artifact_path(backup_path, &artifact.path, &mut issues) else {
        let message = issues
            .first()
            .map(|issue| issue.message.clone())
            .unwrap_or_else(|| "backup artifact path is invalid".to_owned());
        return Err(DomainError::Import {
            message,
            repair: Some("recreate the backup in a safe filesystem path".to_owned()),
        });
    };
    Ok(path)
}

fn backup_verification_issue_is_blocking(issue: &BackupVerificationIssue) -> bool {
    matches!(issue.severity.as_str(), "error" | "high" | "critical")
}

fn copy_derived_artifacts_to_restore(
    backup_path: &Path,
    restore_artifact_dir: &Path,
    side_path: &Path,
    inspect: &BackupInspectReport,
) -> Result<Vec<BackupRestoredDerivedAssetReport>, DomainError> {
    let mut restored = Vec::new();
    for derived in &inspect.derived {
        let mut issues = Vec::new();
        let Some(source_path) = safe_artifact_path(backup_path, &derived.path, &mut issues) else {
            let message = issues
                .first()
                .map(|issue| issue.message.clone())
                .unwrap_or_else(|| "derived backup artifact path is invalid".to_owned());
            return Err(DomainError::Import {
                message,
                repair: Some("recreate the backup in a safe filesystem path".to_owned()),
            });
        };
        let bytes = fs::read(&source_path).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to read derived backup asset '{}': {error}",
                source_path.display()
            ),
            repair: Some("verify the backup directory and retry restore".to_owned()),
        })?;
        let observed_hash = hash_bytes(&bytes);
        let expected_hash = derived.hash.as_deref().unwrap_or("unknown");
        let validation_status = if derived.hash.as_deref() == Some(observed_hash.as_str()) {
            "valid"
        } else {
            "mismatch"
        };
        tracing::info!(
            target: "ee::backup",
            event = "backup_restore_derived_validation",
            kind = %derived.kind,
            path = %derived.path,
            expected_hash = %expected_hash,
            observed_hash = %observed_hash,
            status = validation_status,
            "backup restore derived asset validation observed"
        );
        let restore_path = write_new_relative_file(restore_artifact_dir, &derived.path, &bytes)?;
        let lab_episode_path = if derived.kind == "lab_episode"
            && derived.path.starts_with("derived/lab/episode_files/")
        {
            Some(restore_lab_episode_file(side_path, &derived.path, &bytes)?)
        } else {
            None
        };
        restored.push(BackupRestoredDerivedAssetReport {
            path: derived.path.clone(),
            kind: derived.kind.clone(),
            restore_path: restore_path.to_string_lossy().into_owned(),
            lab_episode_path: lab_episode_path.map(|path| path.to_string_lossy().into_owned()),
        });
    }
    Ok(restored)
}

fn restore_lab_episode_file(
    side_path: &Path,
    backup_relative_path: &str,
    bytes: &[u8],
) -> Result<PathBuf, DomainError> {
    let Some(file_name) = Path::new(backup_relative_path)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Err(DomainError::Storage {
            message: format!("derived lab episode path '{backup_relative_path}' has no file name"),
            repair: Some("recreate the backup with valid lab episode artifact paths".to_owned()),
        });
    };
    let lab_episode_dir = side_path
        .join(WORKSPACE_MARKER)
        .join("lab")
        .join("episodes");
    fs::create_dir_all(&lab_episode_dir).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to create restored lab episode directory '{}': {error}",
            lab_episode_dir.display()
        ),
        repair: Some("choose a writable --side-path".to_owned()),
    })?;
    let restored_path = lab_episode_dir.join(safe_file_stem(file_name));
    write_new_file(&restored_path, bytes)?;
    Ok(restored_path)
}

fn inspect_wal_holds_for_orphans(
    path: &Path,
    manifest_path: &str,
    issues: &mut Vec<BackupVerificationIssue>,
) {
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let Ok(value) = serde_json::from_slice::<JsonValue>(&bytes) else {
        return;
    };
    let present = value
        .get("present")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let row_count = value
        .get("rowCount")
        .and_then(JsonValue::as_i64)
        .unwrap_or(0);
    if present && row_count > 0 {
        tracing::warn!(
            target: "ee::backup",
            event = "backup_wal_holds_orphaned_after_restore",
            path = %manifest_path,
            held_lsn = "unknown",
            row_count,
            reachable_in_snapshot = false,
            "backup WAL hold state is orphaned for restore replay"
        );
        issues.push(
            BackupVerificationIssue::warning(
                "wal_holds_orphaned",
                "backup contains WAL hold state that must not be replayed into a restore side path",
            )
            .with_path(manifest_path.to_owned())
            .with_expected_actual("0", row_count.to_string()),
        );
    }
}

fn inspect_manifest(
    backup_path: &Path,
    manifest_path: &Path,
    manifest_hash: &str,
    manifest: &JsonValue,
) -> BackupInspectReport {
    let mut issues = Vec::new();
    let manifest_schema = json_string(manifest, "schema");
    if !backup_manifest_schema_supported(manifest_schema.as_deref()) {
        issues.push(
            BackupVerificationIssue::error(
                "manifest_schema_mismatch",
                "backup manifest schema is missing or unsupported",
            )
            .with_expected_actual(
                format!("{BACKUP_MANIFEST_SCHEMA_V1} or {BACKUP_MANIFEST_SCHEMA_V2}"),
                manifest_schema.unwrap_or_else(|| "<missing>".to_owned()),
            ),
        );
    }

    let backup_id = json_string(manifest, "backupId").unwrap_or_else(|| {
        issues.push(BackupVerificationIssue::error(
            "backup_id_missing",
            "backup manifest does not include a backupId",
        ));
        backup_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unknown>")
            .to_owned()
    });
    let workspace = manifest.get("workspace").unwrap_or(&JsonValue::Null);
    let verification = manifest.get("verification").unwrap_or(&JsonValue::Null);
    let artifacts = artifact_reports(manifest, &mut issues);
    let derived = derived_asset_reports(manifest, &mut issues);
    if !derived.is_empty() {
        let kinds = derived
            .iter()
            .map(|asset| asset.kind.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(",");
        let total_byte_size = derived
            .iter()
            .filter_map(|asset| asset.byte_size)
            .sum::<u64>();
        tracing::info!(
            target: "ee::backup",
            event = "backup_inspect_derived_summary",
            backup_id = %backup_id,
            derived_count = derived.len(),
            kinds = %kinds,
            total_byte_size,
            "backup manifest derived asset summary inspected"
        );
    }

    BackupInspectReport {
        schema: BACKUP_INSPECT_SCHEMA_V1,
        backup_id,
        label: json_string(manifest, "label"),
        created_at: json_string(manifest, "createdAt"),
        ee_version: json_string(manifest, "eeVersion"),
        backup_path: backup_path.to_string_lossy().into_owned(),
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        manifest_hash: manifest_hash.to_owned(),
        workspace_id: json_string(workspace, "id"),
        workspace_path: json_string(workspace, "path"),
        database_path: json_string(manifest, "databasePath"),
        redaction_level: json_string(manifest, "redactionLevel"),
        export_scope: json_string(manifest, "exportScope"),
        counts: backup_counts(manifest.get("counts").unwrap_or(&JsonValue::Null)),
        verification_status: json_string(verification, "status"),
        artifacts,
        derived,
        degraded: degradation_reports(manifest),
        issues,
    }
}

fn backup_manifest_schema_supported(schema: Option<&str>) -> bool {
    matches!(
        schema,
        Some(BACKUP_MANIFEST_SCHEMA_V1 | BACKUP_MANIFEST_SCHEMA_V2)
    )
}

fn json_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn backup_counts(value: &JsonValue) -> BackupCounts {
    BackupCounts {
        total_records: json_u64(value, "totalRecords"),
        memory_count: json_u64(value, "memoryRecords"),
        link_count: json_u64(value, "linkRecords"),
        tag_count: json_u64(value, "tagRecords"),
        audit_count: json_u64(value, "auditRecords"),
    }
}

fn json_u64(value: &JsonValue, key: &str) -> u64 {
    value.get(key).and_then(JsonValue::as_u64).unwrap_or(0)
}

fn json_bool(value: &JsonValue, key: &str) -> bool {
    value.get(key).and_then(JsonValue::as_bool).unwrap_or(false)
}

fn artifact_reports(
    manifest: &JsonValue,
    issues: &mut Vec<BackupVerificationIssue>,
) -> Vec<BackupArtifactReport> {
    let Some(artifacts) = manifest.get("artifacts").and_then(JsonValue::as_array) else {
        issues.push(BackupVerificationIssue::error(
            "manifest_artifacts_missing",
            "backup manifest does not include an artifacts array",
        ));
        return Vec::new();
    };

    artifacts
        .iter()
        .enumerate()
        .filter_map(|(index, artifact)| {
            let Some(path) = json_string(artifact, "path") else {
                issues.push(BackupVerificationIssue::error(
                    "artifact_path_missing",
                    format!("artifact entry {index} does not include a path"),
                ));
                return None;
            };
            Some(BackupArtifactReport {
                path,
                kind: json_string(artifact, "kind").unwrap_or_else(|| "unknown".to_owned()),
                hash: json_string(artifact, "hash"),
                size_bytes: artifact.get("sizeBytes").and_then(JsonValue::as_u64),
                required: json_bool(artifact, "required"),
            })
        })
        .collect()
}

fn derived_asset_reports(
    manifest: &JsonValue,
    issues: &mut Vec<BackupVerificationIssue>,
) -> Vec<BackupDerivedAssetReport> {
    let Some(derived) = manifest.get("derived") else {
        return Vec::new();
    };
    let Some(derived) = derived.as_array() else {
        issues.push(BackupVerificationIssue::error(
            "manifest_derived_invalid",
            "backup manifest derived field must be an array",
        ));
        return Vec::new();
    };

    derived
        .iter()
        .enumerate()
        .filter_map(|(index, asset)| {
            let Some(path) = json_string(asset, "path") else {
                issues.push(BackupVerificationIssue::error(
                    "derived_asset_path_missing",
                    format!("derived asset entry {index} does not include a path"),
                ));
                return None;
            };
            Some(BackupDerivedAssetReport {
                path,
                kind: json_string(asset, "kind").unwrap_or_else(|| "unknown".to_owned()),
                hash: json_string(asset, "hash"),
                byte_size: asset.get("byte_size").and_then(JsonValue::as_u64),
                captured_at: json_string(asset, "captured_at"),
                episode_id_if_lab: json_string(asset, "episode_id_if_lab"),
            })
        })
        .collect()
}

fn degradation_reports(manifest: &JsonValue) -> Vec<BackupDegradation> {
    manifest
        .get("degraded")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flat_map(|items| items.iter())
        .map(|item| BackupDegradation {
            code: json_string(item, "code").unwrap_or_else(|| "unknown".to_owned()),
            severity: json_string(item, "severity").unwrap_or_else(|| "warning".to_owned()),
            message: json_string(item, "message").unwrap_or_default(),
            next_action: json_string(item, "nextAction").unwrap_or_default(),
        })
        .collect()
}

fn safe_artifact_path(
    backup_path: &Path,
    artifact_path: &str,
    issues: &mut Vec<BackupVerificationIssue>,
) -> Option<PathBuf> {
    let trimmed = artifact_path.trim();
    let relative = Path::new(artifact_path);
    if trimmed.is_empty()
        || trimmed != artifact_path
        || relative.is_absolute()
        || artifact_path
            .chars()
            .any(|ch| ch == '\\' || ch == ':' || ch.is_control())
    {
        issues.push(
            BackupVerificationIssue::error(
                "artifact_path_outside_backup",
                "backup artifact path is empty, absolute, nonportable, or escapes the backup directory",
            )
            .with_path(artifact_path.to_owned()),
        );
        return None;
    }

    let mut has_normal_component = false;
    for component in relative.components() {
        match component {
            Component::Normal(_) => has_normal_component = true,
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                issues.push(
                    BackupVerificationIssue::error(
                        "artifact_path_outside_backup",
                        "backup artifact path is empty, absolute, nonportable, or escapes the backup directory",
                    )
                    .with_path(artifact_path.to_owned()),
                );
                return None;
            }
        }
    }
    if !has_normal_component {
        issues.push(
            BackupVerificationIssue::error(
                "artifact_path_outside_backup",
                "backup artifact path is empty, absolute, nonportable, or escapes the backup directory",
            )
            .with_path(artifact_path.to_owned()),
        );
        return None;
    }

    match backup_relative_path_has_symlink_component(backup_path, relative) {
        Ok(true) => {
            issues.push(
                BackupVerificationIssue::error(
                    "artifact_path_symlink",
                    "backup artifact path traverses a symbolic link",
                )
                .with_path(artifact_path.to_owned()),
            );
            return None;
        }
        Ok(false) => {}
        Err(error) => {
            issues.push(
                BackupVerificationIssue::error("artifact_path_unreadable", error.message())
                    .with_path(artifact_path.to_owned()),
            );
            return None;
        }
    }

    Some(backup_path.join(relative))
}

fn backup_relative_path_has_symlink_component(
    root: &Path,
    relative: &Path,
) -> Result<bool, DomainError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => {
                current.push(segment);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                    Err(error) => {
                        return Err(DomainError::Storage {
                            message: format!(
                                "failed to inspect backup path '{}': {error}",
                                current.display()
                            ),
                            repair: Some("inspect filesystem permissions and retry".to_owned()),
                        });
                    }
                }
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return Ok(true),
        }
    }
    Ok(false)
}

fn load_workspace(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<crate::db::StoredWorkspace, DomainError> {
    let path = workspace_path.to_string_lossy();
    connection
        .get_workspace_by_path(&path)
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee init --workspace . && ee db migrate --workspace .".to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "workspace".to_owned(),
            id: path.into_owned(),
            repair: Some("ee init --workspace .".to_owned()),
        })
}

fn load_export_data(
    connection: &DbConnection,
    workspace: crate::db::StoredWorkspace,
) -> Result<BackupExportData, DomainError> {
    let memories = connection
        .list_memories(&workspace.id, None, true)
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee db check --workspace .".to_owned()),
        })?;
    let memory_ids = memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<BTreeSet<_>>();
    let mut tags_by_memory = BTreeMap::new();
    for memory in &memories {
        let tags =
            connection
                .get_memory_tags(&memory.id)
                .map_err(|error| DomainError::Storage {
                    message: error.to_string(),
                    repair: Some("ee db check --workspace .".to_owned()),
                })?;
        tags_by_memory.insert(memory.id.clone(), tags);
    }
    let links = connection
        .list_all_memory_links(None)
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee db check --workspace .".to_owned()),
        })?
        .into_iter()
        .filter(|link| {
            memory_ids.contains(&link.src_memory_id) && memory_ids.contains(&link.dst_memory_id)
        })
        .collect();
    let audits = connection
        .list_audit_entries(Some(&workspace.id), None)
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee db check --workspace .".to_owned()),
        })?;

    let mut workspace_builder = ExportWorkspaceRecord::builder()
        .workspace_id(workspace.id)
        .path(workspace.path)
        .created_at(workspace.created_at)
        .last_accessed(workspace.updated_at);
    if let Some(name) = workspace.name {
        workspace_builder = workspace_builder.name(name);
    }

    Ok(BackupExportData {
        workspace: workspace_builder
            .build()
            .map_err(export_build_error("build backup workspace record"))?,
        memories,
        tags_by_memory,
        links,
        audits,
    })
}

fn render_records(
    backup_id: &str,
    created_at: &str,
    redaction_level: RedactionLevel,
    data: &BackupExportData,
) -> Result<(Vec<u8>, ExportStats), DomainError> {
    let mut output = Vec::new();
    let stats = {
        let mut exporter = JsonlExporter::new(&mut output, redaction_level, ExportScope::All);
        exporter
            .write_header(
                ExportHeader::builder()
                    .created_at(created_at)
                    .workspace_id(data.workspace.workspace_id.clone())
                    .workspace_path(data.workspace.path.clone())
                    .export_scope(ExportScope::All)
                    .redaction_level(redaction_level)
                    .ee_version(env!("CARGO_PKG_VERSION"))
                    .export_id(backup_id)
                    .import_source(ImportSource::Native)
                    .trust_level(TrustLevel::Validated)
                    .build()
                    .map_err(export_build_error("build backup JSONL header"))?,
            )
            .map_err(io_error("write backup JSONL header"))?;
        exporter
            .write_workspace(data.workspace.clone())
            .map_err(io_error("write backup workspace record"))?;

        let tombstone_reasons = tombstone_reasons_by_memory(&data.audits);
        for memory in &data.memories {
            exporter
                .write_memory(
                    memory_record(
                        memory,
                        tombstone_reasons.get(&memory.id).map(String::as_str),
                    )
                    .map_err(export_build_error("build backup memory record"))?,
                )
                .map_err(io_error("write backup memory record"))?;
            for tag in
                memory_tags(data, memory).map_err(export_build_error("build backup tag record"))?
            {
                exporter
                    .write_tag(tag)
                    .map_err(io_error("write backup tag record"))?;
            }
        }
        for link in &data.links {
            exporter
                .write_link(
                    link_record(link).map_err(export_build_error("build backup link record"))?,
                )
                .map_err(io_error("write backup link record"))?;
        }
        for audit in &data.audits {
            exporter
                .write_audit(
                    audit_record(audit).map_err(export_build_error("build backup audit record"))?,
                )
                .map_err(io_error("write backup audit record"))?;
        }

        let stats = exporter
            .write_footer(
                ExportFooter::builder()
                    .export_id(backup_id)
                    .completed_at(created_at)
                    .build()
                    .map_err(export_build_error("build backup JSONL footer"))?,
            )
            .map_err(io_error("write backup JSONL footer"))?;
        exporter.flush().map_err(io_error("flush backup JSONL"))?;
        stats
    };
    Ok((output, stats))
}

fn memory_record(
    memory: &StoredMemory,
    tombstoned_reason: Option<&str>,
) -> Result<ExportMemoryRecord, ExportRecordBuildError> {
    let mut builder = ExportMemoryRecord::builder()
        .memory_id(memory.id.clone())
        .workspace_id(memory.workspace_id.clone())
        .level(memory.level.clone())
        .kind(memory.kind.clone())
        .content(memory.content.clone())
        .importance(f64::from(memory.importance))
        .confidence(f64::from(memory.confidence))
        .utility(f64::from(memory.utility))
        .created_at(memory.created_at.clone())
        .redacted(false);
    builder = builder.updated_at(memory.updated_at.clone());
    if let Some(provenance_uri) = &memory.provenance_uri {
        builder = builder.provenance_uri(provenance_uri.clone());
    }
    if let Some(tombstoned_at) = &memory.tombstoned_at {
        builder = builder.tombstoned_at(tombstoned_at.clone());
    }
    if let Some(reason) = tombstoned_reason {
        builder = builder.tombstoned_reason(reason.to_owned());
    }
    if let Some(valid_from) = &memory.valid_from {
        builder = builder.valid_from(valid_from.clone());
    }
    if let Some(valid_to) = &memory.valid_to {
        builder = builder
            .valid_to(valid_to.clone())
            .expires_at(valid_to.clone());
    }
    builder.build()
}

fn tombstone_reasons_by_memory(audits: &[StoredAuditEntry]) -> BTreeMap<String, String> {
    let mut reasons = BTreeMap::new();
    for audit in audits {
        if audit.action != audit_actions::MEMORY_TOMBSTONE
            || audit.target_type.as_deref() != Some("memory")
        {
            continue;
        }
        let Some(memory_id) = audit.target_id.as_ref() else {
            continue;
        };
        if reasons.contains_key(memory_id) {
            continue;
        }
        let Some(reason) = tombstone_reason_from_audit_details(audit.details.as_deref()) else {
            continue;
        };
        reasons.insert(memory_id.clone(), reason);
    }
    reasons
}

fn tombstone_reason_from_audit_details(details: Option<&str>) -> Option<String> {
    let value = serde_json::from_str::<JsonValue>(details?).ok()?;
    value
        .get("reason")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(str::to_owned)
}

fn memory_tags(
    data: &BackupExportData,
    memory: &StoredMemory,
) -> Result<Vec<ExportTagRecord>, ExportRecordBuildError> {
    data.tags_by_memory
        .get(&memory.id)
        .into_iter()
        .flat_map(|tags| tags.iter())
        .map(|tag| {
            ExportTagRecord::builder()
                .memory_id(memory.id.clone())
                .tag(tag.clone())
                .created_at(memory.created_at.clone())
                .build()
        })
        .collect()
}

fn link_record(link: &StoredMemoryLink) -> Result<ExportLinkRecord, ExportRecordBuildError> {
    ExportLinkRecord::builder()
        .link_id(link.id.clone())
        .source_memory_id(link.src_memory_id.clone())
        .target_memory_id(link.dst_memory_id.clone())
        .link_type(link.relation.clone())
        .weight(f64::from(link.weight))
        .created_at(link.created_at.clone())
        .metadata(link_metadata(link))
        .build()
}

fn link_metadata(link: &StoredMemoryLink) -> JsonValue {
    let parsed = link
        .metadata_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<JsonValue>(value).ok());
    json!({
        "confidence": link.confidence,
        "directed": link.directed,
        "evidenceCount": link.evidence_count,
        "lastReinforcedAt": link.last_reinforced_at,
        "source": link.source,
        "createdBy": link.created_by,
        "metadata": parsed,
    })
}

fn audit_record(audit: &StoredAuditEntry) -> Result<ExportAuditRecord, ExportRecordBuildError> {
    let mut builder = ExportAuditRecord::builder()
        .audit_id(audit.id.clone())
        .operation(audit.action.clone())
        .performed_at(audit.timestamp.clone())
        .details(audit_details(audit.details.as_deref()));
    if let Some(target_type) = &audit.target_type {
        builder = builder.target_type(target_type.clone());
    }
    if let Some(target_id) = &audit.target_id {
        builder = builder.target_id(target_id.clone());
    }
    if let Some(actor) = &audit.actor {
        builder = builder.performed_by(actor.clone());
    }
    builder.build()
}

fn audit_details(details: Option<&str>) -> JsonValue {
    details.map_or(JsonValue::Null, |details| {
        serde_json::from_str(details).unwrap_or_else(|_| json!({ "text": details }))
    })
}

fn manifest_json(
    report: &BackupCreateReport,
    created_at: &str,
    manifest_hash: Option<&str>,
) -> JsonValue {
    let mut manifest = json!({
        "schema": if report.include_derived {
            BACKUP_MANIFEST_SCHEMA_V2
        } else {
            BACKUP_MANIFEST_SCHEMA_V1
        },
        "backupId": report.backup_id,
        "label": report.label,
        "createdAt": created_at,
        "eeVersion": env!("CARGO_PKG_VERSION"),
        "workspace": {
            "id": report.workspace_id,
            "path": report.workspace_path,
        },
        "databasePath": report.database_path,
        "redactionLevel": report.redaction_level.as_str(),
        "exportScope": report.export_scope.as_str(),
        "counts": {
            "totalRecords": report.total_records,
            "memoryRecords": report.memory_count,
            "linkRecords": report.link_count,
            "tagRecords": report.tag_count,
            "auditRecords": report.audit_count,
        },
        "artifacts": report.artifacts.iter().map(BackupArtifactReport::data_json).collect::<Vec<_>>(),
        "degraded": report.degraded.iter().map(BackupDegradation::data_json).collect::<Vec<_>>(),
        "verification": {
            "status": report.verification_status,
            "manifestHash": manifest_hash,
        },
    });
    if report.include_derived {
        manifest["derived"] = JsonValue::Array(
            report
                .derived
                .iter()
                .map(BackupDerivedAssetReport::manifest_json)
                .collect(),
        );
    }
    manifest
}

fn collect_derived_payloads(
    connection: &DbConnection,
    workspace_path: &Path,
    workspace_id: &str,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
) -> Vec<BackupDerivedPayload> {
    let mut payloads = Vec::new();
    collect_index_manifest_payloads(workspace_path, captured_at, degraded, &mut payloads);
    collect_graph_snapshot_payloads(
        connection,
        workspace_id,
        captured_at,
        degraded,
        &mut payloads,
    );
    collect_task_episode_payloads(
        connection,
        workspace_id,
        captured_at,
        degraded,
        &mut payloads,
    );
    collect_lab_episode_file_payloads(workspace_path, captured_at, degraded, &mut payloads);
    collect_wal_holds_payload(connection, captured_at, degraded, &mut payloads);
    payloads
}

fn collect_index_manifest_payloads(
    workspace_path: &Path,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let candidates = [
        workspace_path
            .join(WORKSPACE_MARKER)
            .join("index")
            .join("ee.index_manifest.json"),
        workspace_path
            .join(WORKSPACE_MARKER)
            .join("index")
            .join("meta.json"),
        workspace_path
            .join(WORKSPACE_MARKER)
            .join("indexes")
            .join("combined")
            .join("manifest.json"),
    ];
    let mut included = false;
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        match fs::read(&candidate) {
            Ok(bytes) => {
                let name = candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(safe_file_stem)
                    .unwrap_or_else(|| "manifest.json".to_owned());
                payloads.push(derived_payload(
                    format!("derived/index/{name}"),
                    "index_manifest",
                    captured_at,
                    None,
                    bytes,
                ));
                included = true;
            }
            Err(error) => degraded.push(BackupDegradation::warning(
                "index_manifest_unreadable",
                format!(
                    "index manifest '{}' could not be read: {error}",
                    candidate.display()
                ),
                "inspect .ee/index permissions and retry backup create --include-derived",
            )),
        }
    }
    if !included {
        degraded.push(BackupDegradation::warning(
            "index_manifest_missing",
            "no workspace index manifest was found; backup includes the durable JSONL source of truth only",
            "run ee index rebuild --workspace . before creating a backup that must include derived index metadata",
        ));
    }
}

fn collect_graph_snapshot_payloads(
    connection: &DbConnection,
    workspace_id: &str,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let snapshots = match connection.list_graph_snapshots(workspace_id, None, 256) {
        Ok(snapshots) => snapshots,
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "graph_snapshots_unreadable",
                format!("graph snapshots could not be read from the database: {error}"),
                "run ee db check --workspace . before retrying backup create --include-derived",
            ));
            return;
        }
    };
    for snapshot in snapshots {
        match json_payload_bytes(&graph_snapshot_json(&snapshot, captured_at)) {
            Ok(bytes) => payloads.push(derived_payload(
                format!(
                    "derived/graph/snapshots/{}.json",
                    safe_file_stem(&snapshot.id)
                ),
                "graph_snapshot",
                captured_at,
                None,
                bytes,
            )),
            Err(error) => degraded.push(BackupDegradation::warning(
                "graph_snapshots_unreadable",
                format!("graph snapshot payload could not be serialized: {error}"),
                "run ee db check --workspace . before retrying backup create --include-derived",
            )),
        }
    }
}

fn graph_snapshot_json(snapshot: &StoredGraphSnapshot, captured_at: &str) -> JsonValue {
    json!({
        "schema": "ee.backup.derived.graph_snapshot.v1",
        "capturedAt": captured_at,
        "snapshot": {
            "id": &snapshot.id,
            "workspaceId": &snapshot.workspace_id,
            "snapshotVersion": snapshot.snapshot_version,
            "schemaVersion": &snapshot.schema_version,
            "graphType": snapshot.graph_type.as_str(),
            "nodeCount": snapshot.node_count,
            "edgeCount": snapshot.edge_count,
            "metrics": serde_json::from_str::<JsonValue>(&snapshot.metrics_json).unwrap_or(JsonValue::Null),
            "contentHash": &snapshot.content_hash,
            "sourceGeneration": snapshot.source_generation,
            "createdAt": &snapshot.created_at,
            "expiresAt": &snapshot.expires_at,
            "status": snapshot.status.as_str(),
        }
    })
}

fn collect_task_episode_payloads(
    connection: &DbConnection,
    workspace_id: &str,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let episodes = match connection.list_task_episodes(Some(workspace_id), None, 256) {
        Ok(episodes) => episodes,
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "lab_episodes_unreadable",
                format!("stored lab episodes could not be read from the database: {error}"),
                "run ee db check --workspace . before retrying backup create --include-derived",
            ));
            return;
        }
    };
    for episode in episodes {
        match json_payload_bytes(&task_episode_json(&episode, captured_at)) {
            Ok(bytes) => payloads.push(derived_payload(
                format!("derived/lab/episodes/{}.json", safe_file_stem(&episode.id)),
                "lab_episode",
                captured_at,
                Some(episode.id),
                bytes,
            )),
            Err(error) => degraded.push(BackupDegradation::warning(
                "lab_episodes_unreadable",
                format!("stored lab episode payload could not be serialized: {error}"),
                "run ee db check --workspace . before retrying backup create --include-derived",
            )),
        }
    }
}

fn task_episode_json(episode: &StoredTaskEpisode, captured_at: &str) -> JsonValue {
    json!({
        "schema": "ee.backup.derived.lab_episode.v1",
        "capturedAt": captured_at,
        "episode": {
            "id": &episode.id,
            "workspaceId": &episode.workspace_id,
            "sessionId": &episode.session_id,
            "taskInput": &episode.task_input,
            "retrievedMemoryIds": &episode.retrieved_memory_ids,
            "contextPackId": &episode.context_pack_id,
            "actions": &episode.actions,
            "outcome": &episode.outcome,
            "outcomeDetails": &episode.outcome_details,
            "startedAt": &episode.started_at,
            "endedAt": &episode.ended_at,
            "durationMs": episode.duration_ms,
            "agent": &episode.agent,
            "episodeHash": &episode.episode_hash,
            "createdAt": &episode.created_at,
        }
    })
}

fn collect_lab_episode_file_payloads(
    workspace_path: &Path,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    collect_lab_episode_file_dir(
        &workspace_path
            .join(WORKSPACE_MARKER)
            .join("lab")
            .join("episodes"),
        "workspace",
        captured_at,
        degraded,
        payloads,
    );
    let Some(episode_dir) = home_lab_episode_dir() else {
        return;
    };
    collect_lab_episode_file_dir(&episode_dir, "home", captured_at, degraded, payloads);
}

fn collect_lab_episode_file_dir(
    episode_dir: &Path,
    source_label: &str,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    if !episode_dir.exists() {
        return;
    }
    let entries = match fs::read_dir(episode_dir) {
        Ok(entries) => entries,
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "lab_episodes_unreadable",
                format!(
                    "lab episode directory '{}' could not be read: {error}",
                    episode_dir.display()
                ),
                "inspect ~/.local/share/ee/lab/episodes permissions and retry backup create --include-derived",
            ));
            return;
        }
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                degraded.push(BackupDegradation::warning(
                    "lab_episodes_unreadable",
                    format!("lab episode file '{}' could not be inspected: {error}", path.display()),
                    "inspect ~/.local/share/ee/lab/episodes permissions and retry backup create --include-derived",
                ));
                continue;
            }
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let safe_name = safe_file_stem(file_name);
        match fs::read(&path) {
            Ok(bytes) => payloads.push(derived_payload(
                format!("derived/lab/episode_files/{source_label}/{safe_name}"),
                "lab_episode",
                captured_at,
                Some(safe_file_stem(
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or(file_name),
                )),
                bytes,
            )),
            Err(error) => degraded.push(BackupDegradation::warning(
                "lab_episodes_unreadable",
                format!("lab episode file '{}' could not be read: {error}", path.display()),
                "inspect ~/.local/share/ee/lab/episodes permissions and retry backup create --include-derived",
            )),
        }
    }
}

fn home_lab_episode_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("ee")
            .join("lab")
            .join("episodes")
    })
}

fn collect_wal_holds_payload(
    connection: &DbConnection,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let tables = match connection.list_user_tables() {
        Ok(tables) => tables,
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "wal_holds_unreadable",
                format!("WAL hold table state could not be inspected: {error}"),
                "run ee db check --workspace . before retrying backup create --include-derived",
            ));
            return;
        }
    };
    let present = tables.iter().any(|table| table == "ee_wal_holds");
    let row_count = if present {
        match connection.count_table_rows("ee_wal_holds") {
            Ok(count) => Some(count),
            Err(error) => {
                degraded.push(BackupDegradation::warning(
                    "wal_holds_unreadable",
                    format!("WAL hold table rows could not be counted: {error}"),
                    "run ee db check --workspace . before retrying backup create --include-derived",
                ));
                None
            }
        }
    } else {
        None
    };

    match json_payload_bytes(&json!({
        "schema": "ee.backup.derived.wal_holds.v1",
        "capturedAt": captured_at,
        "table": "ee_wal_holds",
        "present": present,
        "rowCount": row_count,
    })) {
        Ok(bytes) => payloads.push(derived_payload(
            "derived/wal_holds.json",
            "wal_holds",
            captured_at,
            None,
            bytes,
        )),
        Err(error) => degraded.push(BackupDegradation::warning(
            "wal_holds_unreadable",
            format!("WAL hold state payload could not be serialized: {error}"),
            "run ee db check --workspace . before retrying backup create --include-derived",
        )),
    }
}

fn derived_payload(
    path: impl Into<String>,
    kind: impl Into<String>,
    captured_at: &str,
    episode_id_if_lab: Option<String>,
    bytes: Vec<u8>,
) -> BackupDerivedPayload {
    let path = path.into();
    let kind = kind.into();
    BackupDerivedPayload {
        report: BackupDerivedAssetReport {
            path,
            kind,
            hash: Some(hash_bytes(&bytes)),
            byte_size: Some(bytes.len() as u64),
            captured_at: Some(captured_at.to_owned()),
            episode_id_if_lab,
        },
        bytes,
    }
}

fn json_payload_bytes(value: &JsonValue) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn safe_file_stem(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.is_empty() {
        "episode".to_owned()
    } else {
        cleaned
    }
}

fn backup_degradations(workspace_path: &Path, include_derived: bool) -> Vec<BackupDegradation> {
    let mut degraded = Vec::new();
    let index_manifest = workspace_path
        .join(WORKSPACE_MARKER)
        .join("indexes")
        .join("combined")
        .join("manifest.json");
    if !include_derived && !index_manifest.is_file() {
        degraded.push(BackupDegradation::warning(
            "index_manifest_missing",
            "no workspace index manifest was found; backup includes the durable JSONL source of truth only",
            "run ee index rebuild --workspace . before creating a backup that must include derived index metadata",
        ));
    }
    if !include_derived {
        degraded.push(BackupDegradation::warning(
            "graph_snapshot_not_included",
            "graph snapshots are derived assets and are not included in the EE-223 backup foundation slice",
            "run ee graph refresh after restore, or complete EE-298 for richer backup inspection",
        ));
    }
    degraded
}

fn redaction_pattern_degradations(
    data: &BackupExportData,
    redaction_level: RedactionLevel,
) -> Vec<BackupDegradation> {
    if redaction_level == RedactionLevel::None {
        return Vec::new();
    }

    let mut classes = BTreeSet::new();
    for memory in &data.memories {
        let report = crate::policy::redact_secret_like_content(&memory.content);
        if report.redacted {
            classes.extend(report.redacted_reasons.into_iter().map(str::to_owned));
        }
    }

    classes
        .into_iter()
        .map(|class| {
            BackupDegradation::with_severity(
                "redaction_pattern_matched",
                "medium",
                format!(
                    "redaction matched secret detector class `{class}` at level `{}`",
                    redaction_level.as_str()
                ),
                "review the exported records and keep the redacted source of truth; do not attempt to un-redact without an external vault",
            )
        })
        .collect()
}

fn ensure_backup_directory(backup_root: &Path, backup_path: &Path) -> Result<(), DomainError> {
    fs::create_dir_all(backup_root).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to create backup root '{}': {error}",
            backup_root.display()
        ),
        repair: Some("choose a writable --output-dir".to_owned()),
    })?;
    fs::create_dir(backup_path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to create backup directory '{}': {error}",
            backup_path.display()
        ),
        repair: Some(
            "retry backup creation; existing backup directories are never overwritten".to_owned(),
        ),
    })
}

fn ensure_side_path_is_isolated(side_path: &Path) -> Result<(), DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(side_path)? {
        let message = if symlink_path == side_path {
            format!(
                "side path '{}' is a symbolic link; restore requires an isolated real directory",
                side_path.display()
            )
        } else {
            format!(
                "side path '{}' traverses symbolic link '{}'; restore requires an isolated real directory",
                side_path.display(),
                symlink_path.display()
            )
        };
        return Err(DomainError::PolicyDenied {
            message,
            repair: Some("choose a real, non-symlink directory for --side-path".to_owned()),
        });
    }

    match fs::symlink_metadata(side_path) {
        Ok(metadata) if !metadata.is_dir() => {
            return Err(DomainError::Storage {
                message: format!(
                    "side path '{}' exists but is not a directory",
                    side_path.display()
                ),
                repair: Some("choose a directory path for --side-path".to_owned()),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "failed to inspect side path '{}': {error}",
                    side_path.display()
                ),
                repair: Some(
                    "inspect filesystem permissions or choose another --side-path".to_owned(),
                ),
            });
        }
    }

    let mut entries = fs::read_dir(side_path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to read side path '{}': {error}",
            side_path.display()
        ),
        repair: Some("inspect filesystem permissions or choose another --side-path".to_owned()),
    })?;
    if entries.next().is_some() {
        return Err(DomainError::Storage {
            message: format!(
                "side path '{}' is not empty; restore refuses to overwrite existing data",
                side_path.display()
            ),
            repair: Some("choose a new empty --side-path target".to_owned()),
        });
    }
    Ok(())
}

fn first_existing_symlink_component(path: &Path) -> Result<Option<PathBuf>, DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "failed to inspect side path component '{}': {error}",
                        current.display()
                    ),
                    repair: Some(
                        "inspect filesystem permissions or choose another --side-path".to_owned(),
                    ),
                });
            }
        }
    }
    Ok(None)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), DomainError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| DomainError::Storage {
            message: format!("failed to create '{}': {error}", path.display()),
            repair: Some("retry with a fresh backup id or output directory".to_owned()),
        })?;
    file.write_all(bytes)
        .map_err(|error| DomainError::Storage {
            message: format!("failed to write '{}': {error}", path.display()),
            repair: Some("inspect the partial backup directory before retrying".to_owned()),
        })?;
    file.flush().map_err(|error| DomainError::Storage {
        message: format!("failed to flush '{}': {error}", path.display()),
        repair: Some("inspect disk health and retry backup creation".to_owned()),
    })
}

fn write_new_relative_file(
    root: &Path,
    relative_path: &str,
    bytes: &[u8],
) -> Result<PathBuf, DomainError> {
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create backup artifact directory '{}': {error}",
                parent.display()
            ),
            repair: Some("retry backup creation with a writable output directory".to_owned()),
        })?;
    }
    write_new_file(&path, bytes)?;
    Ok(path)
}

fn hash_file(path: &Path) -> Result<String, DomainError> {
    let mut file = File::open(path).map_err(|error| DomainError::Storage {
        message: format!("failed to read '{}': {error}", path.display()),
        repair: Some("inspect the backup directory and rerun verification".to_owned()),
    })?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher).map_err(|error| DomainError::Storage {
        message: format!("failed to hash '{}': {error}", path.display()),
        repair: Some("inspect the backup directory and rerun verification".to_owned()),
    })?;
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn file_size(path: &Path) -> Result<u64, DomainError> {
    path.metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| DomainError::Storage {
            message: format!("failed to stat '{}': {error}", path.display()),
            repair: Some("inspect the backup directory and rerun verification".to_owned()),
        })
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn io_error(context: &'static str) -> impl FnOnce(io::Error) -> DomainError {
    move |error| DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("inspect database integrity and retry backup creation".to_owned()),
    }
}

fn export_build_error(context: &'static str) -> impl FnOnce(ExportRecordBuildError) -> DomainError {
    move |error| DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("inspect database integrity and retry backup creation".to_owned()),
    }
}

fn normalized_label(label: Option<&str>) -> Option<String> {
    label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
}

fn database_path(options: &BackupCreateOptions, workspace_path: &Path) -> PathBuf {
    options
        .database_path
        .clone()
        .unwrap_or_else(|| workspace_path.join(WORKSPACE_MARKER).join(DEFAULT_DB_FILE))
}

fn backup_root(options: &BackupCreateOptions, workspace_path: &Path) -> PathBuf {
    backup_root_from(options.output_dir.as_deref(), workspace_path)
}

fn backup_root_from(output_dir: Option<&Path>, workspace_path: &Path) -> PathBuf {
    output_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        workspace_path
            .join(WORKSPACE_MARKER)
            .join(DEFAULT_BACKUP_DIR)
    })
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::{MemoryId, MemoryLinkId, WorkspaceId};
    use tempfile::TempDir;
    use uuid::Uuid;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: T,
        expected: T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn fixture() -> Result<(TempDir, PathBuf, PathBuf), DomainError> {
        let tempdir = tempfile::tempdir().map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: None,
        })?;
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(workspace.join(WORKSPACE_MARKER)).map_err(|error| {
            DomainError::Storage {
                message: error.to_string(),
                repair: None,
            }
        })?;
        let database = workspace.join(WORKSPACE_MARKER).join(DEFAULT_DB_FILE);
        let connection =
            DbConnection::open_file(&database).map_err(|error| DomainError::Storage {
                message: error.to_string(),
                repair: None,
            })?;
        connection.migrate().map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: None,
        })?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace
                        .canonicalize()
                        .map_err(|error| DomainError::Storage {
                            message: error.to_string(),
                            repair: None,
                        })?
                        .to_string_lossy()
                        .into_owned(),
                    name: Some("workspace".to_owned()),
                },
            )
            .map_err(|error| DomainError::Storage {
                message: error.to_string(),
                repair: None,
            })?;
        let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Authorization header should be redacted".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.6,
                    importance: 0.7,
                    provenance_uri: Some("ee-test://backup".to_owned()),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: Some("fixture".to_owned()),
                    tags: vec!["backup".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| DomainError::Storage {
                message: error.to_string(),
                repair: None,
            })?;
        connection
            .insert_audit(
                "audit_00000000000000000000000001",
                &CreateAuditInput {
                    workspace_id: Some(workspace_id),
                    actor: Some("test".to_owned()),
                    action: "memory.create".to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(memory_id),
                    details: Some(r#"{"source":"fixture"}"#.to_owned()),
                },
            )
            .map_err(|error| DomainError::Storage {
                message: error.to_string(),
                repair: None,
            })?;
        Ok((tempdir, workspace, database))
    }

    #[test]
    fn dry_run_does_not_create_backup_directory() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("planned-backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out.clone()),
            label: Some("pre-test".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        ensure_equal(report.status.as_str(), "dry_run", "dry run status")?;
        ensure_equal(
            report.verification_status.as_str(),
            "not_checked",
            "dry run verification",
        )?;
        ensure(!out.exists(), "dry run must not create output directory")
    }

    #[test]
    fn memory_record_preserves_lifecycle_metadata() -> TestResult {
        let record = memory_record(
            &StoredMemory {
                id: "mem_00000000000000000000000001".to_owned(),
                workspace_id: "ws_00000000000000000000000001".to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Run release checks before shipping.".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.7,
                importance: 0.8,
                provenance_uri: Some("ee-test://lifecycle".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: Some("fixture".to_owned()),
                provenance_chain_hash: None,
                provenance_chain_hash_version: "v1".to_owned(),
                provenance_verification_status: "unverified".to_owned(),
                provenance_verified_at: None,
                provenance_verification_note: None,
                created_at: "2026-05-01T00:00:00Z".to_owned(),
                updated_at: "2026-05-02T00:00:00Z".to_owned(),
                tombstoned_at: Some("2026-05-03T00:00:00Z".to_owned()),
                valid_from: Some("2026-04-01T00:00:00Z".to_owned()),
                valid_to: Some("2026-06-01T00:00:00Z".to_owned()),
            },
            Some("outdated rule"),
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(
            record.tombstoned_at.as_deref(),
            Some("2026-05-03T00:00:00Z"),
            "tombstoned_at",
        )?;
        ensure_equal(
            record.tombstoned_reason.as_deref(),
            Some("outdated rule"),
            "tombstoned_reason",
        )?;
        ensure_equal(
            record.valid_from.as_deref(),
            Some("2026-04-01T00:00:00Z"),
            "valid_from",
        )?;
        ensure_equal(
            record.valid_to.as_deref(),
            Some("2026-06-01T00:00:00Z"),
            "valid_to",
        )?;
        ensure_equal(
            record.expires_at.as_deref(),
            Some("2026-06-01T00:00:00Z"),
            "expires_at",
        )
    }

    #[test]
    fn backup_create_writes_records_and_manifest_with_hashes() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("pre-test".to_owned()),
            redaction_level: RedactionLevel::Minimal,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(report.status.as_str(), "completed", "backup status")?;
        ensure_equal(
            report.verification_status.as_str(),
            "verified",
            "verification status",
        )?;
        ensure(
            Path::new(&report.records_path).is_file(),
            "records JSONL must be written",
        )?;
        ensure(
            Path::new(&report.manifest_path).is_file(),
            "manifest JSON must be written",
        )?;
        ensure(report.records_hash.is_some(), "records hash is present")?;
        ensure(report.manifest_hash.is_some(), "manifest hash is present")?;

        let records =
            fs::read_to_string(&report.records_path).map_err(|error| error.to_string())?;
        ensure(
            records.contains("[REDACTED]"),
            "minimal redaction should redact secret-like memory content",
        )?;
        let manifest =
            fs::read_to_string(&report.manifest_path).map_err(|error| error.to_string())?;
        ensure(
            manifest.contains(BACKUP_MANIFEST_SCHEMA_V1),
            "manifest schema must be present",
        )
    }

    #[test]
    fn missing_database_returns_storage_error() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let result = create_backup(&BackupCreateOptions {
            workspace_path: tempdir.path().to_path_buf(),
            database_path: Some(tempdir.path().join("missing.db")),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            dry_run: false,
        });

        match result {
            Err(DomainError::Storage { message, repair }) => {
                ensure(
                    message.contains("does not exist"),
                    "missing database should be explicit",
                )?;
                ensure_equal(repair.as_deref(), Some("ee init --workspace ."), "repair")
            }
            other => Err(format!("expected storage error, got {other:?}")),
        }
    }

    #[test]
    fn generated_report_uses_stable_response_schema() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            dry_run: true,
        })
        .map_err(|error| error.message())?;
        let json = report.data_json();

        ensure_equal(
            json.get("schema").and_then(JsonValue::as_str),
            Some(BACKUP_CREATE_SCHEMA_V1),
            "report schema",
        )?;
        ensure_equal(
            json.get("command").and_then(JsonValue::as_str),
            Some("backup create"),
            "command name",
        )?;
        ensure(
            json.get("artifacts")
                .and_then(JsonValue::as_array)
                .is_some_and(|items| !items.is_empty()),
            "artifacts are listed",
        )
    }

    #[test]
    fn inspect_backup_reads_manifest_metadata() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("inspect".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let inspected = inspect_backup(&BackupInspectOptions {
            backup_path: PathBuf::from(&created.backup_path),
        })
        .map_err(|error| error.message())?;

        ensure_equal(inspected.schema, BACKUP_INSPECT_SCHEMA_V1, "inspect schema")?;
        ensure_equal(
            inspected.backup_id.as_str(),
            created.backup_id.as_str(),
            "inspect backup id",
        )?;
        ensure_equal(inspected.label.as_deref(), Some("inspect"), "inspect label")?;
        ensure(
            inspected.manifest_hash.starts_with("blake3:"),
            "inspect manifest hash is blake3",
        )?;
        ensure(
            inspected.issues.is_empty(),
            format!("inspect should be clean: {:?}", inspected.issues),
        )?;
        ensure(
            inspected
                .artifacts
                .iter()
                .any(|artifact| artifact.path == RECORDS_FILE),
            "inspect reports records artifact",
        )
    }

    #[test]
    fn list_backups_returns_manifest_entries_in_stable_order() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out.clone()),
            label: Some("list".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let listed = list_backups(&BackupListOptions {
            workspace_path: workspace,
            output_dir: Some(out),
        })
        .map_err(|error| error.message())?;

        ensure_equal(listed.schema, BACKUP_LIST_SCHEMA_V1, "list schema")?;
        ensure_equal(listed.backups.len(), 1, "listed backup count")?;
        let entry = listed
            .backups
            .first()
            .ok_or_else(|| "missing listed backup".to_owned())?;
        ensure_equal(
            entry.backup_id.as_str(),
            created.backup_id.as_str(),
            "listed backup id",
        )?;
        ensure_equal(entry.issue_count, 0, "listed issue count")
    }

    #[test]
    fn verify_backup_detects_tampered_artifact() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("verify".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        fs::write(&created.records_path, b"tampered\n").map_err(|error| error.to_string())?;

        let verified = verify_backup(&BackupVerifyOptions {
            backup_path: PathBuf::from(&created.backup_path),
        })
        .map_err(|error| error.message())?;

        ensure_equal(verified.schema, BACKUP_VERIFY_SCHEMA_V1, "verify schema")?;
        ensure_equal(verified.status.as_str(), "failed", "verify status")?;
        ensure(
            verified
                .issues
                .iter()
                .any(|issue| issue.code == "artifact_hash_mismatch"),
            "verify detects hash mismatch",
        )
    }

    #[test]
    fn include_derived_writes_v2_manifest_and_wal_holds_state() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("derived".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            created.include_derived,
            "report records include-derived mode",
        )?;
        ensure(
            created
                .derived
                .iter()
                .any(|derived| derived.kind == "wal_holds"),
            "WAL hold state is included as a derived asset",
        )?;
        let manifest_text =
            fs::read_to_string(&created.manifest_path).map_err(|error| error.to_string())?;
        let manifest =
            serde_json::from_str::<JsonValue>(&manifest_text).map_err(|error| error.to_string())?;
        ensure_equal(
            manifest.get("schema").and_then(JsonValue::as_str),
            Some(BACKUP_MANIFEST_SCHEMA_V2),
            "v2 manifest schema",
        )?;
        ensure(
            manifest
                .get("derived")
                .and_then(JsonValue::as_array)
                .is_some_and(|derived| {
                    derived.iter().any(|asset| {
                        asset.get("kind").and_then(JsonValue::as_str) == Some("wal_holds")
                    })
                }),
            "manifest derived array contains WAL hold state",
        )?;

        let verified = verify_backup(&BackupVerifyOptions {
            backup_path: PathBuf::from(&created.backup_path),
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            verified.status.as_str(),
            "verified",
            "derived verify status",
        )?;
        ensure(
            verified
                .checked_derived
                .iter()
                .any(|derived| derived.kind == "wal_holds"),
            "verify checks WAL hold derived asset",
        )
    }

    #[test]
    fn inspect_backup_reports_derived_assets() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("inspect-derived".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let inspected = inspect_backup(&BackupInspectOptions {
            backup_path: PathBuf::from(&created.backup_path),
        })
        .map_err(|error| error.message())?;
        let json = inspected.data_json();

        ensure(
            inspected
                .derived
                .iter()
                .any(|derived| derived.kind == "wal_holds"),
            "inspect reports WAL hold derived asset",
        )?;
        ensure(
            json.get("derived")
                .and_then(JsonValue::as_array)
                .is_some_and(|derived| {
                    derived.iter().any(|asset| {
                        asset.get("kind").and_then(JsonValue::as_str) == Some("wal_holds")
                            && asset.get("byteSize").and_then(JsonValue::as_u64).is_some()
                    })
                }),
            "inspect JSON exposes derived assets with byteSize",
        )
    }

    #[test]
    fn verify_and_restore_report_wal_holds_orphaned_warning() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection
            .execute_raw("CREATE TABLE ee_wal_holds (id TEXT PRIMARY KEY)")
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw("INSERT INTO ee_wal_holds (id) VALUES ('hold_fixture')")
            .map_err(|error| error.to_string())?;
        drop(connection);

        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("wal-holds".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let verified = verify_backup(&BackupVerifyOptions {
            backup_path: PathBuf::from(&created.backup_path),
        })
        .map_err(|error| error.message())?;
        ensure_equal(verified.status.as_str(), "degraded", "verify status")?;
        ensure(
            verified.issues.iter().any(|issue| {
                issue.code == "wal_holds_orphaned"
                    && issue.severity == "warning"
                    && issue.path.as_deref() == Some("derived/wal_holds.json")
            }),
            "verify reports warning-only WAL hold orphan state",
        )?;

        let side_path = tempdir.path().join("restore-wal-holds-side-path");
        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(restored.status.as_str(), "degraded", "restore status")?;
        ensure_equal(restored.issue_count, 1, "restore warning issue count")?;
        ensure(
            restored
                .restored_derived
                .iter()
                .any(|derived| derived.kind == "wal_holds"),
            "restore still materializes WAL hold derived asset",
        )
    }

    #[test]
    fn verify_backup_detects_corrupt_derived_asset() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("derived-corrupt".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        fs::write(
            Path::new(&created.backup_path).join("derived/wal_holds.json"),
            b"{\"schema\":\"tampered\"}\n",
        )
        .map_err(|error| error.to_string())?;

        let verified = verify_backup(&BackupVerifyOptions {
            backup_path: PathBuf::from(&created.backup_path),
        })
        .map_err(|error| error.message())?;

        ensure_equal(verified.status.as_str(), "failed", "verify status")?;
        ensure(
            verified
                .issues
                .iter()
                .any(|issue| issue.code == "derived_asset_corrupt"),
            "verify detects derived asset corruption",
        )
    }

    #[cfg(unix)]
    #[test]
    fn inspect_backup_rejects_symlink_manifest() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let backup_path = tempdir.path().join("backup");
        fs::create_dir_all(&backup_path).map_err(|error| error.to_string())?;
        let outside_manifest = tempdir.path().join("outside-manifest.json");
        fs::write(
            &outside_manifest,
            serde_json::to_vec(&json!({
                "schema": BACKUP_MANIFEST_SCHEMA_V1,
                "backupId": "backup-test",
                "artifacts": [],
            }))
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_manifest, backup_path.join(MANIFEST_FILE))
            .map_err(|error| error.to_string())?;

        let result = inspect_backup(&BackupInspectOptions { backup_path });

        match result {
            Err(DomainError::Storage { message, repair }) => {
                ensure(
                    message.contains("symbolic link"),
                    "symlink manifest should be rejected explicitly",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some("choose a self-contained backup directory"),
                    "symlink manifest repair",
                )
            }
            other => Err(format!("expected storage error, got {other:?}")),
        }
    }

    #[cfg(unix)]
    #[test]
    fn verify_backup_rejects_symlink_artifact_path() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let backup_path = tempdir.path().join("backup");
        fs::create_dir_all(&backup_path).map_err(|error| error.to_string())?;
        let outside_records = tempdir.path().join("outside-records.jsonl");
        let records_payload = b"{\"schema\":\"ee.export.header.v1\"}\n";
        fs::write(&outside_records, records_payload).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_records, backup_path.join(RECORDS_FILE))
            .map_err(|error| error.to_string())?;
        let manifest = json!({
            "schema": BACKUP_MANIFEST_SCHEMA_V1,
            "backupId": "backup-test",
            "artifacts": [{
                "path": RECORDS_FILE,
                "kind": "jsonl_export",
                "hash": hash_bytes(records_payload),
                "sizeBytes": records_payload.len(),
                "required": true,
            }],
        });
        let manifest_bytes =
            serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(backup_path.join(MANIFEST_FILE), manifest_bytes)
            .map_err(|error| error.to_string())?;

        let verified =
            verify_backup(&BackupVerifyOptions { backup_path }).map_err(|error| error.message())?;

        ensure_equal(
            verified.status.as_str(),
            "failed",
            "symlink artifact verification status",
        )?;
        ensure(
            verified.checked_artifacts.is_empty(),
            "symlink artifact should not be hashed as backup evidence",
        )?;
        ensure(
            verified.issues.iter().any(|issue| {
                issue.code == "artifact_path_symlink" && issue.path.as_deref() == Some(RECORDS_FILE)
            }),
            "verify should report symlink artifact path",
        )
    }

    #[test]
    fn restore_backup_to_side_path_imports_memories() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let side_path = tempdir.path().join("restore-side-path");

        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(
            restored.schema,
            BACKUP_RESTORE_SCHEMA_V1,
            "restore report schema",
        )?;
        ensure_equal(restored.status.as_str(), "completed", "restore status")?;
        ensure_equal(
            restored.imported_memory_count,
            1,
            "restore imported memory count",
        )?;
        ensure(
            Path::new(&restored.restored_database_path).is_file(),
            "restored database file exists",
        )?;
        ensure(
            Path::new(&restored.restore_artifact_dir)
                .join(RECORDS_FILE)
                .is_file(),
            "records artifact copied into side path",
        )?;

        let restored_connection = DbConnection::open(DatabaseConfig::file(PathBuf::from(
            &restored.restored_database_path,
        )))
        .map_err(|error| error.to_string())?;
        let workspaces = restored_connection
            .list_workspaces()
            .map_err(|error| error.to_string())?;
        ensure(
            !workspaces.is_empty(),
            "restored workspace count is non-zero",
        )?;
        let total_memories = workspaces
            .iter()
            .map(|workspace| {
                restored_connection
                    .list_memories(&workspace.id, None, true)
                    .map(|memories| memories.len())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .sum::<usize>();
        ensure_equal(total_memories, 1, "restored memory count")
    }

    #[test]
    fn restore_backup_to_side_path_materializes_derived_assets() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let episode_dir = workspace
            .join(WORKSPACE_MARKER)
            .join("lab")
            .join("episodes");
        fs::create_dir_all(&episode_dir).map_err(|error| error.to_string())?;
        fs::write(
            episode_dir.join("ep_restore.json"),
            b"{\"schema\":\"ee.lab.frozen_episode.v1\",\"episode_id\":\"ep_restore\"}\n",
        )
        .map_err(|error| error.to_string())?;

        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-derived".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let side_path = tempdir.path().join("restore-derived-side-path");

        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(restored.status.as_str(), "completed", "restore status")?;
        ensure(
            restored
                .restored_derived
                .iter()
                .any(|derived| derived.kind == "wal_holds"),
            "restore report includes WAL hold derived asset",
        )?;
        ensure(
            restored
                .restored_derived
                .iter()
                .any(|derived| derived.lab_episode_path.is_some()),
            "restore report includes materialized lab episode path",
        )?;
        ensure(
            Path::new(&restored.restore_artifact_dir)
                .join("derived/wal_holds.json")
                .is_file(),
            "restore artifact dir includes WAL hold state",
        )?;
        ensure(
            side_path
                .join(WORKSPACE_MARKER)
                .join("lab")
                .join("episodes")
                .join("ep_restore.json")
                .is_file(),
            "restore side path includes frozen lab episode file",
        )
    }

    #[test]
    fn restore_backup_dry_run_does_not_create_side_path() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-dry-run".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let side_path = tempdir.path().join("restore-dry-run-side-path");

        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        ensure_equal(
            restored.status.as_str(),
            "dry_run",
            "restore dry-run status",
        )?;
        ensure(
            !side_path.exists(),
            "dry-run restore keeps side path untouched",
        )
    }

    #[test]
    fn restore_backup_rejects_non_empty_side_path() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-non-empty".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let side_path = tempdir.path().join("restore-non-empty-side-path");
        fs::create_dir_all(&side_path).map_err(|error| error.to_string())?;
        fs::write(side_path.join("occupied.txt"), b"occupied")
            .map_err(|error| error.to_string())?;

        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path,
            dry_run: false,
        });

        match result {
            Err(DomainError::Storage { message, .. }) => ensure(
                message.contains("not empty"),
                "non-empty side path is rejected",
            ),
            other => Err(format!("expected storage error, got {other:?}")),
        }
    }

    #[cfg(unix)]
    #[test]
    fn restore_backup_rejects_symlinked_side_path_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-symlink-parent".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let real_root = tempdir.path().join("real-side-root");
        fs::create_dir_all(&real_root).map_err(|error| error.to_string())?;
        let linked_root = tempdir.path().join("linked-side-root");
        symlink(&real_root, &linked_root).map_err(|error| error.to_string())?;
        let side_path = linked_root.join("restore-side-path");

        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path,
            dry_run: false,
        });

        match result {
            Err(DomainError::PolicyDenied { message, .. }) => ensure(
                message.contains("traverses symbolic link"),
                "symlinked side path parent is rejected",
            )?,
            other => return Err(format!("expected policy denied error, got {other:?}")),
        }
        ensure(
            !real_root.join("restore-side-path").exists(),
            "restore must not write through a symlinked side-path parent",
        )
    }

    #[test]
    fn link_ids_remain_available_for_future_backup_richness() {
        let _ = MemoryLinkId::from_uuid(Uuid::from_u128(3)).to_string();
    }
}
