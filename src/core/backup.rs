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
use crate::db::{DatabaseConfig, DbConnection, StoredAuditEntry, StoredMemory, StoredMemoryLink};
use crate::models::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_LIST_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1, BACKUP_VERIFY_SCHEMA_V1, BackupId, DomainError, ExportAuditRecord,
    ExportFooter, ExportHeader, ExportLinkRecord, ExportMemoryRecord, ExportScope, ExportTagRecord,
    ExportWorkspaceRecord, ImportSource, RedactionLevel, TrustLevel,
};
use crate::output::jsonl_export::{ExportStats, JsonlExporter};

const DEFAULT_DB_FILE: &str = "ee.db";
const DEFAULT_BACKUP_DIR: &str = "backups";
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
    pub total_records: u64,
    pub memory_count: u64,
    pub link_count: u64,
    pub tag_count: u64,
    pub audit_count: u64,
    pub verification_status: String,
    pub artifacts: Vec<BackupArtifactReport>,
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
            "counts": {
                "totalRecords": self.total_records,
                "memoryRecords": self.memory_count,
                "linkRecords": self.link_count,
                "tagRecords": self.tag_count,
                "auditRecords": self.audit_count,
            },
            "verificationStatus": self.verification_status,
            "artifacts": self.artifacts.iter().map(BackupArtifactReport::data_json).collect::<Vec<_>>(),
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
    pub issues: Vec<BackupVerificationIssue>,
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

/// Honest degradation metadata for assets this slice cannot yet include.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub next_action: String,
}

impl BackupDegradation {
    fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        next_action: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity: "warning".to_owned(),
            message: message.into(),
            next_action: next_action.into(),
        }
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
    let degraded = backup_degradations(&workspace_path);

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

    let status = if issues.is_empty() {
        "verified"
    } else {
        "failed"
    };
    Ok(BackupVerifyReport {
        schema: BACKUP_VERIFY_SCHEMA_V1,
        backup_id: inspect.backup_id,
        status: status.to_owned(),
        backup_path: inspect.backup_path,
        manifest_path: inspect.manifest_path,
        manifest_hash: inspect.manifest_hash,
        checked_artifacts,
        issues,
    })
}

fn inspect_manifest(
    backup_path: &Path,
    manifest_path: &Path,
    manifest_hash: &str,
    manifest: &JsonValue,
) -> BackupInspectReport {
    let mut issues = Vec::new();
    if json_string(manifest, "schema").as_deref() != Some(BACKUP_MANIFEST_SCHEMA_V1) {
        issues.push(
            BackupVerificationIssue::error(
                "manifest_schema_mismatch",
                "backup manifest schema is missing or unsupported",
            )
            .with_expected_actual(
                BACKUP_MANIFEST_SCHEMA_V1,
                json_string(manifest, "schema").unwrap_or_else(|| "<missing>".to_owned()),
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
        artifacts: artifact_reports(manifest, &mut issues),
        degraded: degradation_reports(manifest),
        issues,
    }
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
    let relative = Path::new(artifact_path);
    if artifact_path.trim().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        issues.push(
            BackupVerificationIssue::error(
                "artifact_path_outside_backup",
                "backup artifact path is empty, absolute, or escapes the backup directory",
            )
            .with_path(artifact_path.to_owned()),
        );
        return None;
    }
    Some(backup_path.join(relative))
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
        workspace: workspace_builder.build(),
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
                    .build(),
            )
            .map_err(io_error("write backup JSONL header"))?;
        exporter
            .write_workspace(data.workspace.clone())
            .map_err(io_error("write backup workspace record"))?;

        for memory in &data.memories {
            exporter
                .write_memory(memory_record(memory))
                .map_err(io_error("write backup memory record"))?;
            for tag in memory_tags(data, memory) {
                exporter
                    .write_tag(tag)
                    .map_err(io_error("write backup tag record"))?;
            }
        }
        for link in &data.links {
            exporter
                .write_link(link_record(link))
                .map_err(io_error("write backup link record"))?;
        }
        for audit in &data.audits {
            exporter
                .write_audit(audit_record(audit))
                .map_err(io_error("write backup audit record"))?;
        }

        let stats = exporter
            .write_footer(
                ExportFooter::builder()
                    .export_id(backup_id)
                    .completed_at(created_at)
                    .build(),
            )
            .map_err(io_error("write backup JSONL footer"))?;
        exporter.flush().map_err(io_error("flush backup JSONL"))?;
        stats
    };
    Ok((output, stats))
}

fn memory_record(memory: &StoredMemory) -> ExportMemoryRecord {
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
    builder.build()
}

fn memory_tags(data: &BackupExportData, memory: &StoredMemory) -> Vec<ExportTagRecord> {
    data.tags_by_memory
        .get(&memory.id)
        .into_iter()
        .flat_map(|tags| tags.iter())
        .map(|tag| ExportTagRecord::new(memory.id.clone(), tag.clone(), memory.created_at.clone()))
        .collect()
}

fn link_record(link: &StoredMemoryLink) -> ExportLinkRecord {
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

fn audit_record(audit: &StoredAuditEntry) -> ExportAuditRecord {
    let mut builder = ExportAuditRecord::builder()
        .audit_id(audit.id.clone())
        .operation(audit.action.clone())
        .target_type(audit.target_type.clone().unwrap_or_default())
        .target_id(audit.target_id.clone().unwrap_or_default())
        .performed_at(audit.timestamp.clone())
        .details(audit_details(audit.details.as_deref()));
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
    json!({
        "schema": BACKUP_MANIFEST_SCHEMA_V1,
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
    })
}

fn backup_degradations(workspace_path: &Path) -> Vec<BackupDegradation> {
    let mut degraded = Vec::new();
    let index_manifest = workspace_path
        .join(WORKSPACE_MARKER)
        .join("indexes")
        .join("combined")
        .join("manifest.json");
    if !index_manifest.is_file() {
        degraded.push(BackupDegradation::warning(
            "index_manifest_missing",
            "no workspace index manifest was found; backup includes the durable JSONL source of truth only",
            "run ee index rebuild --workspace . before creating a backup that must include derived index metadata",
        ));
    }
    degraded.push(BackupDegradation::warning(
        "graph_snapshot_not_included",
        "graph snapshots are derived assets and are not included in the EE-223 backup foundation slice",
        "run ee graph refresh after restore, or complete EE-298 for richer backup inspection",
    ));
    degraded
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
    fn backup_create_writes_records_and_manifest_with_hashes() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("pre-test".to_owned()),
            redaction_level: RedactionLevel::Minimal,
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
    fn link_ids_remain_available_for_future_backup_richness() {
        let _ = MemoryLinkId::from_uuid(Uuid::from_u128(3)).to_string();
    }
}
