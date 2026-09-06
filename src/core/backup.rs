//! Backup creation support (EE-223).
//!
//! This first backup slice writes a side-path backup directory containing a
//! redacted JSONL export plus a manifest with content hashes. It never
//! overwrites an existing backup artifact.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use fnx_runtime::CompatibilityMode;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::config::{EnvVar, WORKSPACE_MARKER, read_env_var, read_env_var_os};
use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::core::jsonl_import::{
    IMPORT_ACTION, JsonlImportOptions, import_memory_id, import_verified_backup_jsonl_records,
};
use crate::db::shard::{
    ShardFanoutPosture, ShardFanoutResolverInput, ShardFanoutStatusReport,
    resolve_shard_fanout_status, shard_fanout_enabled_from_env_value,
};
use crate::db::{
    CreateGraphAlgorithmResultInput, CreateGraphAlgorithmWitnessInput, CreateGraphSnapshotInput,
    CreateTaskEpisodeInput, CreateWorkspaceInput, DatabaseConfig, DbConnection, GraphSnapshotType,
    MeshStorageStatus, StoredAuditEntry, StoredEpisodeAction, StoredEvidenceSpan,
    StoredGraphAlgorithmResult, StoredGraphAlgorithmWitness, StoredGraphSnapshot, StoredMemory,
    StoredMemoryLink, StoredSession, StoredTaskEpisode, audit_actions,
};
use crate::models::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_LIST_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1, BACKUP_MANIFEST_SCHEMA_V2, BACKUP_RESTORE_SCHEMA_V1,
    BACKUP_VERIFY_SCHEMA_V1, BackupId, DomainError, ExportAuditRecord, ExportFooter, ExportHeader,
    ExportLinkRecord, ExportMemoryRecord, ExportScope, ExportTagRecord, ExportWorkspaceRecord,
    ImportSource, RedactionLevel, TrustLevel, jsonl::ExportRecordBuildError,
};
use crate::output::jsonl_export::{
    ExportStats, JsonlExporter, redact_content, redact_memory_record,
};
use crate::policy::import_auth::{
    ArtifactContext, EXPORT_ARTIFACT_FAMILY, EXPORT_RECORD_ENCODING_V1, STORE_KEY_NAMESPACE_V1,
    authenticate_artifact,
};
use crate::policy::store_auth::{MacDomain, StoreAuthError, StoreAuthRoot, workspace_keys_dir};

const DEFAULT_DB_FILE: &str = "ee.db";
const DEFAULT_BACKUP_DIR: &str = "backups";
const DEFAULT_RESTORE_DIR: &str = "restores";
const RECORDS_FILE: &str = "records.jsonl";
const MANIFEST_FILE: &str = "manifest.json";
const INIT_AND_MIGRATE_REPAIR_COMMAND: &str =
    "ee init --workspace . && ee migrate run --workspace . --json";
const CASS_BACKUP_CHUNK_ROWS: usize = 128;
const CASS_SESSION_RESTORE_METADATA_SCHEMA_V1: &str = "ee.backup.restored_cass_session_metadata.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackupTablePolicy {
    owner: &'static str,
    disposition: &'static str,
    coverage: &'static str,
}

impl BackupTablePolicy {
    const fn new(owner: &'static str, disposition: &'static str, coverage: &'static str) -> Self {
        Self {
            owner,
            disposition,
            coverage,
        }
    }

    fn schema_covered(self) -> bool {
        !matches!(self.coverage, "not_implemented" | "unclassified")
    }

    fn snapshot_covered(self) -> bool {
        self.schema_covered() && !matches!(self.coverage, "derived_artifact_restore")
    }
}

/// One migration-reconciled table entry in a backup's recovery inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupRecoveryInventoryEntry {
    pub table: String,
    pub owner: String,
    pub disposition: String,
    pub coverage: String,
    pub row_count: u64,
    pub schema_covered: bool,
    pub snapshot_covered: bool,
}

impl BackupRecoveryInventoryEntry {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "table": self.table,
            "owner": self.owner,
            "disposition": self.disposition,
            "coverage": self.coverage,
            "rowCount": self.row_count,
            "schemaCovered": self.schema_covered,
            "snapshotCovered": self.snapshot_covered,
        })
    }
}

/// Recovery coverage computed from the live migrated schema and exact row counts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackupRecoveryInventory {
    pub entries: Vec<BackupRecoveryInventoryEntry>,
    pub schema_coverage_complete: bool,
    pub snapshot_coverage_complete: bool,
    pub uncovered_required_table_count: u32,
    pub uncovered_required_row_count: u64,
    pub unclassified_table_count: u32,
}

impl BackupRecoveryInventory {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": "ee.backup.recovery_inventory.v1",
            "schemaCoverageComplete": self.schema_coverage_complete,
            "snapshotCoverageComplete": self.snapshot_coverage_complete,
            "uncoveredRequiredTableCount": self.uncovered_required_table_count,
            "uncoveredRequiredRowCount": self.uncovered_required_row_count,
            "unclassifiedTableCount": self.unclassified_table_count,
            "tables": self.entries.iter().map(BackupRecoveryInventoryEntry::data_json).collect::<Vec<_>>(),
        })
    }
}

/// Options for one `ee backup create` operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupCreateOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub label: Option<String>,
    pub redaction_level: RedactionLevel,
    pub include_derived: bool,
    pub include_graph_cache: bool,
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
    pub restore_graph_cache: bool,
    pub dry_run: bool,
}

/// Redaction-safe summary of mesh state captured in a backup manifest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackupMeshSummary {
    pub included: bool,
    pub peer_count: u32,
    pub cursor_count: u32,
    pub imported_event_count: u32,
    pub policy_decision_event_count: u32,
    pub policy_failure_event_count: u32,
    pub mapped_memory_count: u32,
    pub cached_body_count: u32,
}

impl BackupMeshSummary {
    #[must_use]
    pub fn from_storage_status(status: &MeshStorageStatus) -> Self {
        Self {
            included: mesh_storage_status_has_rows(status),
            peer_count: status.peer_count,
            cursor_count: status.cursor_count,
            imported_event_count: status.imported_event_count,
            policy_decision_event_count: status.policy_decision_event_count,
            policy_failure_event_count: status.policy_failure_event_count,
            mapped_memory_count: status.mapped_memory_count,
            cached_body_count: status.cached_body_count,
        }
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "included": self.included,
            "tables": {
                "mesh_peers": self.peer_count,
                "mesh_peer_cursors": self.cursor_count,
                "mesh_import_ledger": self.imported_event_count,
                "mesh_policy_decision_events": self.policy_decision_event_count,
                "mesh_policy_failure_events": self.policy_failure_event_count,
                "mesh_memory_mappings": self.mapped_memory_count,
                "mesh_body_cache_metadata": self.cached_body_count,
            },
            "restorePolicy": {
                "peerCredentials": "redacted",
                "peers": "disabled_until_repaired",
                "cursors": "preserved_as_diagnostics_not_replayed",
                "cachedBodies": "metadata_only_revalidate_after_restore",
            },
            "nextAction": if self.included {
                "run ee mesh doctor --workspace <side-path> --json and re-pair peers before enabling mesh sync"
            } else {
                "none"
            },
        })
    }
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
    pub include_graph_cache: bool,
    pub graph_cache_schema_version: Option<u32>,
    pub total_records: u64,
    pub memory_count: u64,
    pub link_count: u64,
    pub tag_count: u64,
    pub audit_count: u64,
    pub verification_status: String,
    pub recovery_inventory: BackupRecoveryInventory,
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
            "includeGraphCache": self.include_graph_cache,
            "graphCache": graph_cache_summary_json(self),
            "counts": {
                "totalRecords": self.total_records,
                "memoryRecords": self.memory_count,
                "linkRecords": self.link_count,
                "tagRecords": self.tag_count,
                "auditRecords": self.audit_count,
            },
            "verificationStatus": self.verification_status,
            "recoveryInventory": self.recovery_inventory.data_json(),
            "artifacts": self.artifacts.iter().map(BackupArtifactReport::data_json).collect::<Vec<_>>(),
            "derived": self.derived.iter().map(BackupDerivedAssetReport::data_json).collect::<Vec<_>>(),
            "degraded": backup_degraded_data_json("backup_create", &self.degraded),
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
            "degraded": backup_degraded_data_json("backup_inspect", &self.degraded),
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
            "degraded": backup_degraded_data_json("backup_list", &self.degraded),
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
    pub restore_graph_cache: bool,
    pub imported_memory_count: u32,
    pub skipped_duplicate_count: u32,
    pub restored_task_episode_count: u32,
    pub restored_cass_session_count: u32,
    pub restored_evidence_span_count: u32,
    pub restored_graph_cache_count: u32,
    pub restored_derived: Vec<BackupRestoredDerivedAssetReport>,
    pub issue_count: u32,
    pub degraded: Vec<BackupDegradation>,
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
            "restoreGraphCache": self.restore_graph_cache,
            "counts": {
                "memoriesImported": self.imported_memory_count,
                "memoriesSkippedDuplicate": self.skipped_duplicate_count,
                "taskEpisodesRestored": self.restored_task_episode_count,
                "cassSessionsRestored": self.restored_cass_session_count,
                "evidenceSpansRestored": self.restored_evidence_span_count,
                "graphCacheRowsRestored": self.restored_graph_cache_count,
                "issues": self.issue_count,
            },
            "restoredDerived": self.restored_derived.iter().map(BackupRestoredDerivedAssetReport::data_json).collect::<Vec<_>>(),
            "degraded": backup_degraded_data_json("backup_restore", &self.degraded),
            "nextActions": self.next_actions,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let prefix = if self.dry_run { "DRY RUN: " } else { "" };
        format!(
            "{prefix}backup restore {status}: {backup_id}\n  side path: {side_path}\n  restored db: {database}\n  imported memories: {imported} (duplicates: {duplicates})\n  restored task episodes: {episodes}\n  restored CASS sessions/evidence: {sessions}/{evidence}\n",
            status = self.status,
            backup_id = self.backup_id,
            side_path = self.side_path,
            database = self.restored_database_path,
            imported = self.imported_memory_count,
            duplicates = self.skipped_duplicate_count,
            episodes = self.restored_task_episode_count,
            sessions = self.restored_cass_session_count,
            evidence = self.restored_evidence_span_count,
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

fn backup_degraded_data_json(
    source: &'static str,
    degraded: &[BackupDegradation],
) -> Vec<JsonValue> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            source,
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry.next_action.clone(),
        )
    }))
    .into_iter()
    .map(|entry| {
        json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": entry.message,
            "nextAction": entry.repair,
            "sources": entry.sources,
        })
    })
    .collect()
}

struct BackupExportData {
    workspace: ExportWorkspaceRecord,
    memories: Vec<StoredMemory>,
    tags_by_memory: BTreeMap<String, Vec<String>>,
    links: Vec<StoredMemoryLink>,
    audits: Vec<StoredAuditEntry>,
    graph_fields_by_memory: BTreeMap<String, BackupMemoryGraphFields>,
    /// bd-multiplicity-aware-trust-p0u7g: per-memory attempt-family block
    /// (pointer + own ledger slot + family origin) so restore can rebuild
    /// the family ledger without inference.
    attempt_families_by_memory: BTreeMap<String, crate::models::ExportAttemptFamilyRecord>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct BackupMemoryGraphFields {
    pagerank_score: Option<f64>,
    betweenness_score: Option<f64>,
    hits_authority: Option<f64>,
    hits_hub: Option<f64>,
    onion_layer: Option<u32>,
    k_truss_max: Option<u32>,
    articulation_point: Option<bool>,
    bayes_alpha: Option<f64>,
    bayes_beta: Option<f64>,
}

impl BackupMemoryGraphFields {
    fn overlay_present(&mut self, imported: Self) {
        if imported.pagerank_score.is_some() {
            self.pagerank_score = imported.pagerank_score;
        }
        if imported.betweenness_score.is_some() {
            self.betweenness_score = imported.betweenness_score;
        }
        if imported.hits_authority.is_some() {
            self.hits_authority = imported.hits_authority;
        }
        if imported.hits_hub.is_some() {
            self.hits_hub = imported.hits_hub;
        }
        if imported.onion_layer.is_some() {
            self.onion_layer = imported.onion_layer;
        }
        if imported.k_truss_max.is_some() {
            self.k_truss_max = imported.k_truss_max;
        }
        if imported.articulation_point.is_some() {
            self.articulation_point = imported.articulation_point;
        }
        if imported.bayes_alpha.is_some() {
            self.bayes_alpha = imported.bayes_alpha;
        }
        if imported.bayes_beta.is_some() {
            self.bayes_beta = imported.bayes_beta;
        }
    }

    fn has_any_field(&self) -> bool {
        self.pagerank_score.is_some()
            || self.betweenness_score.is_some()
            || self.hits_authority.is_some()
            || self.hits_hub.is_some()
            || self.onion_layer.is_some()
            || self.k_truss_max.is_some()
            || self.articulation_point.is_some()
            || self.bayes_alpha.is_some()
            || self.bayes_beta.is_some()
    }
}

struct BackupDerivedPayload {
    report: BackupDerivedAssetReport,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupCassSessionRecord {
    id: String,
    workspace_id: String,
    source_locator_hash: String,
    source_metadata_hash: Option<String>,
    agent_name: Option<String>,
    model: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    message_count: u32,
    token_count: Option<u32>,
    content_hash: String,
    imported_at: String,
    updated_at: String,
}

impl BackupCassSessionRecord {
    fn from_stored(session: &StoredSession) -> Self {
        let restored_metadata = session
            .metadata_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok())
            .filter(|metadata| {
                metadata.get("schema").and_then(JsonValue::as_str)
                    == Some(CASS_SESSION_RESTORE_METADATA_SCHEMA_V1)
            });
        let source_locator_hash = restored_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("sourceLocatorHash"))
            .and_then(JsonValue::as_str)
            .map_or_else(
                || hash_bytes(session.cass_session_id.as_bytes()),
                str::to_owned,
            );
        let source_metadata_hash = restored_metadata.as_ref().map_or_else(
            || {
                session
                    .metadata_json
                    .as_deref()
                    .map(|metadata| hash_bytes(metadata.as_bytes()))
            },
            |metadata| {
                metadata
                    .get("sourceMetadataHash")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
            },
        );
        Self {
            id: session.id.clone(),
            workspace_id: session.workspace_id.clone(),
            source_locator_hash,
            source_metadata_hash,
            agent_name: session.agent_name.clone(),
            model: session.model.clone(),
            started_at: session.started_at.clone(),
            ended_at: session.ended_at.clone(),
            message_count: session.message_count,
            token_count: session.token_count,
            content_hash: session.content_hash.clone(),
            imported_at: session.imported_at.clone(),
            updated_at: session.updated_at.clone(),
        }
    }

    fn into_restored(self, workspace_id: String) -> StoredSession {
        let metadata_json = json!({
            "schema": CASS_SESSION_RESTORE_METADATA_SCHEMA_V1,
            "sourceLocatorPolicy": "omitted_host_local",
            "sourceLocatorHash": self.source_locator_hash,
            "sourceMetadataHash": self.source_metadata_hash,
        })
        .to_string();
        StoredSession {
            cass_session_id: portable_cass_session_id(&self.id),
            id: self.id,
            workspace_id,
            source_path: None,
            agent_name: self.agent_name,
            model: self.model,
            started_at: self.started_at,
            ended_at: self.ended_at,
            message_count: self.message_count,
            token_count: self.token_count,
            content_hash: self.content_hash,
            metadata_json: Some(metadata_json),
            imported_at: self.imported_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupCassEvidenceRecord {
    id: String,
    workspace_id: String,
    session_id: String,
    memory_id: Option<String>,
    cass_span_id: String,
    span_kind: String,
    start_line: u32,
    end_line: u32,
    start_byte: Option<u32>,
    end_byte: Option<u32>,
    role: Option<String>,
    excerpt: String,
    content_hash: String,
    metadata_json: Option<String>,
    producer_kind: String,
    screening_version: u32,
    secret_redaction_status: String,
    redaction_classes_json: String,
    instruction_risk: String,
    search_eligibility: String,
    pack_eligibility: String,
    canonical_provenance_revision: u32,
    canonical_excerpt_hash: Option<String>,
    security_policy_epoch: u32,
    upstream_ref_hash: Option<String>,
    created_at: String,
    updated_at: String,
}

impl BackupCassEvidenceRecord {
    fn from_stored(span: &StoredEvidenceSpan) -> Self {
        Self {
            id: span.id.clone(),
            workspace_id: span.workspace_id.clone(),
            session_id: span.session_id.clone(),
            memory_id: span.memory_id.clone(),
            cass_span_id: span.cass_span_id.clone(),
            span_kind: span.span_kind.clone(),
            start_line: span.start_line,
            end_line: span.end_line,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            role: span.role.clone(),
            excerpt: span.excerpt.clone(),
            content_hash: span.content_hash.clone(),
            metadata_json: span.metadata_json.clone(),
            producer_kind: span.producer_kind.clone(),
            screening_version: span.screening_version,
            secret_redaction_status: span.secret_redaction_status.clone(),
            redaction_classes_json: span.redaction_classes_json.clone(),
            instruction_risk: span.instruction_risk.clone(),
            search_eligibility: span.search_eligibility.clone(),
            pack_eligibility: span.pack_eligibility.clone(),
            canonical_provenance_revision: span.canonical_provenance_revision,
            canonical_excerpt_hash: span.canonical_excerpt_hash.clone(),
            security_policy_epoch: span.security_policy_epoch,
            upstream_ref_hash: span.upstream_ref_hash.clone(),
            created_at: span.created_at.clone(),
            updated_at: span.updated_at.clone(),
        }
    }

    fn into_restored(self, workspace_id: String) -> StoredEvidenceSpan {
        StoredEvidenceSpan {
            id: self.id,
            workspace_id,
            session_id: self.session_id,
            memory_id: self.memory_id,
            cass_span_id: self.cass_span_id,
            span_kind: self.span_kind,
            start_line: self.start_line,
            end_line: self.end_line,
            start_byte: self.start_byte,
            end_byte: self.end_byte,
            role: self.role,
            excerpt: self.excerpt,
            content_hash: self.content_hash,
            metadata_json: self.metadata_json,
            producer_kind: self.producer_kind,
            screening_version: self.screening_version,
            secret_redaction_status: self.secret_redaction_status,
            redaction_classes_json: self.redaction_classes_json,
            instruction_risk: self.instruction_risk,
            search_eligibility: self.search_eligibility,
            pack_eligibility: self.pack_eligibility,
            canonical_provenance_revision: self.canonical_provenance_revision,
            canonical_excerpt_hash: self.canonical_excerpt_hash,
            security_policy_epoch: self.security_policy_epoch,
            upstream_ref_hash: self.upstream_ref_hash,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn redact_for_export(&mut self, level: RedactionLevel, provenance_admitted: bool) {
        if level == RedactionLevel::None {
            return;
        }
        let excerpt = if provenance_admitted {
            redact_content(&self.excerpt, level)
        } else {
            // Legacy or quarantined rows retain their identity and disposition,
            // but cannot carry unchecked source text into a portable backup.
            redact_content(&self.excerpt, RedactionLevel::Full)
        };
        if !provenance_admitted {
            self.cass_span_id = hash_bytes(self.cass_span_id.as_bytes());
            self.span_kind = redact_content(&self.span_kind, level);
            self.role = self.role.as_deref().map(|role| redact_content(role, level));
        }
        if excerpt != self.excerpt || !provenance_admitted {
            self.excerpt = excerpt;
            self.content_hash = hash_bytes(self.excerpt.as_bytes());
            self.canonical_excerpt_hash = None;
            self.canonical_provenance_revision = 0;
            self.security_policy_epoch = 0;
            self.metadata_json = None;
            self.secret_redaction_status = "redacted".to_owned();
            self.redaction_classes_json = "[\"backup_redaction\"]".to_owned();
            self.search_eligibility = "denied".to_owned();
            self.pack_eligibility = "denied".to_owned();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupCassSessionChunk {
    schema: String,
    captured_at: String,
    chunk_index: u32,
    source_locator_policy: String,
    sessions: Vec<BackupCassSessionRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupCassEvidenceChunk {
    schema: String,
    captured_at: String,
    chunk_index: u32,
    evidence_spans: Vec<BackupCassEvidenceRecord>,
}

fn portable_cass_session_id(session_id: &str) -> String {
    format!("ee-session:{session_id}")
}

fn backup_table_policy(table: &str) -> BackupTablePolicy {
    if legacy_migration_table(table) {
        return BackupTablePolicy::new(
            "maintain",
            "intentionally_ephemeral",
            "legacy_debris_not_replayed",
        );
    }

    match table {
        "workspaces" => {
            BackupTablePolicy::new("maintain", "export_restore_required", "records_jsonl")
        }
        "memories" | "memory_tags" | "memory_links" => {
            BackupTablePolicy::new("retrieve", "export_restore_required", "records_jsonl")
        }
        "attempt_families" | "attempt_family_members" => {
            BackupTablePolicy::new("learn", "export_restore_required", "records_jsonl")
        }
        "audit_log" => {
            BackupTablePolicy::new("maintain", "export_restore_required", "records_jsonl")
        }

        "graph_snapshots" | "graph_algorithm_witnesses" | "graph_algorithm_results" => {
            BackupTablePolicy::new(
                "retrieve",
                "derived_rebuildable",
                "derived_artifact_optional",
            )
        }
        "memory_anchor_index"
        | "primer_cache"
        | "retrieval_affinity_accumulation"
        | "retrieval_affinity_cursor"
        | "workspace_generations" => {
            BackupTablePolicy::new("retrieve", "derived_rebuildable", "rebuild_on_restore")
        }
        "model_registry" | "agent_installations" | "agent_history_sources" => {
            BackupTablePolicy::new("maintain", "derived_rebuildable", "rediscover_on_restore")
        }
        "memory_anchors" | "memory_sentinel_results" => {
            BackupTablePolicy::new("maintain", "derived_rebuildable", "rebuild_on_restore")
        }

        "ee_schema_migrations" | "curation_ttl_policies" => BackupTablePolicy::new(
            "maintain",
            "migration_metadata",
            "recreated_by_current_binary",
        ),
        "ee_advisory_locks" | "ee_wal_holds" | "remember_idempotency_keys" => {
            BackupTablePolicy::new(
                "maintain",
                "intentionally_ephemeral",
                "intentionally_not_replayed",
            )
        }
        "preflight_bypass_tokens" => {
            BackupTablePolicy::new("maintain", "secret_rekeyed", "intentionally_not_replayed")
        }

        "mesh_peers"
        | "mesh_peer_cursors"
        | "mesh_import_ledger"
        | "mesh_memory_mappings"
        | "mesh_body_cache_metadata"
        | "mesh_lane_grant_states"
        | "mesh_origin_events"
        | "mesh_origin_event_nonces"
        | "mesh_origin_dispositions"
        | "team_admission_peer_state"
        | "team_history_projections"
        | "team_idp_oidc"
        | "team_idp_policy"
        | "team_idp_token_replay"
        | "team_invite_auth_floor"
        | "team_join_attempts"
        | "team_member_identity"
        | "team_member_nodes"
        | "team_member_signing_keys"
        | "team_members"
        | "team_pending_invites"
        | "team_posture"
        | "team_projects"
        | "team_removal_acknowledgements" => {
            BackupTablePolicy::new("maintain", "secret_rekeyed", "rekey_or_reenroll")
        }

        "task_episodes" => BackupTablePolicy::new(
            "learn",
            "export_restore_required",
            "derived_artifact_restore",
        ),

        "agent_context_profiles"
        | "agents"
        | "artifact_links"
        | "artifacts"
        | "causal_evidence"
        | "certificates"
        | "debt_snapshots"
        | "error_fingerprints"
        | "error_repair_links"
        | "journal_entries"
        | "memory_seals"
        | "memory_sentinel_specs"
        | "rationale_trace_links"
        | "rationale_traces"
        | "rch_verify_runs"
        | "recorder_events"
        | "recorder_runs"
        | "reflection_request_ledger"
        | "search_index_jobs"
        | "situation_records"
        | "tripwire_check_events"
        | "tripwires"
        | "trust_quarantine" => {
            BackupTablePolicy::new("maintain", "export_restore_required", "not_implemented")
        }
        "evidence_spans" | "sessions" => BackupTablePolicy::new(
            "ingest",
            "export_restore_required",
            "derived_artifact_restore",
        ),
        "import_ledger" => {
            BackupTablePolicy::new("ingest", "export_restore_required", "not_implemented")
        }
        "pack_baselines"
        | "pack_candidate_impressions"
        | "pack_evidence_items"
        | "pack_items"
        | "pack_omissions"
        | "pack_records" => {
            BackupTablePolicy::new("pack", "export_restore_required", "not_implemented")
        }
        "curation_candidates"
        | "feedback_events"
        | "feedback_quarantine"
        | "learning_observations"
        | "outcome_evidence_rows"
        | "plan_recipes"
        | "procedural_rules"
        | "procedure_events"
        | "procedures"
        | "rule_source_memories"
        | "rule_tags" => {
            BackupTablePolicy::new("learn", "export_restore_required", "not_implemented")
        }
        _ => BackupTablePolicy::new("maintain", "unclassified", "unclassified"),
    }
}

fn legacy_migration_table(table: &str) -> bool {
    let suffix_version = table.rsplit_once("_v").is_some_and(|(_, version)| {
        !version.is_empty() && version.chars().all(|c| c.is_ascii_digit())
    });
    let prefix_version = table.strip_prefix('v').is_some_and(|rest| {
        let digit_count = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        digit_count > 0 && rest.as_bytes().get(digit_count) == Some(&b'_')
    });
    suffix_version || prefix_version
}

fn build_recovery_inventory(
    connection: &DbConnection,
) -> Result<BackupRecoveryInventory, DomainError> {
    let tables = connection
        .list_user_tables()
        .map_err(|error| DomainError::Storage {
            message: format!("failed to enumerate backup source tables: {error}"),
            repair: Some("ee db check --workspace .".to_owned()),
        })?;
    let mut entries = Vec::with_capacity(tables.len());
    for table in tables {
        let raw_row_count =
            connection
                .count_table_rows(&table)
                .map_err(|error| DomainError::Storage {
                    message: format!("failed to count backup source table {table:?}: {error}"),
                    repair: Some("ee db check --workspace .".to_owned()),
                })?;
        let row_count = u64::try_from(raw_row_count).map_err(|_| DomainError::Storage {
            message: format!("backup row count for {table:?} was negative"),
            repair: Some("ee db check --workspace .".to_owned()),
        })?;
        let policy = backup_table_policy(&table);
        entries.push(BackupRecoveryInventoryEntry {
            table,
            owner: policy.owner.to_owned(),
            disposition: policy.disposition.to_owned(),
            coverage: policy.coverage.to_owned(),
            row_count,
            schema_covered: policy.schema_covered(),
            snapshot_covered: policy.snapshot_covered() || row_count == 0,
        });
    }

    let uncovered_required_table_count = entries
        .iter()
        .filter(|entry| entry.disposition == "export_restore_required" && !entry.schema_covered)
        .count();
    let uncovered_required_row_count = entries
        .iter()
        .filter(|entry| entry.disposition == "export_restore_required" && !entry.snapshot_covered)
        .map(|entry| entry.row_count)
        .sum();
    let unclassified_table_count = entries
        .iter()
        .filter(|entry| entry.disposition == "unclassified")
        .count();

    Ok(BackupRecoveryInventory {
        schema_coverage_complete: uncovered_required_table_count == 0
            && unclassified_table_count == 0,
        snapshot_coverage_complete: entries.iter().all(|entry| entry.snapshot_covered),
        uncovered_required_table_count: u32::try_from(uncovered_required_table_count)
            .unwrap_or(u32::MAX),
        uncovered_required_row_count,
        unclassified_table_count: u32::try_from(unclassified_table_count).unwrap_or(u32::MAX),
        entries,
    })
}

fn recovery_inventory_degradations(inventory: &BackupRecoveryInventory) -> Vec<BackupDegradation> {
    let mut degraded = Vec::new();
    if inventory.unclassified_table_count > 0 {
        degraded.push(BackupDegradation::with_severity(
            "backup_table_inventory_unclassified",
            "high",
            format!(
                "{} migrated table(s) have no backup disposition",
                inventory.unclassified_table_count
            ),
            "classify every migrated table before treating this backup as complete",
        ));
    }
    if inventory.uncovered_required_table_count > 0 {
        degraded.push(BackupDegradation::warning(
            "backup_schema_coverage_incomplete",
            format!(
                "{} export/restore-required table(s) are not implemented by the portable backup format",
                inventory.uncovered_required_table_count
            ),
            "inspect recoveryInventory.tables and add typed export/restore coverage for every not_implemented table",
        ));
    }
    if inventory.uncovered_required_row_count > 0 {
        let nonempty_tables = inventory
            .entries
            .iter()
            .filter(|entry| {
                entry.disposition == "export_restore_required" && !entry.snapshot_covered
            })
            .map(|entry| format!("{}={}", entry.table, entry.row_count))
            .collect::<Vec<_>>()
            .join(", ");
        degraded.push(BackupDegradation::with_severity(
            "backup_source_rows_not_covered",
            "high",
            format!(
                "{} source-of-truth row(s) are not recoverable from this backup: {nonempty_tables}",
                inventory.uncovered_required_row_count
            ),
            "do not treat this artifact as a complete recovery point; implement the listed table exporters and recreate the backup",
        ));
    }
    degraded
}

fn reconcile_derived_recovery_inventory(
    inventory: &mut BackupRecoveryInventory,
    derived: &[BackupDerivedPayload],
) {
    let captured_task_episode_count = derived
        .iter()
        .filter(|asset| {
            asset.report.kind == "lab_episode"
                && asset.report.path.starts_with("derived/lab/episodes/")
        })
        .count() as u64;
    let captured_session_count =
        captured_derived_record_count(derived, "cass_sessions", "sessions");
    let captured_evidence_count =
        captured_derived_record_count(derived, "cass_evidence_spans", "evidenceSpans");

    for (table, captured_count) in [
        ("task_episodes", captured_task_episode_count),
        ("sessions", captured_session_count),
        ("evidence_spans", captured_evidence_count),
    ] {
        if let Some(entry) = inventory
            .entries
            .iter_mut()
            .find(|entry| entry.table == table)
        {
            entry.snapshot_covered = captured_count == entry.row_count;
        }
    }

    inventory.uncovered_required_table_count = u32::try_from(
        inventory
            .entries
            .iter()
            .filter(|entry| entry.disposition == "export_restore_required" && !entry.schema_covered)
            .count(),
    )
    .unwrap_or(u32::MAX);
    inventory.uncovered_required_row_count = inventory
        .entries
        .iter()
        .filter(|entry| entry.disposition == "export_restore_required" && !entry.snapshot_covered)
        .map(|entry| entry.row_count)
        .sum();
    inventory.schema_coverage_complete =
        inventory.uncovered_required_table_count == 0 && inventory.unclassified_table_count == 0;
    inventory.snapshot_coverage_complete =
        inventory.entries.iter().all(|entry| entry.snapshot_covered);
}

fn captured_derived_record_count(
    derived: &[BackupDerivedPayload],
    kind: &str,
    records_field: &str,
) -> u64 {
    derived
        .iter()
        .filter(|asset| asset.report.kind == kind)
        .filter_map(|asset| serde_json::from_slice::<JsonValue>(&asset.bytes).ok())
        .filter_map(|value| {
            value
                .get(records_field)
                .and_then(JsonValue::as_array)
                .map(|records| u64::try_from(records.len()).unwrap_or(u64::MAX))
        })
        .fold(0u64, u64::saturating_add)
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
        // Exit-10 storeless-miss contract: an addressed-but-absent store is an
        // addressing miss, not a storage failure.
        return Err(crate::core::storeless_workspace_error(&database_path));
    }

    let database_config = if options.dry_run {
        DatabaseConfig::read_only_file(database_path.clone())
    } else {
        DatabaseConfig::file(database_path.clone())
    };
    let connection = DbConnection::open(database_config).map_err(|error| DomainError::Storage {
        message: error.to_string(),
        repair: Some(INIT_AND_MIGRATE_REPAIR_COMMAND.to_owned()),
    })?;
    let backup_id = BackupId::now().to_string();
    let backup_root = backup_root(options, &workspace_path);
    let backup_path = backup_root.join(&backup_id);
    let records_path = backup_path.join(RECORDS_FILE);
    let manifest_path = backup_path.join(MANIFEST_FILE);
    let created_at = Utc::now().to_rfc3339();
    let mut degraded = backup_degradations(
        &workspace_path,
        options.include_derived,
        options.include_graph_cache,
    );
    // Durable history and its coverage counts must describe the same database
    // snapshot as the memory records. The optional flags only select caches.
    let (export_data, mut recovery_inventory, derived_payloads, mesh) =
        with_backup_read_snapshot(&connection, || {
            let workspace = load_workspace(&connection, &workspace_path)?;
            let export_data = load_export_data_in_current_snapshot(&connection, workspace)?;
            let inventory = build_recovery_inventory(&connection)?;
            let workspace_id = &export_data.workspace.workspace_id;
            let memory_ids =
                backup_memory_id_mapping(&export_data.memories, options.redaction_level)?;
            let mut payloads = Vec::new();
            collect_task_episode_payloads(
                &connection,
                workspace_id,
                &created_at,
                options.redaction_level,
                &memory_ids,
                &mut degraded,
                &mut payloads,
            );
            collect_cass_payloads(
                &connection,
                workspace_id,
                &created_at,
                options.redaction_level,
                &memory_ids,
                &mut degraded,
                &mut payloads,
            );
            if options.include_derived {
                payloads.extend(collect_derived_payloads(
                    &connection,
                    &workspace_path,
                    workspace_id,
                    &created_at,
                    &mut degraded,
                ));
            } else if options.include_graph_cache {
                payloads.extend(collect_graph_cache_payloads(
                    &connection,
                    workspace_id,
                    &created_at,
                    &mut degraded,
                ));
            }
            let mesh = backup_mesh_summary(&connection, workspace_id, &mut degraded);
            Ok((export_data, inventory, payloads, mesh))
        })?;
    degraded.extend(redaction_pattern_degradations(
        &export_data,
        options.redaction_level,
    ));
    let derived_reports = derived_payloads
        .iter()
        .map(|payload| payload.report.clone())
        .collect::<Vec<_>>();
    reconcile_derived_recovery_inventory(&mut recovery_inventory, &derived_payloads);
    degraded.extend(recovery_inventory_degradations(&recovery_inventory));

    // TC-D14: a store-auth fault must not block the backup — the artifact
    // ships unauthenticated with a high degraded entry, and import then
    // refuses native `human_explicit` trust instead of trusting the header.
    // A dry-run must not initialize the key store merely to preview an
    // artifact, so it only opens an already-existing root.
    let store_auth = load_store_auth_for_backup(&workspace_path, options.dry_run, &mut degraded);

    let (records_bytes, stats) = render_records(
        &backup_id,
        &created_at,
        options.redaction_level,
        &export_data,
        store_auth.as_ref(),
        &mut degraded,
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
        } else if recovery_inventory.snapshot_coverage_complete {
            "completed".to_owned()
        } else {
            "partial".to_owned()
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
        include_graph_cache: options.include_graph_cache,
        graph_cache_schema_version: connection.schema_version().ok().flatten(),
        total_records: stats.total_records,
        memory_count: stats.memory_count,
        link_count: stats.link_count,
        tag_count: stats.tag_count,
        audit_count: stats.audit_count,
        verification_status: if !recovery_inventory.snapshot_coverage_complete {
            "incomplete_source_coverage".to_owned()
        } else if options.dry_run {
            "not_checked".to_owned()
        } else {
            "verified".to_owned()
        },
        recovery_inventory,
        artifacts: vec![planned_records_artifact],
        derived: derived_reports,
        degraded,
    };

    let manifest_json = manifest_json(&report, &created_at, None, &mesh);
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

fn load_store_auth_for_backup(
    workspace_path: &Path,
    dry_run: bool,
    degraded: &mut Vec<BackupDegradation>,
) -> Option<StoreAuthRoot> {
    let keys_dir = workspace_keys_dir(workspace_path);
    let result = if dry_run {
        StoreAuthRoot::open(&keys_dir)
    } else {
        StoreAuthRoot::open_or_create(&keys_dir)
    };

    match result {
        Ok(root) => Some(root),
        Err(StoreAuthError::NotInitialized { .. }) if dry_run => None,
        Err(error) => {
            degraded.push(BackupDegradation::with_severity(
                error.degraded_code(),
                "high",
                error.message(),
                error.repair(),
            ));
            None
        }
    }
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

    if backup_list_root_exists(&backup_root)? {
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

        for path in backup_paths {
            let Some(backup_path) = backup_list_child_dir(path, &mut degraded)? else {
                continue;
            };
            let manifest_path = backup_path.join(MANIFEST_FILE);
            if !backup_list_manifest_is_file(&backup_path, &manifest_path, &mut degraded)? {
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

fn backup_list_root_exists(path: &Path) -> Result<bool, DomainError> {
    if let Some(symlink_path) = backup_list_symlink_component(path)? {
        return Err(DomainError::Storage {
            message: format!(
                "backup root '{}' traverses symbolic link '{}'",
                path.display(),
                symlink_path.display()
            ),
            repair: Some("choose a real, non-symlink directory with --output-dir".to_owned()),
        });
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false);
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "failed to inspect backup root '{}': {error}",
                    path.display()
                ),
                repair: Some("choose a readable --output-dir".to_owned()),
            });
        }
    };
    if !metadata.file_type().is_dir() {
        return Err(DomainError::Storage {
            message: format!("backup root '{}' is not a directory", path.display()),
            repair: Some("choose a directory with --output-dir".to_owned()),
        });
    }
    Ok(true)
}

fn backup_list_child_dir(
    path: PathBuf,
    degraded: &mut Vec<BackupDegradation>,
) -> Result<Option<PathBuf>, DomainError> {
    if let Some(symlink_path) = backup_list_symlink_component(&path)? {
        degraded.push(BackupDegradation::warning(
            "backup_manifest_unreadable",
            format!(
                "backup directory '{}' was skipped because it traverses symbolic link '{}'",
                path.display(),
                symlink_path.display()
            ),
            "replace symlinked backup entries with self-contained backup directories",
        ));
        return Ok(None);
    }

    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "backup_manifest_unreadable",
                format!(
                    "backup directory '{}' could not be inspected: {error}",
                    path.display()
                ),
                "run ee backup inspect on the directory for a focused diagnostic",
            ));
            return Ok(None);
        }
    };

    Ok(metadata.file_type().is_dir().then_some(path))
}

fn backup_list_manifest_is_file(
    backup_path: &Path,
    manifest_path: &Path,
    degraded: &mut Vec<BackupDegradation>,
) -> Result<bool, DomainError> {
    if backup_relative_path_has_symlink_component(backup_path, Path::new(MANIFEST_FILE))? {
        degraded.push(BackupDegradation::warning(
            "backup_manifest_unreadable",
            format!(
                "backup manifest path '{}' traverses a symbolic link",
                manifest_path.display()
            ),
            "run ee backup inspect on the directory for a focused diagnostic",
        ));
        return Ok(false);
    }

    match fs::symlink_metadata(manifest_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => {
            degraded.push(BackupDegradation::warning(
                "backup_manifest_unreadable",
                format!(
                    "backup manifest path '{}' is not a regular file",
                    manifest_path.display()
                ),
                "run ee backup inspect on the directory for a focused diagnostic",
            ));
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            degraded.push(BackupDegradation::warning(
                "backup_manifest_missing",
                format!(
                    "backup directory '{}' has no manifest.json",
                    backup_path.display()
                ),
                "run ee backup inspect on the directory or remove it manually after review",
            ));
            Ok(false)
        }
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "backup_manifest_unreadable",
                format!(
                    "backup manifest path '{}' could not be inspected: {error}",
                    manifest_path.display()
                ),
                "run ee backup inspect on the directory for a focused diagnostic",
            ));
            Ok(false)
        }
    }
}

fn backup_list_symlink_component(path: &Path) -> Result<Option<PathBuf>, DomainError> {
    super::path_safety::first_existing_symlink_component(path).map_err(|error| {
        DomainError::Storage {
            message: format!(
                "failed to inspect backup list path '{}': {error}",
                path.display()
            ),
            repair: Some(
                "inspect filesystem permissions or choose another --output-dir".to_owned(),
            ),
        }
    })
}

/// Inspect one backup manifest without checking artifact hashes.
///
/// # Errors
///
/// Returns a [`DomainError`] if the manifest cannot be read or parsed as JSON.
pub fn inspect_backup(options: &BackupInspectOptions) -> Result<BackupInspectReport, DomainError> {
    let backup_path = normalize_backup_input_path(&options.backup_path)?;
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
    let backup_path = normalize_backup_input_path(&options.backup_path)?;
    let inspect = inspect_backup(&BackupInspectOptions {
        backup_path: backup_path.clone(),
    })?;
    let mut issues = inspect.issues;
    let mut checked_artifacts = Vec::new();
    let mut checked_derived = Vec::new();

    // The manifest cannot list itself (it is rendered before its own hash
    // exists), but verify still content-addresses it via inspect; report it as
    // a checked artifact so the verify projection covers every required file.
    if !inspect
        .artifacts
        .iter()
        .any(|artifact| artifact.path == MANIFEST_FILE)
    {
        let manifest_path = backup_path.join(MANIFEST_FILE);
        checked_artifacts.push(BackupArtifactReport {
            path: MANIFEST_FILE.to_owned(),
            kind: "manifest".to_owned(),
            hash: Some(inspect.manifest_hash.clone()),
            size_bytes: Some(file_size(&manifest_path)?),
            required: true,
        });
    }

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
        match &artifact.hash {
            Some(expected_hash) if &actual_hash != expected_hash => {
                issues.push(
                    BackupVerificationIssue::error(
                        "artifact_hash_mismatch",
                        "backup artifact hash does not match manifest",
                    )
                    .with_path(artifact.path.clone())
                    .with_expected_actual(expected_hash.clone(), actual_hash.clone()),
                );
            }
            Some(_) => {}
            None => {
                issues.push(
                    BackupVerificationIssue::error(
                        "artifact_hash_missing",
                        "backup artifact manifest entry is missing a content hash",
                    )
                    .with_path(artifact.path.clone()),
                );
            }
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
        match &derived.hash {
            Some(expected_hash) if &actual_hash != expected_hash => {
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
            Some(_) => {}
            None => {
                issues.push(
                    BackupVerificationIssue::high(
                        "derived_asset_hash_missing",
                        "derived backup asset manifest entry is missing a content hash",
                    )
                    .with_path(derived.path.clone()),
                );
            }
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
    let backup_path = normalize_backup_input_path(&options.backup_path)?;
    let side_path = normalize_restore_side_path(&options.side_path)?;
    ensure_side_path_outside_workspace(&workspace_path, &side_path)?;

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
    let mut next_actions = restore_base_next_actions(&inspect.backup_id, &side_path);

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
            restore_graph_cache: options.restore_graph_cache,
            imported_memory_count: 0,
            skipped_duplicate_count: 0,
            restored_task_episode_count: 0,
            restored_cass_session_count: 0,
            restored_evidence_span_count: 0,
            restored_graph_cache_count: 0,
            restored_derived: Vec::new(),
            issue_count: u32::try_from(verify.issues.len()).unwrap_or(u32::MAX),
            degraded: Vec::new(),
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
    let restore_degraded = restore_manifest_degradations(&manifest_bytes);
    if restore_degraded
        .iter()
        .any(|entry| entry.code == "mesh_restore_requires_repair")
    {
        next_actions.push(restore_mesh_doctor_next_action(&side_path));
    }
    write_new_file(&restore_manifest_path, &manifest_bytes)?;

    copy_new_file(&source_records_path, &restore_records_path)?;
    let restored_derived = copy_derived_artifacts_to_restore(
        &backup_path,
        &restore_artifact_dir,
        &side_path,
        &inspect,
    )?;
    restore_shard_fanout_assets(&side_path, &restored_derived)?;

    let import_report = import_verified_backup_jsonl_records(&JsonlImportOptions {
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
    let restored_task_episode_count =
        restore_task_episode_assets(&restored_database_path, &restored_derived)?;
    let (restored_cass_session_count, restored_evidence_span_count) =
        restore_cass_assets(&restored_database_path, &restored_derived)?;
    let graph_cache_restored_count = if options.restore_graph_cache {
        restore_graph_cache_assets(&restored_database_path, &restored_derived)?
    } else {
        0
    };
    let restore_issue_count = import_report
        .issues
        .len()
        .saturating_add(verify.issues.len());
    let restore_status = if import_report.status == "completed"
        && verify.issues.is_empty()
        && restore_degraded.is_empty()
    {
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
        restore_graph_cache: options.restore_graph_cache,
        imported_memory_count: import_report.memories_imported,
        skipped_duplicate_count: import_report.memories_skipped_duplicate,
        restored_task_episode_count,
        restored_cass_session_count,
        restored_evidence_span_count,
        restored_graph_cache_count: graph_cache_restored_count,
        restored_derived,
        issue_count: u32::try_from(restore_issue_count).unwrap_or(u32::MAX),
        degraded: restore_degraded,
        next_actions,
    })
}

fn restore_base_next_actions(backup_id: &str, side_path: &Path) -> Vec<String> {
    vec![
        format!(
            "ee backup inspect {} --json",
            shell_quote_command_arg(backup_id)
        ),
        format!(
            "ee search \"<query>\" --workspace {} --json",
            shell_quote_path_arg(side_path)
        ),
    ]
}

fn restore_mesh_doctor_next_action(side_path: &Path) -> String {
    format!(
        "ee mesh doctor --workspace {} --json",
        shell_quote_path_arg(side_path)
    )
}

fn shell_quote_path_arg(path: &Path) -> String {
    let path_text = path.to_string_lossy();
    shell_quote_command_arg(path_text.as_ref())
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

fn restore_manifest_degradations(manifest_bytes: &[u8]) -> Vec<BackupDegradation> {
    let Ok(manifest) = serde_json::from_slice::<JsonValue>(manifest_bytes) else {
        return Vec::new();
    };
    let mut degraded = degradation_reports(&manifest)
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.code.as_str(),
                "backup_source_rows_not_covered" | "backup_table_inventory_unclassified"
            )
        })
        .collect::<Vec<_>>();
    let backup_schema_version = manifest
        .pointer("/graphCache/schemaVersion")
        .and_then(JsonValue::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    if let (Some(backup_schema_version), Some(current_schema_version)) = (
        backup_schema_version,
        crate::db::MIGRATIONS
            .last()
            .map(|migration| migration.version()),
    ) && backup_schema_version < current_schema_version
    {
        degraded.push(BackupDegradation::warning(
            "graph_cache_schema_older_than_binary",
            format!(
                "backup graph cache was captured at schema version {backup_schema_version}, while this binary restores with schema version {current_schema_version}"
            ),
            "restore imports records through the current migrations before replaying graph-cache assets; run ee db status --workspace <side-path> --json after restore to inspect the migrated database",
        ));
    }

    if manifest
        .pointer("/mesh/included")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        degraded.push(BackupDegradation::warning(
            "mesh_restore_requires_repair",
            "backup contains mesh coordination state; restored workspaces keep mesh sync disabled until peers are explicitly re-paired",
            "run ee mesh doctor --workspace <side-path> --json and re-pair peers before enabling mesh sync",
        ));
    }
    degraded
}

fn restore_shard_fanout_assets(
    side_path: &Path,
    restored_derived: &[BackupRestoredDerivedAssetReport],
) -> Result<(), DomainError> {
    let Some(manifest_asset) = restored_derived
        .iter()
        .find(|asset| asset.kind == "shard_fanout_manifest")
    else {
        return Ok(());
    };
    let manifest = read_restored_derived_json(manifest_asset)?;
    let catalog = required_object(&manifest, "catalog")?;
    let catalog_backup_path = required_json_str(catalog, "backupPath")?;
    let catalog_asset = restored_derived_asset(restored_derived, catalog_backup_path)?;
    let catalog_bytes =
        fs::read(&catalog_asset.restore_path).map_err(|error| DomainError::Import {
            message: format!(
                "restored shard fan-out catalog artifact '{}' could not be read: {error}",
                catalog_asset.restore_path
            ),
            repair: Some("verify the backup and retry restore with a fresh side path".to_owned()),
        })?;
    let side_ee_dir = side_path.join(WORKSPACE_MARKER);
    write_new_relative_file(&side_ee_dir, "catalog.db", &catalog_bytes)?;

    let shards = manifest
        .get("shards")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| DomainError::Import {
            message: "backup shard fan-out manifest is missing array field 'shards'".to_owned(),
            repair: Some("recreate the backup with shard fan-out derived assets".to_owned()),
        })?;
    for shard in shards {
        let shard_id = required_json_str(shard, "shardId")?;
        let shard_backup_path = required_json_str(shard, "backupPath")?;
        let shard_asset = restored_derived_asset(restored_derived, shard_backup_path)?;
        let shard_bytes =
            fs::read(&shard_asset.restore_path).map_err(|error| DomainError::Import {
                message: format!(
                    "restored shard fan-out shard artifact '{}' could not be read: {error}",
                    shard_asset.restore_path
                ),
                repair: Some(
                    "verify the backup and retry restore with a fresh side path".to_owned(),
                ),
            })?;
        write_new_relative_file(
            &side_ee_dir,
            &format!("shards/{}.db", safe_file_stem(shard_id)),
            &shard_bytes,
        )?;
    }
    Ok(())
}

fn restored_derived_asset<'a>(
    restored_derived: &'a [BackupRestoredDerivedAssetReport],
    backup_path: &str,
) -> Result<&'a BackupRestoredDerivedAssetReport, DomainError> {
    restored_derived
        .iter()
        .find(|asset| asset.path == backup_path)
        .ok_or_else(|| DomainError::Import {
            message: format!(
                "backup shard fan-out manifest references missing derived asset '{backup_path}'"
            ),
            repair: Some("recreate the backup with complete shard fan-out assets".to_owned()),
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
        if derived
            .path
            .rsplit('/')
            .next()
            .is_some_and(is_appledouble_file_name)
        {
            continue;
        }
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
        let metadata =
            fs::symlink_metadata(&source_path).map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to stat derived backup asset '{}': {error}",
                    source_path.display()
                ),
                repair: Some("verify the backup directory and retry restore".to_owned()),
            })?;
        const MAX_DERIVED_ASSET_BYTES: u64 = 250 * 1024 * 1024;
        if metadata.len() > MAX_DERIVED_ASSET_BYTES {
            return Err(DomainError::Storage {
                message: format!(
                    "derived backup asset '{}' exceeds maximum allowed size of {} bytes",
                    source_path.display(),
                    MAX_DERIVED_ASSET_BYTES
                ),
                repair: Some("inspect backup size constraints".to_owned()),
            });
        }
        let bytes = fs::read(&source_path).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to read derived backup asset '{}': {error}",
                source_path.display()
            ),
            repair: Some("verify the backup directory and retry restore".to_owned()),
        })?;
        let observed_hash = hash_bytes(&bytes);
        let expected_hash = derived.hash.as_deref().ok_or_else(|| DomainError::Import {
            message: format!(
                "derived backup asset '{}' is missing a manifest hash during restore",
                derived.path
            ),
            repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
        })?;
        let validation_status = if expected_hash == observed_hash {
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
        if expected_hash != observed_hash {
            return Err(DomainError::Import {
                message: format!(
                    "derived backup asset '{}' hash changed during restore: expected {}, observed {}",
                    derived.path, expected_hash, observed_hash
                ),
                repair: Some(
                    "rerun ee backup verify <backup-path> --json and restore from a trusted backup copy"
                        .to_owned(),
                ),
            });
        }
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

fn restore_task_episode_assets(
    restored_database_path: &Path,
    restored_derived: &[BackupRestoredDerivedAssetReport],
) -> Result<u32, DomainError> {
    let episode_assets = restored_derived
        .iter()
        .filter(|asset| {
            asset.kind == "lab_episode" && asset.path.starts_with("derived/lab/episodes/")
        })
        .collect::<Vec<_>>();
    if episode_assets.is_empty() {
        return Ok(0);
    }

    let connection = DbConnection::open(DatabaseConfig::file(restored_database_path.to_path_buf()))
        .map_err(|error| DomainError::Import {
            message: format!(
                "failed opening restored database '{}' for task-episode restore: {error}",
                restored_database_path.display()
            ),
            repair: Some("retry restore with a fresh --side-path".to_owned()),
        })?;
    connection.migrate().map_err(|error| DomainError::Import {
        message: format!(
            "failed preparing restored database '{}' for task-episode restore: {error}",
            restored_database_path.display()
        ),
        repair: Some("retry restore with a fresh --side-path".to_owned()),
    })?;
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| DomainError::Import {
            message: format!(
                "failed reading restored workspaces for task-episode restore: {error}"
            ),
            repair: Some("retry restore with a fresh --side-path".to_owned()),
        })?;

    let mut restored_count = 0u32;
    for asset in episode_assets {
        let value = read_restored_derived_json(asset)?;
        if value.get("schema").and_then(JsonValue::as_str)
            != Some("ee.backup.derived.lab_episode.v1")
        {
            return Err(DomainError::Import {
                message: format!(
                    "restored task-episode asset '{}' has an unsupported schema",
                    asset.path
                ),
                repair: Some(
                    "recreate the backup with ee backup create --include-derived".to_owned(),
                ),
            });
        }
        let episode = required_object(&value, "episode")?;
        let id = required_json_str(episode, "id")?;
        let source_workspace_id = episode.get("workspaceId").and_then(JsonValue::as_str);
        let workspace_id =
            remap_restored_workspace_id(&workspaces, source_workspace_id, "task episode")?;
        let retrieved_memory_ids = serde_json::from_value::<Vec<String>>(
            episode
                .get("retrievedMemoryIds")
                .cloned()
                .ok_or_else(|| missing_derived_field("retrievedMemoryIds"))?,
        )
        .map_err(|error| malformed_derived_field("retrievedMemoryIds", error))?;
        let actions = serde_json::from_value::<Vec<StoredEpisodeAction>>(
            episode
                .get("actions")
                .cloned()
                .ok_or_else(|| missing_derived_field("actions"))?,
        )
        .map_err(|error| malformed_derived_field("actions", error))?;
        let input = CreateTaskEpisodeInput {
            workspace_id,
            session_id: json_string(episode, "sessionId"),
            task_input: required_json_str(episode, "taskInput")?.to_owned(),
            retrieved_memory_ids,
            context_pack_id: json_string(episode, "contextPackId"),
            actions,
            outcome: required_json_str(episode, "outcome")?.to_owned(),
            outcome_details: json_string(episode, "outcomeDetails"),
            started_at: required_json_str(episode, "startedAt")?.to_owned(),
            ended_at: json_string(episode, "endedAt"),
            duration_ms: episode.get("durationMs").and_then(JsonValue::as_u64),
            agent: json_string(episode, "agent"),
            episode_hash: json_string(episode, "episodeHash"),
        };
        connection
            .insert_task_episode_with_created_at(
                id,
                &input,
                required_json_str(episode, "createdAt")?,
            )
            .map_err(|error| DomainError::Import {
                message: format!("failed restoring task episode '{id}': {error}"),
                repair: Some("restore to a fresh --side-path and retry".to_owned()),
            })?;
        restored_count = restored_count.saturating_add(1);
    }
    Ok(restored_count)
}

fn remap_restored_workspace_id(
    workspaces: &[crate::db::StoredWorkspace],
    source_workspace_id: Option<&str>,
    entity: &str,
) -> Result<Option<String>, DomainError> {
    let Some(source_workspace_id) = source_workspace_id else {
        return Ok(None);
    };
    if let Some(workspace) = workspaces
        .iter()
        .find(|workspace| workspace.id == source_workspace_id)
    {
        return Ok(Some(workspace.id.clone()));
    }
    if let [workspace] = workspaces {
        return Ok(Some(workspace.id.clone()));
    }
    Err(DomainError::Import {
        message: format!(
            "{entity} references workspace '{source_workspace_id}', but the restored database has no unambiguous matching workspace"
        ),
        repair: Some("restore to a fresh --side-path and inspect records.jsonl".to_owned()),
    })
}

fn restore_cass_assets(
    restored_database_path: &Path,
    restored_derived: &[BackupRestoredDerivedAssetReport],
) -> Result<(u32, u32), DomainError> {
    let mut session_assets = restored_derived
        .iter()
        .filter(|asset| asset.kind == "cass_sessions")
        .collect::<Vec<_>>();
    let mut evidence_assets = restored_derived
        .iter()
        .filter(|asset| asset.kind == "cass_evidence_spans")
        .collect::<Vec<_>>();
    if session_assets.is_empty() && evidence_assets.is_empty() {
        return Ok((0, 0));
    }
    session_assets.sort_by(|left, right| left.path.cmp(&right.path));
    evidence_assets.sort_by(|left, right| left.path.cmp(&right.path));

    let connection = DbConnection::open(DatabaseConfig::file(restored_database_path.to_path_buf()))
        .map_err(|error| DomainError::Import {
            message: format!(
                "failed opening restored database '{}' for CASS recovery: {error}",
                restored_database_path.display()
            ),
            repair: Some("retry restore with a fresh --side-path".to_owned()),
        })?;
    connection.migrate().map_err(|error| DomainError::Import {
        message: format!(
            "failed preparing restored database '{}' for CASS recovery: {error}",
            restored_database_path.display()
        ),
        repair: Some("retry restore with a fresh --side-path".to_owned()),
    })?;
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| DomainError::Import {
            message: format!("failed reading restored workspaces for CASS recovery: {error}"),
            repair: Some("retry restore with a fresh --side-path".to_owned()),
        })?;

    let mut sessions = Vec::new();
    for (expected_index, asset) in session_assets.into_iter().enumerate() {
        let value = read_restored_derived_json(asset)?;
        let chunk = serde_json::from_value::<BackupCassSessionChunk>(value)
            .map_err(|error| malformed_cass_recovery_asset(asset, error))?;
        if chunk.schema != "ee.backup.derived.cass_sessions.v1"
            || chunk.source_locator_policy != "omitted_host_local"
            || chunk.chunk_index != u32::try_from(expected_index).unwrap_or(u32::MAX)
        {
            return Err(unsupported_cass_recovery_asset(asset));
        }
        for record in chunk.sessions {
            let workspace_id = remap_restored_workspace_id(
                &workspaces,
                Some(&record.workspace_id),
                "CASS session",
            )?
            .ok_or_else(|| unsupported_cass_recovery_asset(asset))?;
            sessions.push(record.into_restored(workspace_id));
        }
    }

    let mut evidence = Vec::new();
    for (expected_index, asset) in evidence_assets.into_iter().enumerate() {
        let value = read_restored_derived_json(asset)?;
        let chunk = serde_json::from_value::<BackupCassEvidenceChunk>(value)
            .map_err(|error| malformed_cass_recovery_asset(asset, error))?;
        if chunk.schema != "ee.backup.derived.cass_evidence_spans.v1"
            || chunk.chunk_index != u32::try_from(expected_index).unwrap_or(u32::MAX)
        {
            return Err(unsupported_cass_recovery_asset(asset));
        }
        for record in chunk.evidence_spans {
            let workspace_id = remap_restored_workspace_id(
                &workspaces,
                Some(&record.workspace_id),
                "CASS evidence span",
            )?
            .ok_or_else(|| unsupported_cass_recovery_asset(asset))?;
            evidence.push(record.into_restored(workspace_id));
        }
    }

    connection
        .with_transaction(|| {
            for session in &sessions {
                connection.insert_session_for_recovery(session)?;
            }
            for span in &evidence {
                connection.insert_evidence_span_for_recovery(span)?;
            }
            Ok(())
        })
        .map_err(|error| DomainError::Import {
            message: format!("failed restoring portable CASS rows: {error}"),
            repair: Some("restore to a fresh --side-path and retry".to_owned()),
        })?;

    Ok((
        u32::try_from(sessions.len()).unwrap_or(u32::MAX),
        u32::try_from(evidence.len()).unwrap_or(u32::MAX),
    ))
}

fn malformed_cass_recovery_asset(
    asset: &BackupRestoredDerivedAssetReport,
    error: serde_json::Error,
) -> DomainError {
    DomainError::Import {
        message: format!(
            "restored CASS asset '{}' has malformed typed rows: {error}",
            asset.path
        ),
        repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
    }
}

fn unsupported_cass_recovery_asset(asset: &BackupRestoredDerivedAssetReport) -> DomainError {
    DomainError::Import {
        message: format!(
            "restored CASS asset '{}' has an unsupported schema, chunk order, or source-locator policy",
            asset.path
        ),
        repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
    }
}

fn missing_derived_field(field: &str) -> DomainError {
    DomainError::Import {
        message: format!("backup derived task episode is missing field '{field}'"),
        repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
    }
}

fn malformed_derived_field(field: &str, error: serde_json::Error) -> DomainError {
    DomainError::Import {
        message: format!("backup derived task episode field '{field}' is malformed: {error}"),
        repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
    }
}

fn restore_graph_cache_assets(
    restored_database_path: &Path,
    restored_derived: &[BackupRestoredDerivedAssetReport],
) -> Result<u32, DomainError> {
    if !restored_derived
        .iter()
        .any(|asset| asset.kind.starts_with("graph_"))
    {
        return Ok(0);
    }

    let connection = DbConnection::open(DatabaseConfig::file(restored_database_path.to_path_buf()))
        .map_err(|error| DomainError::Import {
            message: format!(
                "failed opening restored database '{}' for graph cache restore: {error}",
                restored_database_path.display()
            ),
            repair: Some("retry restore with a fresh --side-path".to_owned()),
        })?;
    connection.migrate().map_err(|error| DomainError::Import {
        message: format!(
            "failed preparing restored database '{}' for graph cache restore: {error}",
            restored_database_path.display()
        ),
        repair: Some(
            "inspect restored records.jsonl and retry with a fresh --side-path".to_owned(),
        ),
    })?;
    let restored_workspace_id =
        restore_graph_cache_workspace_id(&connection, restored_database_path, restored_derived)?;

    let mut restored_rows = 0u32;
    for asset in restored_derived
        .iter()
        .filter(|asset| asset.kind == "graph_snapshot")
    {
        let value = read_restored_derived_json(asset)?;
        restore_graph_snapshot_asset(&connection, &restored_workspace_id, &value)?;
        restored_rows = restored_rows.saturating_add(1);
    }
    for asset in restored_derived
        .iter()
        .filter(|asset| asset.kind == "graph_algorithm_witness")
    {
        let value = read_restored_derived_json(asset)?;
        restore_graph_algorithm_witness_asset(&connection, &restored_workspace_id, &value)?;
        restored_rows = restored_rows.saturating_add(1);
    }
    for asset in restored_derived
        .iter()
        .filter(|asset| asset.kind == "graph_algorithm_result")
    {
        let value = read_restored_derived_json(asset)?;
        restore_graph_algorithm_result_asset(&connection, &restored_workspace_id, &value)?;
        restored_rows = restored_rows.saturating_add(1);
    }
    Ok(restored_rows)
}

fn restore_graph_cache_workspace_id(
    connection: &DbConnection,
    restored_database_path: &Path,
    restored_derived: &[BackupRestoredDerivedAssetReport],
) -> Result<String, DomainError> {
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| DomainError::Import {
            message: format!("failed reading restored workspace for graph cache restore: {error}"),
            repair: Some(
                "inspect restored records.jsonl and retry with a fresh --side-path".to_owned(),
            ),
        })?;
    match workspaces.as_slice() {
        [] => restore_graph_cache_workspace_from_assets(
            connection,
            restored_database_path,
            restored_derived,
        ),
        [workspace] => Ok(workspace.id.clone()),
        _ => {
            let asset_workspace_id = restored_derived
                .iter()
                .filter(|asset| asset.kind.starts_with("graph_"))
                .find_map(|asset| {
                    read_restored_derived_json(asset)
                        .ok()
                        .and_then(|value| json_string(&value, "workspaceId"))
                });
            if let Some(asset_workspace_id) = asset_workspace_id {
                if workspaces
                    .iter()
                    .any(|workspace| workspace.id == asset_workspace_id)
                {
                    return Ok(asset_workspace_id);
                }
            }
            crate::core::workspace::pick_workspace_row(connection, workspaces)
                .map(|workspace| workspace.id)
                .map_err(|error| DomainError::Import {
                    message: format!(
                        "failed choosing restored workspace for graph cache restore: {}",
                        error.message()
                    ),
                    repair: Some(
                        "inspect restored records.jsonl and retry with a fresh --side-path"
                            .to_owned(),
                    ),
                })
        }
    }
}

fn restore_graph_cache_workspace_from_assets(
    connection: &DbConnection,
    restored_database_path: &Path,
    restored_derived: &[BackupRestoredDerivedAssetReport],
) -> Result<String, DomainError> {
    let workspace_id = restored_derived
        .iter()
        .filter(|asset| asset.kind.starts_with("graph_"))
        .find_map(|asset| {
            read_restored_derived_json(asset)
                .ok()
                .and_then(|value| json_string(&value, "workspaceId"))
        })
        .ok_or_else(|| DomainError::Import {
            message: "restored database has no workspace row or graph-cache workspace id"
                .to_owned(),
            repair: Some(
                "inspect restored graph cache assets and retry with a fresh --side-path".to_owned(),
            ),
        })?;
    let restored_workspace_path = restored_database_path
        .parent()
        .and_then(Path::parent)
        .map_or_else(
            || restored_database_path.display().to_string(),
            |path| path.display().to_string(),
        );
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: restored_workspace_path,
                name: Some("restored backup".to_owned()),
            },
        )
        .map_err(|error| DomainError::Import {
            message: format!(
                "failed creating restored workspace row for graph cache restore: {error}"
            ),
            repair: Some(
                "inspect restored graph cache assets and retry with a fresh --side-path".to_owned(),
            ),
        })?;
    Ok(workspace_id)
}

fn read_restored_derived_json(
    asset: &BackupRestoredDerivedAssetReport,
) -> Result<JsonValue, DomainError> {
    let bytes = fs::read(&asset.restore_path).map_err(|error| DomainError::Import {
        message: format!(
            "failed reading restored derived asset '{}': {error}",
            asset.restore_path
        ),
        repair: Some("run ee backup verify <id-or-path> --json and retry restore".to_owned()),
    })?;
    serde_json::from_slice(&bytes).map_err(|error| DomainError::Import {
        message: format!(
            "restored derived asset '{}' is not valid JSON: {error}",
            asset.restore_path
        ),
        repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
    })
}

fn restore_graph_snapshot_asset(
    connection: &DbConnection,
    restored_workspace_id: &str,
    value: &JsonValue,
) -> Result<(), DomainError> {
    let snapshot = required_object(value, "snapshot")?;
    let id = required_json_str(snapshot, "id")?;
    let graph_type = required_json_str(snapshot, "graphType")?
        .parse::<GraphSnapshotType>()
        .map_err(|error| DomainError::Import {
            message: format!("backup graph snapshot '{id}' has invalid graph type: {error}"),
            repair: Some("recreate the backup after rebuilding graph snapshots".to_owned()),
        })?;
    let metrics_json = serde_json::to_string(snapshot.get("metrics").unwrap_or(&JsonValue::Null))
        .map_err(|error| DomainError::Import {
        message: format!("backup graph snapshot '{id}' metrics are not serializable: {error}"),
        repair: Some("recreate the backup after rebuilding graph snapshots".to_owned()),
    })?;
    connection
        .insert_graph_snapshot(
            id,
            &CreateGraphSnapshotInput {
                workspace_id: restored_workspace_id.to_owned(),
                snapshot_version: required_json_u32(snapshot, "snapshotVersion")?,
                schema_version: required_json_str(snapshot, "schemaVersion")?.to_owned(),
                graph_type,
                node_count: required_json_u32(snapshot, "nodeCount")?,
                edge_count: required_json_u32(snapshot, "edgeCount")?,
                metrics_json,
                content_hash: required_json_str(snapshot, "contentHash")?.to_owned(),
                source_generation: required_json_u32(snapshot, "sourceGeneration")?,
                expires_at: snapshot
                    .get("expiresAt")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned),
            },
        )
        .map_err(|error| DomainError::Import {
            message: format!("failed restoring graph snapshot '{id}': {error}"),
            repair: Some("restore to a fresh --side-path and retry".to_owned()),
        })
}

fn restore_graph_algorithm_witness_asset(
    connection: &DbConnection,
    restored_workspace_id: &str,
    value: &JsonValue,
) -> Result<(), DomainError> {
    let witness = required_object(value, "witness")?;
    let snapshot_id = required_json_str(witness, "snapshotId")?.to_owned();
    connection
        .insert_graph_algorithm_witness(&CreateGraphAlgorithmWitnessInput {
            workspace_id: restored_workspace_id.to_owned(),
            snapshot_id: snapshot_id.clone(),
            algorithm: required_json_str(witness, "algorithm")?.to_owned(),
            params_json: json_field_to_string(witness, "params")?,
            witness_json: json_field_to_string(witness, "witness")?,
        })
        .map_err(|error| DomainError::Import {
            message: format!(
                "failed restoring graph algorithm witness for snapshot '{snapshot_id}': {error}"
            ),
            repair: Some("restore to a fresh --side-path and retry".to_owned()),
        })
}

fn restore_graph_algorithm_result_asset(
    connection: &DbConnection,
    restored_workspace_id: &str,
    value: &JsonValue,
) -> Result<(), DomainError> {
    let result = required_object(value, "result")?;
    let snapshot_id = required_json_str(result, "snapshotId")?.to_owned();
    connection
        .upsert_graph_algorithm_result(&CreateGraphAlgorithmResultInput {
            workspace_id: restored_workspace_id.to_owned(),
            snapshot_id: snapshot_id.clone(),
            algorithm: required_json_str(result, "algorithm")?.to_owned(),
            params_hash: required_json_str(result, "paramsHash")?.to_owned(),
            result_json: json_field_to_string(result, "result")?,
            ttl_seconds: required_json_u64(result, "ttlSeconds")?,
        })
        .map_err(|error| DomainError::Import {
            message: format!(
                "failed restoring graph algorithm result for snapshot '{snapshot_id}': {error}"
            ),
            repair: Some("restore to a fresh --side-path and retry".to_owned()),
        })
}

fn required_object<'a>(value: &'a JsonValue, field: &str) -> Result<&'a JsonValue, DomainError> {
    value
        .get(field)
        .filter(|child| child.is_object())
        .ok_or_else(|| DomainError::Import {
            message: format!("backup derived graph asset missing object field '{field}'"),
            repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
        })
}

fn required_json_str<'a>(value: &'a JsonValue, field: &str) -> Result<&'a str, DomainError> {
    value
        .get(field)
        .and_then(JsonValue::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| DomainError::Import {
            message: format!("backup derived graph asset missing string field '{field}'"),
            repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
        })
}

fn required_json_u32(value: &JsonValue, field: &str) -> Result<u32, DomainError> {
    let raw = required_json_u64(value, field)?;
    u32::try_from(raw).map_err(|_| DomainError::Import {
        message: format!("backup derived graph asset field '{field}' does not fit u32"),
        repair: Some("recreate the backup after rebuilding graph snapshots".to_owned()),
    })
}

fn required_json_u64(value: &JsonValue, field: &str) -> Result<u64, DomainError> {
    value
        .get(field)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| DomainError::Import {
            message: format!("backup derived graph asset missing integer field '{field}'"),
            repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
        })
}

fn json_field_to_string(value: &JsonValue, field: &str) -> Result<String, DomainError> {
    let Some(child) = value.get(field) else {
        return Err(DomainError::Import {
            message: format!("backup derived graph asset missing JSON field '{field}'"),
            repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
        });
    };
    serde_json::to_string(child).map_err(|error| DomainError::Import {
        message: format!("backup derived graph asset field '{field}' is not serializable: {error}"),
        repair: Some("recreate the backup with ee backup create --include-derived".to_owned()),
    })
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
    let verification_status = json_string(verification, "status");
    if verification_status.as_deref() == Some("incomplete_source_coverage") {
        issues.push(BackupVerificationIssue::warning(
            "backup_source_coverage_incomplete",
            "backup integrity is verifiable, but the recovery inventory reports source-of-truth rows that are not represented in restore artifacts",
        ));
    }
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
        verification_status,
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
    let requested = crate::core::workspace::stable_workspace_id(workspace_path);
    crate::core::workspace::select_existing_workspace_row(connection, &requested, &[workspace_path])
        .map_err(|error| DomainError::Storage {
            message: error.message(),
            repair: Some(INIT_AND_MIGRATE_REPAIR_COMMAND.to_owned()),
        })?
        .ok_or_else(|| DomainError::NotFound {
            resource: "workspace".to_owned(),
            id: workspace_path.to_string_lossy().into_owned(),
            repair: Some("ee init --workspace .".to_owned()),
        })
}

#[cfg(test)]
fn load_export_data(
    connection: &DbConnection,
    workspace: crate::db::StoredWorkspace,
) -> Result<BackupExportData, DomainError> {
    with_backup_read_snapshot(connection, || {
        load_export_data_in_current_snapshot(connection, workspace)
    })
}

fn with_backup_read_snapshot<T>(
    connection: &DbConnection,
    load: impl FnOnce() -> Result<T, DomainError>,
) -> Result<T, DomainError> {
    connection
        .begin_read_snapshot()
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee db check --workspace .".to_owned()),
        })?;
    let result = load();
    match result {
        Ok(data) => {
            connection
                .commit_read_snapshot()
                .map_err(|error| DomainError::Storage {
                    message: error.to_string(),
                    repair: Some("ee db check --workspace .".to_owned()),
                })?;
            Ok(data)
        }
        Err(error) => {
            if let Err(rollback_error) = connection.rollback_read_snapshot() {
                tracing::error!(
                    error = %error.message(),
                    rollback_error = %rollback_error,
                    "failed to roll back backup export read snapshot"
                );
            }
            Err(error)
        }
    }
}

fn load_export_data_in_current_snapshot(
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
    let mut tags_by_memory = memories
        .iter()
        .map(|memory| (memory.id.clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for memory_chunk in memories.chunks(128) {
        let ids = memory_chunk
            .iter()
            .map(|memory| memory.id.as_str())
            .collect::<Vec<_>>();
        let tags =
            connection
                .get_memory_tags_batch(&ids)
                .map_err(|error| DomainError::Storage {
                    message: error.to_string(),
                    repair: Some("ee db check --workspace .".to_owned()),
                })?;
        tags_by_memory.extend(tags);
    }
    // The ledger is keyed by revision-stable logical identity. Export its
    // slot exactly once, on the current head; attaching the same slot to every
    // historical revision makes restore attempt duplicate primary keys and
    // misrepresents revisions as sibling attempts.
    let current_memory_ids = memories
        .iter()
        .filter(|memory| memory.valid_to.is_none())
        .map(|memory| memory.id.clone())
        .collect::<Vec<_>>();
    let attempt_family_batch = connection
        .get_memory_attempt_family_details_batch_in_current_snapshot(&current_memory_ids)
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee db check --workspace .".to_owned()),
        })?;
    let attempt_families_by_memory = attempt_family_batch
        .by_memory_id
        .into_iter()
        .map(|(memory_id, details)| {
            (
                memory_id,
                crate::models::ExportAttemptFamilyRecord {
                    family_id: details.family.family_id,
                    declared_size: details.family.declared_size,
                    attempt_index: details.family.attempt_index,
                    disposition: details.family.disposition,
                    origin: details.origin,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
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
        .filter(|link| {
            crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
        })
        .collect::<Vec<_>>();
    let audits = connection
        .list_audit_entries(Some(&workspace.id), None)
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee db check --workspace .".to_owned()),
        })?;
    let graph_fields_by_memory =
        export_memory_graph_fields_by_id(connection, &workspace.id, &memories, &links, &audits)?;

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
        graph_fields_by_memory,
        attempt_families_by_memory,
    })
}

fn render_records(
    backup_id: &str,
    created_at: &str,
    redaction_level: RedactionLevel,
    data: &BackupExportData,
    store_auth: Option<&StoreAuthRoot>,
    degraded: &mut Vec<BackupDegradation>,
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
                        data.graph_fields_by_memory.get(&memory.id),
                        data.attempt_families_by_memory.get(&memory.id),
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

        // MAC the canonical header over the records root accumulated from the
        // exact emitted (post-redaction) memory line bytes of this snapshot.
        let authentication = store_auth.and_then(|auth_root| {
            let (records_root, record_count) = exporter.finalize_records_root();
            let context = ArtifactContext {
                artifact_family: EXPORT_ARTIFACT_FAMILY,
                record_encoding_version: EXPORT_RECORD_ENCODING_V1,
                source_key_namespace: STORE_KEY_NAMESPACE_V1,
                workspace_scope: &data.workspace.workspace_id,
            };
            match authenticate_artifact(
                auth_root,
                MacDomain::NativeImportRecordsRoot,
                &context,
                &records_root,
                record_count,
            ) {
                Ok(header) => Some(header),
                Err(error) => {
                    degraded.push(BackupDegradation::with_severity(
                        error.degraded_code(),
                        "high",
                        error.message(),
                        error.repair(),
                    ));
                    None
                }
            }
        });

        let stats = exporter
            .write_footer(
                ExportFooter::builder()
                    .export_id(backup_id)
                    .completed_at(created_at)
                    .authentication(authentication)
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
    graph_fields: Option<&BackupMemoryGraphFields>,
    attempt_family: Option<&crate::models::ExportAttemptFamilyRecord>,
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
        .trust_class(memory.trust_class.clone())
        .created_at(memory.created_at.clone())
        .redacted(false);
    builder = builder.updated_at(memory.updated_at.clone());
    if let Some(trust_subclass) = &memory.trust_subclass {
        builder = builder.trust_subclass(trust_subclass.clone());
    }
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
    if let Some(fields) = graph_fields {
        builder = apply_backup_memory_graph_fields(builder, fields);
    }
    if let Some(family) = attempt_family {
        builder = builder.attempt_family(family.clone());
    }
    builder.build()
}

fn backup_memory_id_mapping(
    memories: &[StoredMemory],
    redaction_level: RedactionLevel,
) -> Result<BTreeMap<String, String>, DomainError> {
    let mut exported_ids = BTreeSet::new();
    let mut restored_ids = BTreeSet::new();
    let mut mapping = BTreeMap::new();
    for memory in memories {
        let record = memory_record(memory, None, None, None)
            .map_err(export_build_error("build backup memory reference"))?;
        let record = redact_memory_record(record, redaction_level);
        let restored_id =
            import_memory_id(&record, redaction_level).map_err(|issue| DomainError::Storage {
                message: format!(
                    "backup memory reference cannot be restored: {}",
                    issue.message
                ),
                repair: Some(
                    "run ee db check --workspace . before recreating the backup".to_owned(),
                ),
            })?;
        if !exported_ids.insert(record.memory_id) || !restored_ids.insert(restored_id.clone()) {
            return Err(DomainError::Storage {
                message: "backup redaction maps distinct memories to the same identity".to_owned(),
                repair: Some("choose --redaction strict to redact secrets and paths while preserving distinct memory IDs".to_owned()),
            });
        }
        mapping.insert(memory.id.clone(), restored_id);
    }
    Ok(mapping)
}

fn apply_backup_memory_graph_fields(
    mut builder: crate::models::ExportMemoryRecordBuilder,
    fields: &BackupMemoryGraphFields,
) -> crate::models::ExportMemoryRecordBuilder {
    if let Some(value) = fields.pagerank_score {
        builder = builder.pagerank_score(value);
    }
    if let Some(value) = fields.betweenness_score {
        builder = builder.betweenness_score(value);
    }
    if let Some(value) = fields.hits_authority {
        builder = builder.hits_authority(value);
    }
    if let Some(value) = fields.hits_hub {
        builder = builder.hits_hub(value);
    }
    if let Some(value) = fields.onion_layer {
        builder = builder.onion_layer(value);
    }
    if let Some(value) = fields.k_truss_max {
        builder = builder.k_truss_max(value);
    }
    if let Some(value) = fields.articulation_point {
        builder = builder.articulation_point(value);
    }
    if let Some(value) = fields.bayes_alpha {
        builder = builder.bayes_alpha(value);
    }
    if let Some(value) = fields.bayes_beta {
        builder = builder.bayes_beta(value);
    }
    builder
}

fn export_memory_graph_fields_by_id(
    connection: &DbConnection,
    workspace_id: &str,
    memories: &[StoredMemory],
    links: &[StoredMemoryLink],
    audits: &[StoredAuditEntry],
) -> Result<BTreeMap<String, BackupMemoryGraphFields>, DomainError> {
    let mut fields_by_memory: BTreeMap<String, BackupMemoryGraphFields> = BTreeMap::new();
    // No pre-insertion: graph fields are only emitted when backed by real evidence.
    apply_imported_memory_graph_fields(&mut fields_by_memory, memories, audits);

    for memory in memories {
        if let Some((alpha, beta)) =
            connection
                .get_memory_bayes_posterior(&memory.id)
                .map_err(|error| DomainError::Storage {
                    message: error.to_string(),
                    repair: Some("ee db check --workspace .".to_owned()),
                })?
        {
            let fields = fields_by_memory.entry(memory.id.clone()).or_default();
            fields.bayes_alpha = finite_f64(alpha);
            fields.bayes_beta = finite_f64(beta);
        }
    }

    if let Some(snapshot) = connection
        .get_latest_graph_snapshot(workspace_id, GraphSnapshotType::MemoryLinks)
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("ee graph centrality-refresh --workspace .".to_owned()),
        })?
    {
        if let Ok(centrality) = crate::graph::graph_snapshot_centrality_report(&snapshot) {
            for score in centrality.scores {
                let fields = fields_by_memory.entry(score.memory_id).or_default();
                fields.pagerank_score = finite_f64(score.pagerank);
                fields.betweenness_score = finite_f64(score.betweenness);
                fields.hits_authority = finite_f64(score.authority);
                fields.hits_hub = finite_f64(score.hub);
            }
        }
    }

    if !links.is_empty() {
        add_structural_graph_fields(&mut fields_by_memory, memories, links);
    }
    Ok(fields_by_memory)
}

fn apply_imported_memory_graph_fields(
    fields_by_memory: &mut BTreeMap<String, BackupMemoryGraphFields>,
    memories: &[StoredMemory],
    audits: &[StoredAuditEntry],
) {
    let known_memory_ids = memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<BTreeSet<_>>();
    let mut applied_memory_ids = BTreeSet::new();

    for audit in audits {
        if audit.action != IMPORT_ACTION || audit.target_type.as_deref() != Some("memory") {
            continue;
        }
        let Some(memory_id) = audit.target_id.as_deref() else {
            continue;
        };
        if !known_memory_ids.contains(memory_id) {
            continue;
        }
        let Some(imported_fields) =
            imported_memory_graph_fields_from_audit(audit.details.as_deref())
        else {
            continue;
        };
        if !applied_memory_ids.insert(memory_id.to_owned()) {
            continue;
        }
        fields_by_memory
            .entry(memory_id.to_owned())
            .or_default()
            .overlay_present(imported_fields);
    }
}

fn imported_memory_graph_fields_from_audit(
    details: Option<&str>,
) -> Option<BackupMemoryGraphFields> {
    let value = serde_json::from_str::<JsonValue>(details?).ok()?;
    let fields = value.get("sourceGraphFields")?.as_object()?;
    let imported = BackupMemoryGraphFields {
        pagerank_score: fields
            .get("pagerank_score")
            .and_then(JsonValue::as_f64)
            .and_then(finite_f64),
        betweenness_score: fields
            .get("betweenness_score")
            .and_then(JsonValue::as_f64)
            .and_then(finite_f64),
        hits_authority: fields
            .get("hits_authority")
            .and_then(JsonValue::as_f64)
            .and_then(finite_f64),
        hits_hub: fields
            .get("hits_hub")
            .and_then(JsonValue::as_f64)
            .and_then(finite_f64),
        onion_layer: fields.get("onion_layer").and_then(json_u32),
        k_truss_max: fields.get("k_truss_max").and_then(json_u32),
        articulation_point: fields
            .get("articulation_point")
            .and_then(JsonValue::as_bool),
        bayes_alpha: fields
            .get("bayes_alpha")
            .and_then(JsonValue::as_f64)
            .and_then(finite_f64),
        bayes_beta: fields
            .get("bayes_beta")
            .and_then(JsonValue::as_f64)
            .and_then(finite_f64),
    };
    imported.has_any_field().then_some(imported)
}

fn json_u32(value: &JsonValue) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}

fn add_structural_graph_fields(
    fields_by_memory: &mut BTreeMap<String, BackupMemoryGraphFields>,
    memories: &[StoredMemory],
    links: &[StoredMemoryLink],
) {
    let mut graph = crate::graph::Graph::new(CompatibilityMode::Strict);
    for memory in memories {
        graph.add_node(&memory.id);
    }
    for link in links {
        let _ = graph.extend_edges_unrecorded(std::iter::once((
            link.src_memory_id.as_str(),
            link.dst_memory_id.as_str(),
        )));
    }

    let onion = crate::graph::decay::compute_onion_layers(&graph);
    for (memory_id, layer) in onion.layers_by_memory {
        if let Some(layer) = usize_to_u32(layer) {
            fields_by_memory.entry(memory_id).or_default().onion_layer = Some(layer);
        }
    }

    let articulation_points = crate::graph::decay::compute_articulation_points(&graph)
        .memory_ids
        .into_iter()
        .collect::<BTreeSet<_>>();
    for memory in memories {
        fields_by_memory
            .entry(memory.id.clone())
            .or_default()
            .articulation_point = Some(articulation_points.contains(&memory.id));
    }

    for member in crate::graph::health::compute_k_truss(&graph).top_memories_at_k {
        if let Some(max_k) = usize_to_u32(member.max_k) {
            fields_by_memory
                .entry(member.memory_id)
                .or_default()
                .k_truss_max = Some(max_k);
        }
    }
}

fn finite_f64(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn usize_to_u32(value: usize) -> Option<u32> {
    u32::try_from(value).ok()
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
    mesh: &BackupMeshSummary,
) -> JsonValue {
    let mut manifest = json!({
        "schema": if report.include_derived || report.include_graph_cache || !report.derived.is_empty() {
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
        "includeGraphCache": report.include_graph_cache,
        "graphCache": graph_cache_summary_json(report),
        "mesh": mesh.data_json(),
        "counts": {
            "totalRecords": report.total_records,
            "memoryRecords": report.memory_count,
            "linkRecords": report.link_count,
            "tagRecords": report.tag_count,
            "auditRecords": report.audit_count,
        },
        "recoveryInventory": report.recovery_inventory.data_json(),
        "artifacts": report.artifacts.iter().map(BackupArtifactReport::data_json).collect::<Vec<_>>(),
        "degraded": backup_degraded_data_json("backup_manifest", &report.degraded),
        "verification": {
            "status": report.verification_status,
            "manifestHash": manifest_hash,
        },
    });
    if report.include_derived || report.include_graph_cache || !report.derived.is_empty() {
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

fn graph_cache_summary_json(report: &BackupCreateReport) -> JsonValue {
    let snapshot_assets = report
        .derived
        .iter()
        .filter(|asset| asset.kind == "graph_snapshot")
        .count();
    let witness_assets = report
        .derived
        .iter()
        .filter(|asset| asset.kind == "graph_algorithm_witness")
        .count();
    let result_assets = report
        .derived
        .iter()
        .filter(|asset| asset.kind == "graph_algorithm_result")
        .count();
    json!({
        "included": report.include_graph_cache,
        "schemaVersion": report.graph_cache_schema_version,
        "tables": [
            "graph_snapshots",
            "graph_algorithm_witnesses",
            "graph_algorithm_results",
        ],
        "assetCounts": {
            "graphSnapshots": snapshot_assets,
            "graphAlgorithmWitnesses": witness_assets,
            "graphAlgorithmResults": result_assets,
        },
    })
}

fn backup_mesh_summary(
    connection: &DbConnection,
    workspace_id: &str,
    degraded: &mut Vec<BackupDegradation>,
) -> BackupMeshSummary {
    match connection.mesh_storage_status(workspace_id) {
        Ok(status) => BackupMeshSummary::from_storage_status(&status),
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "mesh_backup_status_unavailable",
                format!("mesh backup status could not be summarized: {error}"),
                "run ee doctor --workspace . --json before relying on mesh restore diagnostics",
            ));
            BackupMeshSummary::default()
        }
    }
}

fn mesh_storage_status_has_rows(status: &MeshStorageStatus) -> bool {
    status.peer_count > 0
        || status.cursor_count > 0
        || status.imported_event_count > 0
        || status.policy_decision_event_count > 0
        || status.policy_failure_event_count > 0
        || status.mapped_memory_count > 0
        || status.cached_body_count > 0
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
    collect_shard_fanout_payloads(
        workspace_path,
        workspace_id,
        captured_at,
        degraded,
        &mut payloads,
    );
    collect_graph_snapshot_payloads(
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

fn collect_graph_cache_payloads(
    connection: &DbConnection,
    workspace_id: &str,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
) -> Vec<BackupDerivedPayload> {
    let mut payloads = Vec::new();
    collect_graph_snapshot_payloads(
        connection,
        workspace_id,
        captured_at,
        degraded,
        &mut payloads,
    );
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
        let Some(bytes) = read_index_manifest_candidate(&candidate, degraded) else {
            continue;
        };
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
    if !included {
        degraded.push(BackupDegradation::warning(
            "index_manifest_missing",
            "no workspace index manifest was found; backup includes the durable JSONL source of truth only",
            "run ee index rebuild --workspace . before creating a backup that must include derived index metadata",
        ));
    }
}

fn read_index_manifest_candidate(
    candidate: &Path,
    degraded: &mut Vec<BackupDegradation>,
) -> Option<Vec<u8>> {
    match first_existing_symlink_component(candidate) {
        Ok(Some(symlink_path)) => {
            degraded.push(BackupDegradation::warning(
                "index_manifest_symlink",
                format!(
                    "index manifest '{}' was skipped because it traverses symlinked path component '{}'",
                    candidate.display(),
                    symlink_path.display()
                ),
                "replace .ee/index manifests with regular files before retrying backup create --include-derived",
            ));
            return None;
        }
        Ok(None) => {}
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "index_manifest_unreadable",
                error.message(),
                "inspect .ee/index permissions and retry backup create --include-derived",
            ));
            return None;
        }
    }

    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return None;
        }
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "index_manifest_unreadable",
                format!(
                    "index manifest '{}' could not be inspected: {error}",
                    candidate.display()
                ),
                "inspect .ee/index permissions and retry backup create --include-derived",
            ));
            return None;
        }
    };
    if !metadata.file_type().is_file() {
        return None;
    }

    const MAX_INDEX_MANIFEST_BYTES: u64 = 1024 * 1024;
    if metadata.len() > MAX_INDEX_MANIFEST_BYTES {
        degraded.push(BackupDegradation::warning(
            "index_manifest_too_large",
            format!(
                "index manifest '{}' is {} bytes, exceeding the {} byte limit",
                candidate.display(),
                metadata.len(),
                MAX_INDEX_MANIFEST_BYTES
            ),
            "inspect .ee/index for unexpected large files and retry backup create --include-derived",
        ));
        return None;
    }

    match fs::read(candidate) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "index_manifest_unreadable",
                format!(
                    "index manifest '{}' could not be read: {error}",
                    candidate.display()
                ),
                "inspect .ee/index permissions and retry backup create --include-derived",
            ));
            None
        }
    }
}

fn collect_shard_fanout_payloads(
    workspace_path: &Path,
    workspace_id: &str,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let enabled =
        shard_fanout_enabled_from_env_value(read_env_var(EnvVar::ShardFanoutEnabled).as_deref());
    if !enabled {
        return;
    }
    let status = resolve_shard_fanout_status(ShardFanoutResolverInput {
        enabled,
        workspace_id: Some(workspace_id.to_owned()),
        workspace_root: Some(workspace_path.to_path_buf()),
        shards_dir_override: read_env_var_os(EnvVar::ShardsDir).map(PathBuf::from),
    });
    collect_shard_fanout_payloads_from_status(&status, captured_at, degraded, payloads);
}

fn collect_shard_fanout_payloads_from_status(
    status: &ShardFanoutStatusReport,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    for entry in &status.degraded {
        degraded.push(BackupDegradation::with_severity(
            entry.code,
            entry.severity,
            entry.message,
            entry.repair,
        ));
    }
    if status.posture != ShardFanoutPosture::Enabled {
        return;
    }

    let Some(shard_id) = status.shard_id.as_deref() else {
        degraded.push(BackupDegradation::warning(
            "shard_fanout_workspace_unavailable",
            "shard fan-out is enabled but no workspace shard id was available for backup",
            "run ee status --workspace . --json and retry backup create --include-derived",
        ));
        return;
    };
    let Some(shard_path) = status.shard_path.as_deref() else {
        degraded.push(BackupDegradation::warning(
            "shard_fanout_shard_missing",
            "shard fan-out is enabled but no workspace shard path was available for backup",
            "run ee migrate shard-fanout --workspace . --dry-run --json",
        ));
        return;
    };

    let Some(catalog_bytes) = read_shard_fanout_asset(&status.catalog_path, "catalog", degraded)
    else {
        return;
    };
    let Some(shard_bytes) = read_shard_fanout_asset(shard_path, "workspace shard", degraded) else {
        return;
    };

    let catalog_backup_path = "derived/shards/catalog.db";
    let shard_backup_path = format!("derived/shards/{}.db", safe_file_stem(shard_id));
    let catalog_hash = hash_bytes(&catalog_bytes);
    let shard_hash = hash_bytes(&shard_bytes);
    let manifest = json!({
        "schema": "ee.backup.derived.shard_fanout.v1",
        "capturedAt": captured_at,
        "workspaceId": status.workspace_id.as_deref(),
        "shardId": shard_id,
        "catalog": {
            "backupPath": catalog_backup_path,
            "sourcePath": status.catalog_path.to_string_lossy(),
            "schemaVersion": status.catalog_contract.schema_version,
            "hash": catalog_hash,
            "byteSize": catalog_bytes.len() as u64,
        },
        "shards": [{
            "workspaceId": status.workspace_id.as_deref(),
            "shardId": shard_id,
            "backupPath": shard_backup_path,
            "sourcePath": shard_path.to_string_lossy(),
            "hash": shard_hash,
            "byteSize": shard_bytes.len() as u64,
            "schemaVersion": status.catalog_contract.schema_version,
            "shardGeneration": status.shard_generation,
        }],
        "redaction": {
            "status": "not_applicable",
            "reason": "catalog and shard database files are storage artifacts; user memory content remains governed by records.jsonl redaction",
        },
        "restore": {
            "sidePathCatalog": ".ee/catalog.db",
            "sidePathShardRoot": ".ee/shards",
            "overwritePolicy": "write_new_file",
        },
    });
    match json_payload_bytes(&manifest) {
        Ok(manifest_bytes) => {
            payloads.push(derived_payload(
                catalog_backup_path,
                "shard_fanout_catalog",
                captured_at,
                None,
                catalog_bytes,
            ));
            payloads.push(derived_payload(
                shard_backup_path,
                "shard_fanout_workspace_shard",
                captured_at,
                None,
                shard_bytes,
            ));
            payloads.push(derived_payload(
                "derived/shards/manifest.json",
                "shard_fanout_manifest",
                captured_at,
                None,
                manifest_bytes,
            ));
        }
        Err(error) => degraded.push(BackupDegradation::warning(
            "shard_fanout_manifest_unreadable",
            format!("shard fan-out backup manifest could not be serialized: {error}"),
            "run ee db check --workspace . before retrying backup create --include-derived",
        )),
    }
}

fn read_shard_fanout_asset(
    path: &Path,
    label: &'static str,
    degraded: &mut Vec<BackupDegradation>,
) -> Option<Vec<u8>> {
    match first_existing_symlink_component(path) {
        Ok(Some(symlink_path)) => {
            degraded.push(BackupDegradation::warning(
                "shard_fanout_asset_symlink",
                format!(
                    "shard fan-out {label} '{}' was skipped because it traverses symlinked path component '{}'",
                    path.display(),
                    symlink_path.display()
                ),
                "replace symlinked shard fan-out files with regular files before retrying backup create --include-derived",
            ));
            return None;
        }
        Ok(None) => {}
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "shard_fanout_asset_unreadable",
                error.message(),
                "inspect shard fan-out filesystem permissions and retry backup create --include-derived",
            ));
            return None;
        }
    }

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            degraded.push(BackupDegradation::warning(
                "shard_fanout_asset_unreadable",
                format!(
                    "shard fan-out {label} '{}' is not a regular file",
                    path.display()
                ),
                "run ee migrate shard-fanout --workspace . --dry-run --json",
            ));
            return None;
        }
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "shard_fanout_asset_unreadable",
                format!(
                    "shard fan-out {label} '{}' could not be inspected: {error}",
                    path.display()
                ),
                "inspect shard fan-out filesystem permissions and retry backup create --include-derived",
            ));
            return None;
        }
    }

    match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "shard_fanout_asset_unreadable",
                format!(
                    "shard fan-out {label} '{}' could not be read: {error}",
                    path.display()
                ),
                "inspect shard fan-out filesystem permissions and retry backup create --include-derived",
            ));
            None
        }
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
        collect_graph_algorithm_payloads(connection, &snapshot, captured_at, degraded, payloads);
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

fn collect_graph_algorithm_payloads(
    connection: &DbConnection,
    snapshot: &StoredGraphSnapshot,
    captured_at: &str,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let witnesses =
        match connection.list_graph_algorithm_witnesses(&snapshot.workspace_id, &snapshot.id, None)
        {
            Ok(witnesses) => witnesses,
            Err(error) => {
                degraded.push(BackupDegradation::warning(
                    "graph_algorithm_witnesses_unreadable",
                    format!(
                        "graph algorithm witnesses could not be read from the database: {error}"
                    ),
                    "run ee db check --workspace . before retrying backup create --include-derived",
                ));
                Vec::new()
            }
        };
    for (index, witness) in witnesses.iter().enumerate() {
        match json_payload_bytes(&graph_algorithm_witness_json(witness, captured_at)) {
            Ok(bytes) => payloads.push(derived_payload(
                format!(
                    "derived/graph/witnesses/{}-{:04}.json",
                    safe_file_stem(&snapshot.id),
                    index
                ),
                "graph_algorithm_witness",
                captured_at,
                None,
                bytes,
            )),
            Err(error) => degraded.push(BackupDegradation::warning(
                "graph_algorithm_witnesses_unreadable",
                format!("graph algorithm witness payload could not be serialized: {error}"),
                "run ee db check --workspace . before retrying backup create --include-derived",
            )),
        }
    }

    let results =
        match connection.list_graph_algorithm_results(&snapshot.workspace_id, &snapshot.id, None) {
            Ok(results) => results,
            Err(error) => {
                degraded.push(BackupDegradation::warning(
                    "graph_algorithm_results_unreadable",
                    format!(
                        "graph algorithm result cache could not be read from the database: {error}"
                    ),
                    "run ee db check --workspace . before retrying backup create --include-derived",
                ));
                Vec::new()
            }
        };
    for (index, result) in results.iter().enumerate() {
        match json_payload_bytes(&graph_algorithm_result_json(result, captured_at)) {
            Ok(bytes) => payloads.push(derived_payload(
                format!(
                    "derived/graph/results/{}-{:04}.json",
                    safe_file_stem(&snapshot.id),
                    index
                ),
                "graph_algorithm_result",
                captured_at,
                None,
                bytes,
            )),
            Err(error) => degraded.push(BackupDegradation::warning(
                "graph_algorithm_results_unreadable",
                format!("graph algorithm result payload could not be serialized: {error}"),
                "run ee db check --workspace . before retrying backup create --include-derived",
            )),
        }
    }
}

fn graph_algorithm_witness_json(
    witness: &StoredGraphAlgorithmWitness,
    captured_at: &str,
) -> JsonValue {
    json!({
        "schema": "ee.backup.derived.graph_algorithm_witness.v1",
        "capturedAt": captured_at,
        "witness": {
            "workspaceId": &witness.workspace_id,
            "snapshotId": &witness.snapshot_id,
            "algorithm": &witness.algorithm,
            "params": parse_json_or_string(&witness.params_json),
            "witness": parse_json_or_string(&witness.witness_json),
            "recordedAt": &witness.recorded_at,
        }
    })
}

fn graph_algorithm_result_json(
    result: &StoredGraphAlgorithmResult,
    captured_at: &str,
) -> JsonValue {
    json!({
        "schema": "ee.backup.derived.graph_algorithm_result.v1",
        "capturedAt": captured_at,
        "result": {
            "workspaceId": &result.workspace_id,
            "snapshotId": &result.snapshot_id,
            "algorithm": &result.algorithm,
            "paramsHash": &result.params_hash,
            "result": parse_json_or_string(&result.result_json),
            "computedAt": &result.computed_at,
            "ttlSeconds": result.ttl_seconds,
        }
    })
}

fn parse_json_or_string(value: &str) -> JsonValue {
    serde_json::from_str(value).unwrap_or_else(|_| JsonValue::String(value.to_owned()))
}

fn collect_task_episode_payloads(
    connection: &DbConnection,
    workspace_id: &str,
    captured_at: &str,
    redaction_level: RedactionLevel,
    memory_ids: &BTreeMap<String, String>,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let episodes = match connection.list_task_episodes(Some(workspace_id), None, u32::MAX) {
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
        match json_payload_bytes(&task_episode_json(
            &episode,
            captured_at,
            redaction_level,
            memory_ids,
        )) {
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

fn task_episode_json(
    episode: &StoredTaskEpisode,
    captured_at: &str,
    redaction_level: RedactionLevel,
    memory_ids: &BTreeMap<String, String>,
) -> JsonValue {
    let original = episode;
    let mut episode = episode.clone();
    for id in &mut episode.retrieved_memory_ids {
        if let Some(restored_id) = memory_ids.get(id) {
            id.clone_from(restored_id);
        }
    }
    episode.task_input = redact_content(&episode.task_input, redaction_level);
    episode.outcome_details = episode
        .outcome_details
        .as_deref()
        .map(|text| redact_content(text, redaction_level));
    episode.agent = episode
        .agent
        .as_deref()
        .map(|text| redact_content(text, redaction_level));
    for action in &mut episode.actions {
        action.action_type = redact_content(&action.action_type, redaction_level);
        action.target_id = action.target_id.as_deref().map(|text| {
            memory_ids
                .get(text)
                .cloned()
                .unwrap_or_else(|| redact_content(text, redaction_level))
        });
        action.details = action
            .details
            .as_deref()
            .map(|text| redact_content(text, redaction_level));
    }
    if &episode != original {
        // A source hash must not authenticate a redacted episode body.
        episode.episode_hash = None;
    }
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

fn collect_cass_payloads(
    connection: &DbConnection,
    workspace_id: &str,
    captured_at: &str,
    redaction_level: RedactionLevel,
    memory_ids: &BTreeMap<String, String>,
    degraded: &mut Vec<BackupDegradation>,
    payloads: &mut Vec<BackupDerivedPayload>,
) {
    let sessions = match connection.list_sessions(workspace_id) {
        Ok(sessions) => sessions,
        Err(error) => {
            degraded.push(required_backup_rows_unreadable("sessions", error));
            return;
        }
    };
    for (index, chunk) in sessions.chunks(CASS_BACKUP_CHUNK_ROWS).enumerate() {
        let chunk = BackupCassSessionChunk {
            schema: "ee.backup.derived.cass_sessions.v1".to_owned(),
            captured_at: captured_at.to_owned(),
            chunk_index: u32::try_from(index).unwrap_or(u32::MAX),
            source_locator_policy: "omitted_host_local".to_owned(),
            sessions: chunk
                .iter()
                .map(|session| {
                    let mut record = BackupCassSessionRecord::from_stored(session);
                    record.agent_name = record
                        .agent_name
                        .as_deref()
                        .map(|text| redact_content(text, redaction_level));
                    record.model = record
                        .model
                        .as_deref()
                        .map(|text| redact_content(text, redaction_level));
                    record
                })
                .collect(),
        };
        match serialized_payload_bytes(&chunk) {
            Ok(bytes) => payloads.push(derived_payload(
                format!("derived/cass/sessions-{index:04}.json"),
                "cass_sessions",
                captured_at,
                None,
                bytes,
            )),
            Err(error) => {
                degraded.push(required_backup_rows_unreadable("sessions", error));
                return;
            }
        }
    }

    let evidence = match connection.list_evidence_spans_for_workspace(workspace_id) {
        Ok(evidence) => evidence,
        Err(error) => {
            degraded.push(required_backup_rows_unreadable("evidence_spans", error));
            return;
        }
    };
    let sessions_by_id = sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect::<BTreeMap<_, _>>();
    for (index, chunk) in evidence.chunks(CASS_BACKUP_CHUNK_ROWS).enumerate() {
        let chunk = BackupCassEvidenceChunk {
            schema: "ee.backup.derived.cass_evidence_spans.v1".to_owned(),
            captured_at: captured_at.to_owned(),
            chunk_index: u32::try_from(index).unwrap_or(u32::MAX),
            evidence_spans: chunk
                .iter()
                .map(|span| {
                    let mut record = BackupCassEvidenceRecord::from_stored(span);
                    if let Some(id) = record.memory_id.as_mut()
                        && let Some(restored_id) = memory_ids.get(id)
                    {
                        id.clone_from(restored_id);
                    }
                    let provenance_admitted = sessions_by_id
                        .get(span.session_id.as_str())
                        .is_some_and(|session| {
                            span.is_derivation_admitted_for_session(workspace_id, session)
                        });
                    record.redact_for_export(redaction_level, provenance_admitted);
                    record
                })
                .collect(),
        };
        match serialized_payload_bytes(&chunk) {
            Ok(bytes) => payloads.push(derived_payload(
                format!("derived/cass/evidence-spans-{index:04}.json"),
                "cass_evidence_spans",
                captured_at,
                None,
                bytes,
            )),
            Err(error) => {
                degraded.push(required_backup_rows_unreadable("evidence_spans", error));
                return;
            }
        }
    }
}

fn required_backup_rows_unreadable(
    table: &str,
    error: impl std::fmt::Display,
) -> BackupDegradation {
    BackupDegradation::with_severity(
        "backup_source_rows_not_covered",
        "high",
        format!("required {table} rows could not be captured for recovery: {error}"),
        "run ee db check --workspace . and recreate the backup before treating it as a recovery point",
    )
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
    match first_existing_symlink_component(episode_dir) {
        Ok(Some(symlink_path)) => {
            degraded.push(BackupDegradation::warning(
                "lab_episodes_unreadable",
                format!(
                    "lab episode directory '{}' was skipped because it traverses symbolic link '{}'",
                    episode_dir.display(),
                    symlink_path.display()
                ),
                "replace symlinked lab episode paths with real directories before retrying backup create --include-derived",
            ));
            return;
        }
        Ok(None) => {}
        Err(error) => {
            degraded.push(BackupDegradation::warning(
                "lab_episodes_unreadable",
                format!(
                    "lab episode directory '{}' could not be inspected: {error}",
                    episode_dir.display()
                ),
                "inspect ~/.local/share/ee/lab/episodes permissions and retry backup create --include-derived",
            ));
            return;
        }
    }
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
        let metadata = match fs::symlink_metadata(&path) {
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
        if is_appledouble_file_name(file_name) {
            continue;
        }
        let safe_name = safe_file_stem(file_name);
        match read_lab_episode_source_file(&path) {
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
        };
    }
}

/// Hard upper bound on the byte length of a lab-episode source file read
/// by `read_lab_episode_source_file` during `backup create
/// --include-derived`. Matches the parallel cap that `src/core/lab.rs`
/// (5491131c) uses for `read_lab_file_to_string_no_follow`, so the
/// backup-side and lab-side readers share a single ceiling and a file
/// rejected by one is also rejected by the other.
///
/// 16 MiB is generous: realistic lab episode files are tens of KB to a
/// few MB, and the cap leaves headroom for captures with thousands of
/// evidence ids while bounding worst-case allocation. A peer agent that
/// pre-stages a multi-GiB file under `~/.local/share/ee/lab/episodes/`
/// (the shared lab episode store) would otherwise OOM every `backup
/// create --include-derived` invocation that scans the directory.
const LAB_EPISODE_SOURCE_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;

fn read_lab_episode_source_file(path: &Path) -> io::Result<Vec<u8>> {
    // Bounded read: cap at `LAB_EPISODE_SOURCE_FILE_MAX_BYTES + 1` so
    // the post-read size check distinguishes "exactly at cap" (accepted)
    // from "above cap" (rejected) without a separate stat call on the
    // read path. The caller (line 3853) only checks
    // `metadata.file_type().is_file()` before reaching this read — NO
    // size guard — so an unbounded `read_to_end` on a multi-GiB
    // peer-planted lab episode would force a matching `Vec<u8>` pre-size
    // and OOM the backup hot path. The error path is already mapped to
    // a per-file `lab_episodes_unreadable` degraded entry at the caller,
    // so an over-cap file gracefully degrades to a warning instead of
    // crashing the whole backup. Same defensive pattern as the parallel
    // cap at `src/core/lab.rs::read_lab_file_to_string_no_follow`
    // (5491131c), `src/cache/pack_l2.rs::read_cache_entry_file`
    // (8ba93c0e), and the round-2 cap pass on workspace metadata.
    let file = open_backup_artifact_for_read(path)?;
    let mut bytes = Vec::new();
    file.take(LAB_EPISODE_SOURCE_FILE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LAB_EPISODE_SOURCE_FILE_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing to read lab episode source `{}`: exceeded the {LAB_EPISODE_SOURCE_FILE_MAX_BYTES}-byte cap",
                path.display(),
            ),
        ));
    }
    Ok(bytes)
}

fn is_appledouble_file_name(file_name: &str) -> bool {
    file_name.starts_with("._")
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

fn serialized_payload_bytes(value: &impl Serialize) -> Result<Vec<u8>, serde_json::Error> {
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

fn backup_degradations(
    workspace_path: &Path,
    include_derived: bool,
    include_graph_cache: bool,
) -> Vec<BackupDegradation> {
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
    if !include_graph_cache {
        degraded.push(BackupDegradation::warning(
            "graph_snapshot_not_included",
            "graph snapshots and graph algorithm cache rows are not included in this backup",
            "rerun backup create with --include-graph-cache",
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
    ensure_backup_create_path_has_no_symlink_components(backup_root, "backup root")?;
    ensure_backup_create_path_has_no_symlink_components(backup_path, "backup directory")?;
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

fn ensure_backup_create_path_has_no_symlink_components(
    path: &Path,
    role: &'static str,
) -> Result<(), DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "{role} '{}' traverses symbolic link '{}'; backup creation requires a real output path",
                path.display(),
                symlink_path.display()
            ),
            repair: Some("choose a real, non-symlink directory for --output-dir".to_owned()),
        });
    }
    Ok(())
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

fn ensure_side_path_outside_workspace(
    workspace_path: &Path,
    side_path: &Path,
) -> Result<(), DomainError> {
    let absolute_side_path = lexical_absolute_path(side_path);
    let workspace_path = lexical_absolute_path(workspace_path);
    if absolute_side_path.starts_with(&workspace_path) {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "side path '{}' must be outside source workspace '{}'",
                side_path.display(),
                workspace_path.display()
            ),
            repair: Some("choose a separate --side-path target outside the workspace".to_owned()),
        });
    }
    Ok(())
}

fn lexical_absolute_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
        }
    }
    normalized
}

fn first_existing_symlink_component(path: &Path) -> Result<Option<PathBuf>, DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        #[cfg(windows)]
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        #[cfg(not(windows))]
        if matches!(component, Component::RootDir) {
            continue;
        }
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
    ensure_backup_write_path_has_no_symlink_components(path, "backup artifact")?;
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
    file.sync_all().map_err(|error| DomainError::Storage {
        message: format!("failed to sync '{}': {error}", path.display()),
        repair: Some("inspect disk health and retry backup creation".to_owned()),
    })
}

fn copy_new_file(source: &Path, destination: &Path) -> Result<(), DomainError> {
    ensure_backup_write_path_has_no_symlink_components(source, "backup source artifact")?;
    ensure_backup_write_path_has_no_symlink_components(destination, "backup restore artifact")?;
    let mut source_file =
        open_backup_artifact_for_read(source).map_err(|error| DomainError::Storage {
            message: format!("failed to open '{}': {error}", source.display()),
            repair: Some("verify the backup artifact and retry restore".to_owned()),
        })?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| DomainError::Storage {
            message: format!("failed to create '{}': {error}", destination.display()),
            repair: Some("retry restore with a fresh side path".to_owned()),
        })?;
    io::copy(&mut source_file, &mut destination_file).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to copy '{}' to '{}': {error}",
            source.display(),
            destination.display()
        ),
        repair: Some("inspect disk health and retry restore".to_owned()),
    })?;
    destination_file
        .sync_all()
        .map_err(|error| DomainError::Storage {
            message: format!("failed to sync '{}': {error}", destination.display()),
            repair: Some("inspect disk health and retry restore".to_owned()),
        })
}

fn write_new_relative_file(
    root: &Path,
    relative_path: &str,
    bytes: &[u8],
) -> Result<PathBuf, DomainError> {
    let path = root.join(relative_path);
    ensure_backup_write_path_has_no_symlink_components(&path, "backup relative artifact")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create backup artifact directory '{}': {error}",
                parent.display()
            ),
            repair: Some("retry backup creation with a writable output directory".to_owned()),
        })?;
    }
    ensure_backup_write_path_has_no_symlink_components(&path, "backup relative artifact")?;
    write_new_file(&path, bytes)?;
    Ok(path)
}

fn ensure_backup_write_path_has_no_symlink_components(
    path: &Path,
    role: &'static str,
) -> Result<(), DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "{role} '{}' traverses symbolic link '{}'; backup writes require real artifact paths",
                path.display(),
                symlink_path.display()
            ),
            repair: Some(
                "replace symlinked backup artifact paths with real directories".to_owned(),
            ),
        });
    }
    Ok(())
}

fn open_backup_artifact_for_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_backup_artifact_read_options(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_backup_artifact_read_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_backup_artifact_read_options(_options: &mut OpenOptions) {}

fn hash_file(path: &Path) -> Result<String, DomainError> {
    let mut file = open_backup_artifact_for_read(path).map_err(|error| DomainError::Storage {
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

fn normalize_backup_input_path(path: &Path) -> Result<PathBuf, DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "backup path '{}' traverses symbolic link '{}'; backup inspect, verify, and restore require a self-contained backup directory",
                path.display(),
                symlink_path.display()
            ),
            repair: Some("choose a self-contained backup directory".to_owned()),
        });
    }
    Ok(normalize_path(path))
}

fn normalize_restore_side_path(path: &Path) -> Result<PathBuf, DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        let message = if symlink_path == path {
            format!(
                "side path '{}' is a symbolic link; restore requires an isolated real directory",
                path.display()
            )
        } else {
            format!(
                "side path '{}' traverses symbolic link '{}'; restore requires an isolated real directory",
                path.display(),
                symlink_path.display()
            )
        };
        return Err(DomainError::PolicyDenied {
            message,
            repair: Some("choose a real, non-symlink directory for --side-path".to_owned()),
        });
    }
    Ok(normalize_path(path))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::core::jsonl_import::import_jsonl_records;
    use crate::db::{
        CreateAuditInput, CreateEvidenceSpanInput, CreateGraphAlgorithmResultInput,
        CreateGraphAlgorithmWitnessInput, CreateGraphSnapshotInput, CreateMemoryInput,
        CreateMemoryLinkInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
        GraphSnapshotType, MemoryLinkRelation, MemoryLinkSource,
    };
    use crate::models::{EvidenceId, MemoryId, MemoryLinkId, SessionId, WorkspaceId};
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

    fn directory_entry_names(path: &Path) -> Result<Vec<String>, String> {
        let mut names = fs::read_dir(path)
            .map_err(|error| error.to_string())?
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        Ok(names)
    }

    fn optional_file_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn database_sidecar_path(database: &Path, suffix: &str) -> PathBuf {
        let mut path = database.as_os_str().to_os_string();
        path.push(suffix);
        PathBuf::from(path)
    }

    fn stored_memory_fixture(id: &str) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
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
        }
    }

    fn backup_cass_session_fixture(id: &str, workspace_id: &str) -> BackupCassSessionRecord {
        BackupCassSessionRecord {
            id: id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            source_locator_hash: hash_bytes(b"cass://portable-session"),
            source_metadata_hash: None,
            agent_name: Some("codex".to_owned()),
            model: Some("gpt-5".to_owned()),
            started_at: Some("2026-09-01T00:00:00Z".to_owned()),
            ended_at: Some("2026-09-01T00:01:00Z".to_owned()),
            message_count: 2,
            token_count: Some(64),
            content_hash: hash_bytes(b"portable CASS session"),
            imported_at: "2026-09-01T00:02:00Z".to_owned(),
            updated_at: "2026-09-01T00:03:00Z".to_owned(),
        }
    }

    fn backup_cass_evidence_fixture(
        id: &str,
        workspace_id: &str,
        session_id: &str,
    ) -> BackupCassEvidenceRecord {
        let excerpt = "Portable CASS recovery evidence";
        BackupCassEvidenceRecord {
            id: id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            session_id: session_id.to_owned(),
            memory_id: None,
            cass_span_id: "cass://portable-session:1".to_owned(),
            span_kind: "message".to_owned(),
            start_line: 1,
            end_line: 2,
            start_byte: Some(0),
            end_byte: Some(32),
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: hash_bytes(excerpt.as_bytes()),
            metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
            producer_kind: "cass_import".to_owned(),
            screening_version: 1,
            secret_redaction_status: "clean".to_owned(),
            redaction_classes_json: "[]".to_owned(),
            instruction_risk: "none".to_owned(),
            search_eligibility: "eligible".to_owned(),
            pack_eligibility: "eligible".to_owned(),
            canonical_provenance_revision: 1,
            canonical_excerpt_hash: Some(hash_bytes(excerpt.as_bytes())),
            security_policy_epoch: 1,
            upstream_ref_hash: Some(hash_bytes(b"cass://portable-session:1")),
            created_at: "2026-09-01T00:02:00Z".to_owned(),
            updated_at: "2026-09-01T00:03:00Z".to_owned(),
        }
    }

    fn restored_cass_asset(path: &Path, kind: &str) -> BackupRestoredDerivedAssetReport {
        BackupRestoredDerivedAssetReport {
            path: path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            kind: kind.to_owned(),
            restore_path: path.to_string_lossy().into_owned(),
            lab_episode_path: None,
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

    fn backup_denied_mesh_link_metadata() -> String {
        json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_backup_denied",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_backup_decision_denied",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }

    fn seed_mesh_backup_fixture(
        connection: &DbConnection,
        workspace_id: &str,
        local_memory_id: &str,
    ) -> TestResult {
        let peer_id = "peer_backup_fixture";
        let origin_node_id = "node_backup_fixture";
        let origin_workspace_id = "wsp_remote_backup_fixture";
        let logical_memory_id = "mem_remote_backup_fixture";
        connection
            .upsert_mesh_peer(&crate::db::UpsertMeshPeerInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer_id.to_owned(),
                origin_node_id: origin_node_id.to_owned(),
                display_name: Some("remote builder".to_owned()),
                policy_summary_json: Some(json!({"token": "secret-peer-token"}).to_string()),
                enabled: true,
                last_seen_at: Some("2026-05-21T00:00:00Z".to_owned()),
            })
            .map_err(|error| error.to_string())?;
        connection
            .upsert_mesh_peer_cursor(&crate::db::UpsertMeshPeerCursorInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer_id.to_owned(),
                origin_node_id: origin_node_id.to_owned(),
                origin_workspace_id: origin_workspace_id.to_owned(),
                last_seq: 7,
                tip_event_hash: Some("blake3:mesh-tip".to_owned()),
                tip_audit_hash: Some("blake3:mesh-audit".to_owned()),
                status: "current".to_owned(),
                updated_at: Some("2026-05-21T00:01:00Z".to_owned()),
            })
            .map_err(|error| error.to_string())?;
        connection
            .insert_mesh_import_ledger_event(&crate::db::InsertMeshImportLedgerEventInput {
                workspace_id: workspace_id.to_owned(),
                event_id: "mesh_evt_backup_fixture".to_owned(),
                origin_node_id: origin_node_id.to_owned(),
                origin_workspace_id: origin_workspace_id.to_owned(),
                producer_peer_id: Some(peer_id.to_owned()),
                seq: 7,
                prev_event_hash: None,
                event_hash: "blake3:mesh-event".to_owned(),
                event_kind: "create".to_owned(),
                logical_memory_id: logical_memory_id.to_owned(),
                content_hash: "blake3:mesh-content".to_owned(),
                material_lane: "metadata".to_owned(),
                redaction_class: "metadataOnly".to_owned(),
                trust_lane: "peerAgent".to_owned(),
                import_decision: "allow".to_owned(),
                local_memory_id: Some(local_memory_id.to_owned()),
                body_cache_key: Some("body-cache-backup-fixture".to_owned()),
                policy_failure_surface_json: None,
                policy_decision_json: None,
                event_json: json!({"schema": "ee.mesh.event.fixture.v1"}).to_string(),
                imported_at: Some("2026-05-21T00:02:00Z".to_owned()),
            })
            .map_err(|error| error.to_string())?;
        connection
            .upsert_mesh_memory_mapping(&crate::db::UpsertMeshMemoryMappingInput {
                workspace_id: workspace_id.to_owned(),
                origin_node_id: origin_node_id.to_owned(),
                origin_workspace_id: origin_workspace_id.to_owned(),
                logical_memory_id: logical_memory_id.to_owned(),
                local_memory_id: Some(local_memory_id.to_owned()),
                latest_event_hash: "blake3:mesh-event".to_owned(),
                content_hash: "blake3:mesh-content".to_owned(),
                trust_lane: "peerAgent".to_owned(),
                redaction_class: "metadataOnly".to_owned(),
                updated_at: Some("2026-05-21T00:03:00Z".to_owned()),
            })
            .map_err(|error| error.to_string())?;
        connection
            .upsert_mesh_body_cache_metadata(&crate::db::UpsertMeshBodyCacheMetadataInput {
                workspace_id: workspace_id.to_owned(),
                body_cache_key: "body-cache-backup-fixture".to_owned(),
                origin_node_id: origin_node_id.to_owned(),
                origin_workspace_id: origin_workspace_id.to_owned(),
                logical_memory_id: logical_memory_id.to_owned(),
                content_hash: "blake3:mesh-content".to_owned(),
                body_ref_json: Some(json!({"credential": "secret-body-ref"}).to_string()),
                preview_hash: Some("blake3:mesh-preview".to_owned()),
                size_bytes: Some(128),
                cache_status: "available".to_owned(),
                local_body_hash: Some("blake3:mesh-body".to_owned()),
                cached_at: Some("2026-05-21T00:04:00Z".to_owned()),
                expires_at: None,
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn sample_import_jsonl_with_graph_fields() -> String {
        [
            r#"{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-04-30T00:00:00Z","workspace_id":"wsp_01234567890123456789012345","workspace_path":"/source","export_scope":"memories","redaction_level":"none","record_count":3,"ee_version":"0.1.0","hostname":null,"export_id":"exp-001","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}"#,
            r#"{"schema":"ee.export.memory.v1","memory_id":"mem_01234567890123456789012345","workspace_id":"wsp_01234567890123456789012345","level":"procedural","kind":"rule","content":"Run cargo fmt --check before release.","importance":0.8,"confidence":0.9,"utility":0.7,"pagerank_score":0.12,"betweenness_score":0.34,"hits_authority":0.56,"hits_hub":0.78,"onion_layer":3,"k_truss_max":4,"articulation_point":true,"bayes_alpha":2.5,"bayes_beta":1.5,"created_at":"2026-04-30T00:00:00Z","updated_at":null,"expires_at":null,"source_agent":"MistySalmon","provenance_uri":"ee-export://fixture","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}"#,
            r#"{"schema":"ee.export.tag.v1","memory_id":"mem_01234567890123456789012345","tag":"Release","created_at":"2026-04-30T00:00:00Z"}"#,
            r#"{"schema":"ee.export.footer.v1","export_id":"exp-001","completed_at":"2026-04-30T00:01:00Z","total_records":4,"memory_count":1,"link_count":0,"tag_count":1,"audit_count":0,"checksum":null,"success":true,"error_message":null}"#,
        ]
        .join("\n")
    }

    #[test]
    fn recovery_inventory_classifies_every_fresh_migrated_table() -> TestResult {
        let (_tempdir, _workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;

        let inventory = build_recovery_inventory(&connection).map_err(|error| error.message())?;
        connection.close().map_err(|error| error.to_string())?;

        let unclassified = inventory
            .entries
            .iter()
            .filter(|entry| entry.disposition == "unclassified")
            .map(|entry| entry.table.as_str())
            .collect::<Vec<_>>();
        ensure(
            unclassified.is_empty(),
            format!("fresh migrated tables missing backup disposition: {unclassified:?}"),
        )?;
        ensure_equal(
            inventory.unclassified_table_count,
            0,
            "fresh schema unclassified table count",
        )
    }

    #[test]
    fn recovery_inventory_marks_nonempty_uncovered_source_rows_partial() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("canonicalize backup fixture workspace: {error}"))?;
        let database = database
            .canonicalize()
            .map_err(|error| format!("canonicalize backup fixture database: {error}"))?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_id = connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .first()
            .map(|stored| stored.id.clone())
            .ok_or_else(|| "backup fixture omitted workspace row".to_owned())?;
        connection
            .insert_session(
                "sess_01234567890123456789012345",
                &CreateSessionInput {
                    workspace_id: workspace_id.clone(),
                    cass_session_id: "cass-backup-uncovered-01".to_owned(),
                    source_path: Some("/Users/alice/private/session.jsonl".to_owned()),
                    agent_name: Some("codex".to_owned()),
                    model: Some("fixture".to_owned()),
                    started_at: Some("2026-09-01T00:00:00Z".to_owned()),
                    ended_at: Some("2026-09-01T00:01:00Z".to_owned()),
                    message_count: 1,
                    token_count: Some(8),
                    content_hash: "blake3:fixture".to_owned(),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        // Sessions are now captured by default. Keep a real, still-unsupported
        // durable row as the negative control for incomplete recovery.
        connection
            .insert_import_ledger(
                "imp_01234567890123456789012345",
                &crate::db::CreateImportLedgerInput {
                    workspace_id,
                    source_kind: "cass".to_owned(),
                    source_id: "backup-uncovered-import".to_owned(),
                    status: "completed".to_owned(),
                    cursor_json: None,
                    imported_session_count: 1,
                    imported_span_count: 0,
                    attempt_count: 1,
                    error_code: None,
                    error_message: None,
                    started_at: Some("2026-09-01T00:00:00Z".to_owned()),
                    completed_at: Some("2026-09-01T00:01:00Z".to_owned()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(workspace.join("inventory-backups")),
            label: Some("inventory-gap".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(report.status.as_str(), "partial", "partial backup status")?;
        ensure_equal(
            report.verification_status.as_str(),
            "incomplete_source_coverage",
            "partial backup verification posture",
        )?;
        ensure(
            !report.recovery_inventory.snapshot_coverage_complete,
            "nonempty uncovered import row must make snapshot coverage incomplete",
        )?;
        let session = report
            .recovery_inventory
            .entries
            .iter()
            .find(|entry| entry.table == "sessions")
            .ok_or_else(|| "recovery inventory omitted sessions".to_owned())?;
        ensure_equal(session.row_count, 1, "captured session row count")?;
        ensure_equal(
            session.coverage.as_str(),
            "derived_artifact_restore",
            "session backup coverage",
        )?;
        ensure(
            session.snapshot_covered,
            "default backup covers its session",
        )?;
        let ledger = report
            .recovery_inventory
            .entries
            .iter()
            .find(|entry| entry.table == "import_ledger")
            .ok_or_else(|| "recovery inventory omitted import_ledger".to_owned())?;
        ensure_equal(ledger.row_count, 1, "uncovered import row count")?;
        ensure(!ledger.snapshot_covered, "import ledger remains uncovered")?;
        ensure(
            report.degraded.iter().any(|entry| {
                entry.code == "backup_source_rows_not_covered"
                    && entry.severity == "high"
                    && entry.message.contains("import_ledger=1")
            }),
            format!(
                "partial backup omitted high source-coverage degradation: {:?}",
                report.degraded
            ),
        )?;

        let manifest: JsonValue = serde_json::from_slice(
            &fs::read(&report.manifest_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        ensure_equal(
            manifest.pointer("/recoveryInventory/snapshotCoverageComplete"),
            Some(&JsonValue::Bool(false)),
            "manifest snapshot coverage posture",
        )?;
        let verify = verify_backup(&BackupVerifyOptions {
            backup_path: PathBuf::from(report.backup_path),
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            verify.status.as_str(),
            "degraded",
            "partial backup integrity verification status",
        )
    }

    #[test]
    fn task_episode_coverage_is_independent_of_optional_cache_capture() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_id = connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "missing fixture workspace".to_owned())?
            .id;
        connection
            .insert_task_episode(
                "ep_823456789012345678901234567",
                &CreateTaskEpisodeInput {
                    workspace_id: Some(workspace_id),
                    session_id: None,
                    task_input: "Recover the task using api_key=backup-secret-canary".to_owned(),
                    retrieved_memory_ids: Vec::new(),
                    context_pack_id: None,
                    actions: Vec::new(),
                    outcome: "success".to_owned(),
                    outcome_details: None,
                    started_at: "2026-09-01T00:00:00Z".to_owned(),
                    ended_at: None,
                    duration_ms: None,
                    agent: Some("codex".to_owned()),
                    episode_hash: Some("blake3:inventory-fixture".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let portable_only = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: Some(workspace.join("portable-only-backups")),
            label: Some("without-derived-episodes".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let portable_episode_inventory = portable_only
            .recovery_inventory
            .entries
            .iter()
            .find(|entry| entry.table == "task_episodes")
            .ok_or_else(|| "recovery inventory omitted task_episodes".to_owned())?;
        ensure_equal(
            portable_episode_inventory.coverage.as_str(),
            "derived_artifact_restore",
            "task episode recovery mechanism",
        )?;
        ensure(
            portable_episode_inventory.snapshot_covered,
            "task episode row is captured with optional caches disabled",
        )?;
        ensure_equal(
            portable_only.status.as_str(),
            "completed",
            "portable-only task episode backup status",
        )?;
        let episode_asset = portable_only
            .derived
            .iter()
            .find(|asset| asset.kind == "lab_episode")
            .ok_or_else(|| "default backup omitted task episode".to_owned())?;
        let bytes = fs::read(Path::new(&portable_only.backup_path).join(&episode_asset.path))
            .map_err(|error| error.to_string())?;
        ensure(
            !String::from_utf8_lossy(&bytes).contains("backup-secret-canary"),
            "default task history must apply export secret redaction",
        )?;
        let episode: JsonValue =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        ensure_equal(
            episode.pointer("/episode/episodeHash"),
            Some(&JsonValue::Null),
            "redaction invalidates the original episode body hash",
        )?;

        let with_derived = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(workspace.join("derived-episode-backups")),
            label: Some("with-derived-episodes".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: true,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let derived_episode_inventory = with_derived
            .recovery_inventory
            .entries
            .iter()
            .find(|entry| entry.table == "task_episodes")
            .ok_or_else(|| "recovery inventory omitted task_episodes".to_owned())?;
        ensure(
            derived_episode_inventory.snapshot_covered,
            "complete task episode artifact capture must satisfy snapshot coverage",
        )?;
        ensure_equal(
            with_derived
                .derived
                .iter()
                .filter(|asset| {
                    asset.kind == "lab_episode" && asset.path.starts_with("derived/lab/episodes/")
                })
                .count(),
            1,
            "captured task episode artifact count",
        )?;
        ensure_equal(
            with_derived.status.as_str(),
            "completed",
            "derived task episode backup status",
        )
    }

    #[test]
    fn cass_recovery_coverage_requires_exact_aggregate_counts() -> TestResult {
        let mut inventory = BackupRecoveryInventory {
            entries: vec![
                BackupRecoveryInventoryEntry {
                    table: "sessions".to_owned(),
                    owner: "ingest".to_owned(),
                    disposition: "export_restore_required".to_owned(),
                    coverage: "derived_artifact_restore".to_owned(),
                    row_count: 2,
                    schema_covered: true,
                    snapshot_covered: false,
                },
                BackupRecoveryInventoryEntry {
                    table: "evidence_spans".to_owned(),
                    owner: "ingest".to_owned(),
                    disposition: "export_restore_required".to_owned(),
                    coverage: "derived_artifact_restore".to_owned(),
                    row_count: 1,
                    schema_covered: true,
                    snapshot_covered: false,
                },
            ],
            schema_coverage_complete: true,
            snapshot_coverage_complete: false,
            uncovered_required_table_count: 0,
            uncovered_required_row_count: 3,
            unclassified_table_count: 0,
        };
        let mut payloads = vec![
            derived_payload(
                "derived/cass/sessions-0000.json",
                "cass_sessions",
                "2026-09-02T00:00:00Z",
                None,
                json_payload_bytes(&json!({"sessions": [{}]}))
                    .map_err(|error| error.to_string())?,
            ),
            derived_payload(
                "derived/cass/evidence-spans-0000.json",
                "cass_evidence_spans",
                "2026-09-02T00:00:00Z",
                None,
                json_payload_bytes(&json!({"evidenceSpans": [{}]}))
                    .map_err(|error| error.to_string())?,
            ),
        ];

        reconcile_derived_recovery_inventory(&mut inventory, &payloads);
        let sessions = inventory
            .entries
            .iter()
            .find(|entry| entry.table == "sessions")
            .ok_or_else(|| "inventory omitted sessions".to_owned())?;
        let evidence = inventory
            .entries
            .iter()
            .find(|entry| entry.table == "evidence_spans")
            .ok_or_else(|| "inventory omitted evidence_spans".to_owned())?;
        ensure(
            !sessions.snapshot_covered,
            "partial session chunk cannot claim coverage",
        )?;
        ensure(
            evidence.snapshot_covered,
            "exact evidence aggregate count satisfies coverage",
        )?;
        ensure_equal(
            inventory.uncovered_required_row_count,
            2,
            "partial CASS aggregate uncovered row count",
        )?;

        payloads.push(derived_payload(
            "derived/cass/sessions-0001.json",
            "cass_sessions",
            "2026-09-02T00:00:00Z",
            None,
            json_payload_bytes(&json!({"sessions": [{}]})).map_err(|error| error.to_string())?,
        ));
        reconcile_derived_recovery_inventory(&mut inventory, &payloads);
        ensure(
            inventory.snapshot_coverage_complete,
            "exact chunk totals satisfy CASS snapshot coverage",
        )?;
        ensure_equal(
            inventory.uncovered_required_row_count,
            0,
            "complete CASS aggregate uncovered row count",
        )
    }

    #[test]
    fn portable_cass_session_rebackup_preserves_absent_source_metadata() -> TestResult {
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(3)).to_string();
        let record = backup_cass_session_fixture(
            &SessionId::from_uuid(Uuid::from_u128(4)).to_string(),
            &workspace_id,
        );
        let restored = record.clone().into_restored(workspace_id);

        ensure_equal(
            BackupCassSessionRecord::from_stored(&restored),
            record,
            "portable CASS session remains stable across a second backup",
        )
    }

    #[test]
    fn cass_recovery_rolls_back_sessions_when_evidence_reference_is_missing() -> TestResult {
        let (tempdir, _workspace, database) = fixture().map_err(|error| error.message())?;
        let source_workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(3)).to_string();
        let session_id = SessionId::from_uuid(Uuid::from_u128(4)).to_string();
        let missing_session_id = SessionId::from_uuid(Uuid::from_u128(5)).to_string();
        let evidence_id = EvidenceId::from_uuid(Uuid::from_u128(6)).to_string();
        let session_path = tempdir.path().join("sessions-0000.json");
        let evidence_path = tempdir.path().join("evidence-spans-0000.json");
        let session_chunk = BackupCassSessionChunk {
            schema: "ee.backup.derived.cass_sessions.v1".to_owned(),
            captured_at: "2026-09-02T00:00:00Z".to_owned(),
            chunk_index: 0,
            source_locator_policy: "omitted_host_local".to_owned(),
            sessions: vec![backup_cass_session_fixture(
                &session_id,
                &source_workspace_id,
            )],
        };
        let evidence_chunk = BackupCassEvidenceChunk {
            schema: "ee.backup.derived.cass_evidence_spans.v1".to_owned(),
            captured_at: "2026-09-02T00:00:00Z".to_owned(),
            chunk_index: 0,
            evidence_spans: vec![backup_cass_evidence_fixture(
                &evidence_id,
                &source_workspace_id,
                &missing_session_id,
            )],
        };
        fs::write(
            &session_path,
            serialized_payload_bytes(&session_chunk).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            &evidence_path,
            serialized_payload_bytes(&evidence_chunk).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let assets = vec![
            restored_cass_asset(&session_path, "cass_sessions"),
            restored_cass_asset(&evidence_path, "cass_evidence_spans"),
        ];

        let error = restore_cass_assets(&database, &assets)
            .expect_err("dangling evidence reference must fail CASS recovery");
        ensure(
            error.to_string().contains("session does not exist"),
            format!("unexpected dangling-reference error: {error}"),
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        ensure_equal(
            connection
                .get_session(&session_id)
                .map_err(|error| error.to_string())?,
            None,
            "failed CASS transaction rolls back its inserted session",
        )?;
        ensure_equal(
            connection
                .get_evidence_span(&evidence_id)
                .map_err(|error| error.to_string())?,
            None,
            "failed CASS transaction leaves no evidence row",
        )
    }

    #[test]
    fn task_episode_derived_asset_round_trips_into_restored_database() -> TestResult {
        let (tempdir, _workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let restored_workspace_id = connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "missing restored workspace".to_owned())?
            .id;
        connection.close().map_err(|error| error.to_string())?;

        let source_episode = StoredTaskEpisode {
            id: "ep_723456789012345678901234567".to_owned(),
            workspace_id: Some(WorkspaceId::from_uuid(Uuid::from_u128(3)).to_string()),
            session_id: Some("sess_derived_restore_fixture".to_owned()),
            task_input: "Restore one task episode derived asset".to_owned(),
            retrieved_memory_ids: vec![MemoryId::from_uuid(Uuid::from_u128(2)).to_string()],
            context_pack_id: Some("pack_derived_restore_fixture".to_owned()),
            actions: vec![StoredEpisodeAction {
                action_type: "verify".to_owned(),
                target_id: Some("task_episode".to_owned()),
                details: Some("focused derived-asset round trip".to_owned()),
                timestamp: "2026-09-01T00:00:01Z".to_owned(),
            }],
            outcome: "success".to_owned(),
            outcome_details: Some("episode survived".to_owned()),
            started_at: "2026-09-01T00:00:00Z".to_owned(),
            ended_at: Some("2026-09-01T00:00:02Z".to_owned()),
            duration_ms: Some(2_000),
            agent: Some("codex".to_owned()),
            episode_hash: Some("blake3:focused-episode-restore-fixture".to_owned()),
            created_at: "2026-09-01T00:00:03Z".to_owned(),
        };
        let restore_path = tempdir.path().join("task-episode-derived.json");
        fs::write(
            &restore_path,
            json_payload_bytes(&task_episode_json(
                &source_episode,
                "2026-09-02T00:00:00Z",
                RedactionLevel::None,
                &BTreeMap::new(),
            ))
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let restored_derived = [BackupRestoredDerivedAssetReport {
            path: format!("derived/lab/episodes/{}.json", source_episode.id),
            kind: "lab_episode".to_owned(),
            restore_path: restore_path.to_string_lossy().into_owned(),
            lab_episode_path: None,
        }];

        let restored_count = restore_task_episode_assets(&database, &restored_derived)
            .map_err(|error| error.message())?;
        ensure_equal(restored_count, 1, "restored task episode count")?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let restored_episode = connection
            .get_task_episode(&source_episode.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "restored database omitted task episode".to_owned())?;
        let mut expected_episode = source_episode;
        expected_episode.workspace_id = Some(restored_workspace_id);
        ensure_equal(
            restored_episode,
            expected_episode,
            "focused task episode derived-asset round trip",
        )
    }

    #[test]
    fn backup_report_degraded_entries_are_aggregated() -> TestResult {
        let report = BackupListReport {
            schema: BACKUP_LIST_SCHEMA_V1,
            backup_root: "/tmp/ee-backups".to_owned(),
            backups: Vec::new(),
            degraded: vec![
                BackupDegradation::warning(
                    "backup_index_unavailable",
                    "index manifest unavailable",
                    "run ee index rebuild --workspace .",
                ),
                BackupDegradation::with_severity(
                    "backup_index_unavailable",
                    "high",
                    "index manifest and graph cache unavailable",
                    "rerun backup create with --include-graph-cache",
                ),
            ],
        };

        let json = report.data_json();
        let degraded = json
            .get("degraded")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| "expected degraded array".to_owned())?;

        ensure(
            degraded.len() == 1,
            format!("expected one aggregated degradation, got {degraded:?}"),
        )?;
        ensure(
            degraded[0].get("code").and_then(JsonValue::as_str) == Some("backup_index_unavailable"),
            format!("unexpected code: {:?}", degraded[0]),
        )?;
        ensure(
            degraded[0].get("severity").and_then(JsonValue::as_str) == Some("high"),
            format!("unexpected severity: {:?}", degraded[0]),
        )?;
        ensure(
            degraded[0].get("nextAction").and_then(JsonValue::as_str)
                == Some("rerun backup create with --include-graph-cache"),
            format!("unexpected nextAction: {:?}", degraded[0]),
        )?;
        ensure(
            degraded[0]
                .get("sources")
                .and_then(JsonValue::as_array)
                .is_some_and(|sources| {
                    sources == [JsonValue::String("backup_list".to_owned())].as_slice()
                }),
            format!("unexpected sources: {:?}", degraded[0]),
        )
    }

    #[test]
    fn dry_run_does_not_create_backup_directory() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("planned-backups");
        let keys_dir = workspace_keys_dir(&workspace);
        let database_dir = database
            .parent()
            .ok_or_else(|| "fixture database must have a parent directory".to_owned())?;
        let database_bytes_before = fs::read(&database).map_err(|error| error.to_string())?;
        let wal_path = database_sidecar_path(&database, "-wal");
        let shm_path = database_sidecar_path(&database, "-shm");
        let wal_bytes_before = optional_file_bytes(&wal_path)?;
        let shm_bytes_before = optional_file_bytes(&shm_path)?;
        let database_entries_before = directory_entry_names(database_dir)?;
        ensure(
            !keys_dir.exists(),
            "fixture must begin without a store-authentication key directory",
        )?;
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: Some(out.clone()),
            label: Some("pre-test".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        ensure_equal(report.status.as_str(), "dry_run", "dry run status")?;
        ensure_equal(
            report.verification_status.as_str(),
            "not_checked",
            "dry run verification",
        )?;
        ensure(!out.exists(), "dry run must not create output directory")?;
        ensure(
            !keys_dir.exists(),
            "dry run must not initialize the store-authentication key directory",
        )?;
        ensure(
            report.degraded.iter().all(|entry| {
                entry.code != crate::policy::store_auth::MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE
            }),
            "an absent key store is an expected dry-run state, not a degradation",
        )?;
        ensure_equal(
            fs::read(&database).map_err(|error| error.to_string())?,
            database_bytes_before,
            "dry run must not change database bytes",
        )?;
        ensure_equal(
            optional_file_bytes(&wal_path)?,
            wal_bytes_before,
            "dry run must not create or change the WAL sidecar",
        )?;
        ensure_equal(
            optional_file_bytes(&shm_path)?,
            shm_bytes_before,
            "dry run must not create or change the shared-memory sidecar",
        )?;
        ensure_equal(
            directory_entry_names(database_dir)?,
            database_entries_before,
            "dry run must not add database lock or journal artifacts",
        )
    }

    #[test]
    fn dry_run_loads_existing_store_auth_without_changing_it() -> TestResult {
        let (_tempdir, workspace, _database) = fixture().map_err(|error| error.message())?;
        let keys_dir = workspace_keys_dir(&workspace);
        let created = StoreAuthRoot::create(&keys_dir).map_err(|error| error.message())?;
        let key_path = keys_dir.join("store_auth_root.json");
        let bytes_before = fs::read(&key_path).map_err(|error| error.to_string())?;
        let entries_before = directory_entry_names(&keys_dir)?;
        let key_id_before = created.current_key_id();
        let mut degraded = Vec::new();

        let loaded =
            load_store_auth_for_backup(&workspace, true, &mut degraded).ok_or_else(|| {
                "dry run must load an initialized store-authentication root".to_owned()
            })?;

        ensure_equal(
            loaded.current_key_id(),
            key_id_before,
            "dry-run store-authentication key id",
        )?;
        ensure(
            degraded.is_empty(),
            format!("healthy existing key store must not degrade dry run: {degraded:?}"),
        )?;
        ensure_equal(
            fs::read(&key_path).map_err(|error| error.to_string())?,
            bytes_before,
            "dry run must not change store-authentication bytes",
        )?;
        ensure_equal(
            directory_entry_names(&keys_dir)?,
            entries_before,
            "dry run must not change store-authentication directory entries",
        )
    }

    #[cfg(unix)]
    #[test]
    fn dry_run_degrades_for_symlinked_store_auth_without_creating_backup() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let keys_target = workspace.join("dry-run-keys-elsewhere");
        fs::create_dir_all(&keys_target).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&keys_target, workspace_keys_dir(&workspace))
            .map_err(|error| error.to_string())?;
        let out = workspace.join("dry-run-degraded-backups");

        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(out.clone()),
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        let entry = report
            .degraded
            .iter()
            .find(|entry| {
                entry.code == crate::policy::store_auth::MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE
            })
            .ok_or_else(|| "symlinked key store must degrade a dry-run backup".to_owned())?;
        ensure_equal(entry.severity.as_str(), "high", "degraded severity")?;
        ensure(
            !out.exists(),
            "degraded dry run must not create backup output",
        )
    }

    #[test]
    fn dry_run_accepts_targetless_and_type_only_audits() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection
            .insert_audit(
                "audit_00000000000000000000000002",
                &CreateAuditInput {
                    workspace_id: Some(WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string()),
                    actor: Some("test".to_owned()),
                    action: "db.check_integrity".to_owned(),
                    target_type: None,
                    target_id: None,
                    details: Some(r#"{"status":"ok"}"#.to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_audit(
                "audit_00000000000000000000000003",
                &CreateAuditInput {
                    workspace_id: Some(WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string()),
                    actor: Some("test".to_owned()),
                    action: "search_completed".to_owned(),
                    target_type: Some("search".to_owned()),
                    target_id: None,
                    details: Some(r#"{"resultCount":1}"#.to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(workspace.join("planned-backups")),
            label: Some("integrity-audit".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        ensure_equal(report.status.as_str(), "dry_run", "dry run status")?;
        ensure_equal(
            report.audit_count,
            3,
            "backup must accept targeted, targetless, and type-only audits",
        )
    }

    #[test]
    fn audit_record_preserves_independently_optional_targets() -> TestResult {
        for (target_type, target_id, expected_type, expected_id, context) in [
            (None, None, None, None, "targetless audit"),
            (
                Some("search".to_owned()),
                None,
                Some("search"),
                None,
                "type-only audit",
            ),
            (
                None,
                Some("source-001".to_owned()),
                None,
                Some("source-001"),
                "id-only audit",
            ),
        ] {
            let stored = StoredAuditEntry {
                id: format!("audit-{context}"),
                workspace_id: Some(WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string()),
                timestamp: "2026-08-23T12:00:00Z".to_owned(),
                actor: Some("test".to_owned()),
                action: "backup.audit-shape".to_owned(),
                target_type,
                target_id,
                details: None,
                surface: "backup".to_owned(),
                mutation_kind: "backup.audit-shape".to_owned(),
                before_hash: None,
                after_hash: None,
                prev_row_hash: None,
                this_row_hash: None,
            };
            let exported =
                audit_record(&stored).map_err(|error| format!("{context} must export: {error}"))?;
            ensure_equal(
                exported.target_type.as_deref(),
                expected_type,
                &format!("{context} target_type"),
            )?;
            ensure_equal(
                exported.target_id.as_deref(),
                expected_id,
                &format!("{context} target_id"),
            )?;
        }

        Ok(())
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
            None,
            None,
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

    /// bd-multiplicity-aware-trust-p0u7g: the exported memory record must
    /// carry the full attempt-family block (pointer + slot + disposition +
    /// origin) and a family-less memory must serialize without the key, so
    /// restore can rebuild the ledger without inference and old backups stay
    /// byte-compatible.
    #[test]
    fn memory_record_preserves_attempt_family_block() -> TestResult {
        let family = crate::models::ExportAttemptFamilyRecord {
            family_id: "fam-backup-a".to_owned(),
            declared_size: Some(18),
            attempt_index: Some(1),
            disposition: Some("selected".to_owned()),
            origin: Some("declared".to_owned()),
        };
        let record = memory_record(
            &stored_memory_fixture("mem_00000000000000000000000002"),
            None,
            None,
            Some(&family),
        )
        .map_err(|error| error.to_string())?;
        let exported = record
            .attempt_family
            .as_ref()
            .ok_or_else(|| "attempt family block missing from export record".to_string())?;
        ensure_equal(exported, &family, "attempt family block round-trips")?;
        let line = serde_json::to_string(&record).map_err(|error| error.to_string())?;
        ensure(
            line.contains("\"attempt_family\"") && line.contains("fam-backup-a"),
            "serialized record must carry the attempt_family key",
        )?;
        let reparsed: crate::models::ExportMemoryRecord =
            serde_json::from_str(&line).map_err(|error| error.to_string())?;
        ensure_equal(
            &reparsed.attempt_family,
            &Some(family),
            "attempt family survives serde round-trip",
        )?;

        let without = memory_record(
            &stored_memory_fixture("mem_00000000000000000000000003"),
            None,
            None,
            None,
        )
        .map_err(|error| error.to_string())?;
        let plain_line = serde_json::to_string(&without).map_err(|error| error.to_string())?;
        ensure(
            !plain_line.contains("attempt_family"),
            "family-less memories serialize without the attempt_family key",
        )
    }

    #[test]
    fn revised_family_memory_exports_one_ledger_slot_on_current_head() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_record =
            load_workspace(&connection, &workspace).map_err(|error| error.message())?;
        let original_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        connection
            .set_memory_attempt_family(
                &original_id,
                &crate::db::MemoryAttemptFamily {
                    family_id: "fam-backup-revision".to_owned(),
                    declared_size: Some(3),
                    attempt_index: Some(1),
                    disposition: Some("selected".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let revised_id = MemoryId::from_uuid(Uuid::from_u128(0xfeed)).to_string();
        connection
            .with_transaction(|| {
                connection.expire_memory_valid_to(&original_id, "2026-08-09T00:00:00Z")?;
                connection.insert_memory_revision(
                    &revised_id,
                    &original_id,
                    &CreateMemoryInput {
                        workspace_id: workspace_record.id.clone(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: "Revised selected attempt survives backup restore.".to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.7,
                        importance: 0.8,
                        provenance_uri: Some("ee-test://backup-revision".to_owned()),
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: Some("fixture".to_owned()),
                        tags: Vec::new(),
                        valid_from: Some("2026-08-09T00:00:00Z".to_owned()),
                        valid_to: None,
                    },
                )?;
                connection.carry_memory_attempt_family_pointer(&original_id, &revised_id)?;
                Ok(())
            })
            .map_err(|error| error.to_string())?;

        let export =
            load_export_data(&connection, workspace_record).map_err(|error| error.message())?;
        ensure(
            !export.attempt_families_by_memory.contains_key(&original_id),
            "superseded revision must not duplicate the logical family's ledger slot",
        )?;
        let current = export
            .attempt_families_by_memory
            .get(&revised_id)
            .ok_or_else(|| "current revision omitted family export block".to_owned())?;
        ensure_equal(
            current.attempt_index,
            Some(1),
            "current revision preserves the logical family slot",
        )?;
        ensure_equal(
            current.disposition.as_deref(),
            Some("selected"),
            "current revision preserves selected disposition",
        )
    }

    #[test]
    fn memory_record_preserves_export_graph_fields() -> TestResult {
        let record = memory_record(
            &stored_memory_fixture("mem_00000000000000000000000002"),
            None,
            Some(&BackupMemoryGraphFields {
                pagerank_score: Some(0.12),
                betweenness_score: Some(0.34),
                hits_authority: Some(0.56),
                hits_hub: Some(0.78),
                onion_layer: Some(3),
                k_truss_max: Some(4),
                articulation_point: Some(true),
                bayes_alpha: Some(2.5),
                bayes_beta: Some(1.5),
            }),
            None,
        )
        .map_err(|error| error.to_string())?;

        ensure_equal(record.pagerank_score, Some(0.12), "pagerank_score")?;
        ensure_equal(record.betweenness_score, Some(0.34), "betweenness_score")?;
        ensure_equal(record.hits_authority, Some(0.56), "hits_authority")?;
        ensure_equal(record.hits_hub, Some(0.78), "hits_hub")?;
        ensure_equal(record.onion_layer, Some(3), "onion_layer")?;
        ensure_equal(record.k_truss_max, Some(4), "k_truss_max")?;
        ensure_equal(record.articulation_point, Some(true), "articulation_point")?;
        ensure_equal(record.bayes_alpha, Some(2.5), "bayes_alpha")?;
        ensure_equal(record.bayes_beta, Some(1.5), "bayes_beta")
    }

    #[test]
    fn backup_create_writes_records_and_manifest_with_hashes() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("pre-test".to_owned()),
            redaction_level: RedactionLevel::Minimal,
            include_derived: false,
            include_graph_cache: false,
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
    fn backup_create_authenticates_the_records_footer() -> TestResult {
        use crate::policy::import_auth::{
            ImportAuthOutcome, RecordsRootBuilder, canonical_record_hash, verify_artifact,
        };

        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("auth-backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            !report.degraded.iter().any(|entry| {
                entry.code == crate::policy::store_auth::MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE
            }),
            "a healthy workspace must not degrade store authentication",
        )?;

        // Recompute the records root exactly the way slice-4 import will: over
        // the raw emitted memory line bytes, in order.
        let records =
            fs::read_to_string(&report.records_path).map_err(|error| error.to_string())?;
        let mut builder = RecordsRootBuilder::new();
        let mut footer = None;
        for line in records.lines() {
            let value: JsonValue = serde_json::from_str(line).map_err(|error| error.to_string())?;
            match value.get("schema").and_then(JsonValue::as_str) {
                Some("ee.export.memory.v1") => {
                    let memory_id = value
                        .get("memory_id")
                        .and_then(JsonValue::as_str)
                        .ok_or_else(|| "memory record is missing memory_id".to_owned())?;
                    builder.push(memory_id, &canonical_record_hash(line.as_bytes()));
                }
                Some("ee.export.footer.v1") => {
                    footer = Some(
                        serde_json::from_str::<ExportFooter>(line)
                            .map_err(|error| error.to_string())?,
                    );
                }
                _ => {}
            }
        }
        let footer = footer.ok_or_else(|| "records JSONL has no footer".to_owned())?;
        let header = footer
            .authentication
            .ok_or_else(|| "footer must carry a store-local authentication block".to_owned())?;
        ensure_equal(header.record_count, report.memory_count, "record count")?;

        let root =
            StoreAuthRoot::open(workspace_keys_dir(&workspace)).map_err(|error| error.message())?;
        let context = ArtifactContext {
            artifact_family: EXPORT_ARTIFACT_FAMILY,
            record_encoding_version: EXPORT_RECORD_ENCODING_V1,
            source_key_namespace: STORE_KEY_NAMESPACE_V1,
            workspace_scope: &report.workspace_id,
        };
        let outcome = verify_artifact(
            &root,
            MacDomain::NativeImportRecordsRoot,
            &context,
            &header,
            &builder.finalize(),
            builder.count(),
        )
        .map_err(|error| error.message())?;
        ensure(
            matches!(outcome, ImportAuthOutcome::Authenticated { .. }),
            format!("recomputed records must authenticate, got {outcome:?}"),
        )
    }

    #[cfg(unix)]
    #[test]
    fn backup_create_degrades_when_the_key_store_is_symlinked() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let keys_target = workspace.join("keys-elsewhere");
        fs::create_dir_all(&keys_target).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&keys_target, workspace_keys_dir(&workspace))
            .map_err(|error| error.to_string())?;

        let out = workspace.join("degraded-backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let entry = report
            .degraded
            .iter()
            .find(|entry| {
                entry.code == crate::policy::store_auth::MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE
            })
            .ok_or_else(|| "symlinked key store must degrade the backup".to_owned())?;
        ensure_equal(entry.severity.as_str(), "high", "degraded severity")?;

        let records =
            fs::read_to_string(&report.records_path).map_err(|error| error.to_string())?;
        let footer_line = records
            .lines()
            .find(|line| line.contains(r#""schema":"ee.export.footer.v1""#))
            .ok_or_else(|| "records JSONL has no footer".to_owned())?;
        let footer: ExportFooter =
            serde_json::from_str(footer_line).map_err(|error| error.to_string())?;
        ensure(
            footer.authentication.is_none(),
            "an unauthenticated backup must not carry an authentication block",
        )
    }

    #[test]
    fn backup_create_omits_graph_fields_when_no_graph_evidence_exists() -> TestResult {
        // Without an imported graph snapshot, structural links, or imported
        // graph fields, the backup MUST NOT emit placeholder zero/default
        // graph metrics — absent evidence stays absent. Bayes posterior fields
        // are DB-backed memory columns and may be exported independently.
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("no-graph-evidence-backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("no-graph-evidence".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let records =
            fs::read_to_string(&report.records_path).map_err(|error| error.to_string())?;
        let memory_record = records
            .lines()
            .find(|line| line.contains(r#""schema":"ee.export.memory.v1""#))
            .ok_or_else(|| "backup JSONL memory record missing".to_owned())?;

        for absent in [
            "pagerank_score",
            "betweenness_score",
            "hits_authority",
            "hits_hub",
            "onion_layer",
            "k_truss_max",
            "articulation_point",
        ] {
            ensure(
                !memory_record.contains(absent),
                format!(
                    "memory export record must NOT include placeholder {absent} without real evidence: {memory_record}"
                ),
            )?;
        }
        Ok(())
    }

    #[test]
    fn backup_create_preserves_imported_graph_fields_without_snapshot() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("imported-workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let source_path = tempdir.path().join("source-with-graph-fields.jsonl");
        fs::write(&source_path, sample_import_jsonl_with_graph_fields())
            .map_err(|error| error.to_string())?;

        let import_report = import_jsonl_records(&JsonlImportOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            source_path,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(import_report.status.as_str(), "completed", "import status")?;
        ensure(
            import_report.issues.is_empty(),
            format!("import should not emit issues: {:?}", import_report.issues),
        )?;

        let out = workspace.join("imported-graph-field-backups");
        let backup_report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            output_dir: Some(out),
            label: Some("imported-graph-fields".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let records =
            fs::read_to_string(&backup_report.records_path).map_err(|error| error.to_string())?;
        let memory_record = records
            .lines()
            .find(|line| line.contains(r#""schema":"ee.export.memory.v1""#))
            .ok_or_else(|| "backup JSONL memory record missing".to_owned())?;

        for expected in [
            r#""pagerank_score":0.12"#,
            r#""betweenness_score":0.34"#,
            r#""hits_authority":0.56"#,
            r#""hits_hub":0.78"#,
            r#""onion_layer":3"#,
            r#""k_truss_max":4"#,
            r#""articulation_point":true"#,
            r#""bayes_alpha":2.5"#,
            r#""bayes_beta":1.5"#,
        ] {
            ensure(
                memory_record.contains(expected),
                format!("memory export record must preserve {expected}: {memory_record}"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn backup_create_exports_bayes_and_persisted_centrality_fields() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_record =
            load_workspace(&connection, &workspace).map_err(|error| error.message())?;
        let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        connection
            .update_memory_bayes_posterior(&memory_id, 2.5, 1.5)
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                "gsnap_0000000000000000000009100",
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_record.id,
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot.metrics.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 1,
                    edge_count: 0,
                    metrics_json: json!({
                        "nodes": [{
                            "id": memory_id,
                            "pagerank": 0.42,
                            "betweenness": 0.24,
                            "hub": 0.66,
                            "authority": 0.88
                        }],
                        "edges": []
                    })
                    .to_string(),
                    content_hash: "blake3:backup-centrality-fields".to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let out = workspace.join("backups-with-graph-fields");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("graph-fields".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let records =
            fs::read_to_string(&report.records_path).map_err(|error| error.to_string())?;
        ensure(
            records.contains(r#""pagerank_score":0.42"#),
            "backup JSONL must include pagerank_score",
        )?;
        ensure(
            records.contains(r#""betweenness_score":0.24"#),
            "backup JSONL must include betweenness_score",
        )?;
        ensure(
            records.contains(r#""hits_hub":0.66"#),
            "backup JSONL must include hits_hub",
        )?;
        ensure(
            records.contains(r#""hits_authority":0.88"#),
            "backup JSONL must include hits_authority",
        )?;
        ensure(
            records.contains(r#""bayes_alpha":2.5"#),
            "backup JSONL must include bayes_alpha",
        )?;
        ensure(
            records.contains(r#""bayes_beta":1.5"#),
            "backup JSONL must include bayes_beta",
        )
    }

    #[test]
    fn backup_export_filters_denied_mesh_links() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_record =
            load_workspace(&connection, &workspace).map_err(|error| error.message())?;
        let primary_memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        let secondary_memory_id = MemoryId::from_uuid(Uuid::from_u128(0x8402)).to_string();
        let allowed_link_id = MemoryLinkId::from_uuid(Uuid::from_u128(0x8403)).to_string();
        let denied_link_id = MemoryLinkId::from_uuid(Uuid::from_u128(0x8404)).to_string();
        connection
            .insert_memory(
                &secondary_memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_record.id.clone(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "Secondary memory for backup mesh filtering.".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.6,
                    importance: 0.7,
                    provenance_uri: Some("ee-test://backup-secondary".to_owned()),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: Some("fixture".to_owned()),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_link(
                &allowed_link_id,
                &CreateMemoryLinkInput {
                    src_memory_id: primary_memory_id.clone(),
                    dst_memory_id: secondary_memory_id.clone(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("backup-mesh-test".to_owned()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_link(
                &denied_link_id,
                &CreateMemoryLinkInput {
                    src_memory_id: secondary_memory_id.clone(),
                    dst_memory_id: primary_memory_id.clone(),
                    relation: MemoryLinkRelation::Contradicts,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("backup-mesh-test".to_owned()),
                    metadata_json: Some(backup_denied_mesh_link_metadata()),
                },
            )
            .map_err(|error| error.to_string())?;

        let export =
            load_export_data(&connection, workspace_record).map_err(|error| error.message())?;

        ensure_equal(export.links.len(), 1, "visible exported link count")?;
        ensure_equal(
            export.links[0].id.as_str(),
            allowed_link_id.as_str(),
            "only allowed local link exported",
        )
    }

    #[test]
    fn backup_manifest_summarizes_mesh_without_credentials_and_restore_warns() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_record =
            load_workspace(&connection, &workspace).map_err(|error| error.message())?;
        let local_memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        seed_mesh_backup_fixture(&connection, &workspace_record.id, &local_memory_id)?;

        let out = workspace.join("mesh-backups");
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("mesh-dr".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let manifest_text =
            fs::read_to_string(&report.manifest_path).map_err(|error| error.to_string())?;
        ensure(
            !manifest_text.contains("secret-peer-token")
                && !manifest_text.contains("secret-body-ref"),
            "mesh backup manifest must not include peer credentials or cached body refs",
        )?;
        let manifest =
            serde_json::from_str::<JsonValue>(&manifest_text).map_err(|error| error.to_string())?;
        ensure_equal(
            manifest
                .pointer("/mesh/included")
                .and_then(JsonValue::as_bool),
            Some(true),
            "manifest mesh included flag",
        )?;
        ensure_equal(
            manifest
                .pointer("/mesh/tables/mesh_peers")
                .and_then(JsonValue::as_u64),
            Some(1),
            "manifest mesh peer count",
        )?;
        ensure_equal(
            manifest
                .pointer("/mesh/tables/mesh_peer_cursors")
                .and_then(JsonValue::as_u64),
            Some(1),
            "manifest mesh cursor count",
        )?;
        ensure_equal(
            manifest
                .pointer("/mesh/tables/mesh_import_ledger")
                .and_then(JsonValue::as_u64),
            Some(1),
            "manifest mesh event count",
        )?;
        ensure_equal(
            manifest
                .pointer("/mesh/tables/mesh_memory_mappings")
                .and_then(JsonValue::as_u64),
            Some(1),
            "manifest mesh mapping count",
        )?;
        ensure_equal(
            manifest
                .pointer("/mesh/restorePolicy/peerCredentials")
                .and_then(JsonValue::as_str),
            Some("redacted"),
            "manifest mesh credential policy",
        )?;
        ensure_equal(
            manifest
                .pointer("/mesh/tables/mesh_body_cache_metadata")
                .and_then(JsonValue::as_u64),
            Some(1),
            "manifest mesh body cache count",
        )?;

        let side_path = tempdir.path().join("mesh-restore-side");
        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&report.backup_path),
            side_path,
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        ensure(
            restored
                .degraded
                .iter()
                .any(|entry| entry.code == "mesh_restore_requires_repair"),
            "mesh restore must warn that peers need explicit repair",
        )?;
        ensure(
            restored
                .next_actions
                .iter()
                .any(|action| action.contains("ee mesh doctor")),
            "mesh restore next actions include mesh doctor",
        )
    }

    #[test]
    fn restore_next_actions_shell_quote_unsafe_side_path() -> TestResult {
        let side_path = Path::new("/tmp/restore dir/it' ll");
        let base_actions = restore_base_next_actions("backup-20260501", side_path);

        ensure_equal(
            base_actions,
            vec![
                "ee backup inspect backup-20260501 --json".to_owned(),
                "ee search \"<query>\" --workspace '/tmp/restore dir/it'\\'' ll' --json".to_owned(),
            ],
            "restore base next actions quote shell-unsafe side paths",
        )?;
        ensure_equal(
            restore_mesh_doctor_next_action(side_path),
            "ee mesh doctor --workspace '/tmp/restore dir/it'\\'' ll' --json".to_owned(),
            "restore mesh doctor next action quotes shell-unsafe side path",
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
            include_graph_cache: false,
            dry_run: false,
        });

        match result {
            Err(DomainError::WorkspaceStoreMissing {
                message, repair, ..
            }) => {
                // Exit-10 storeless-miss contract: an addressed-but-absent
                // store is an addressing miss, not a storage failure.
                ensure(
                    message.contains("Database not found"),
                    "missing database should be explicit",
                )?;
                ensure(
                    repair
                        .as_deref()
                        .is_some_and(|repair| repair.contains("ee init --workspace")),
                    "repair keeps conditional init last",
                )
            }
            other => Err(format!(
                "expected workspace-store-missing error, got {other:?}"
            )),
        }
    }

    #[test]
    fn unreadable_database_repair_uses_current_migrate_command() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database = tempdir.path().join("empty.db");
        File::create(&database).map_err(|error| error.to_string())?;

        let result = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        });

        match result {
            Err(DomainError::Storage { repair, .. }) => ensure_equal(
                repair.as_deref(),
                Some(INIT_AND_MIGRATE_REPAIR_COMMAND),
                "repair",
            ),
            other => Err(format!("expected storage error, got {other:?}")),
        }
    }

    #[cfg(unix)]
    #[test]
    fn create_backup_rejects_symlinked_output_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let real_output_parent = tempdir.path().join("real-output-parent");
        fs::create_dir_all(&real_output_parent).map_err(|error| error.to_string())?;
        let output_parent_link = tempdir.path().join("linked-output-parent");
        symlink(&real_output_parent, &output_parent_link).map_err(|error| error.to_string())?;
        let output_dir = output_parent_link.join("backups");

        let result = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(output_dir),
            label: Some("symlink-output".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        });

        match result {
            Err(DomainError::PolicyDenied { message, repair }) => {
                ensure(
                    message.contains("traverses symbolic link"),
                    "symlinked output parent is rejected",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some("choose a real, non-symlink directory for --output-dir"),
                    "symlinked output repair",
                )?;
            }
            other => return Err(format!("expected policy denied error, got {other:?}")),
        }
        ensure(
            !real_output_parent.join("backups").exists(),
            "backup creation must not write through a symlinked output parent",
        )
    }

    #[test]
    fn generated_report_uses_stable_response_schema() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let report = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
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
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("inspect".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
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
            include_graph_cache: false,
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
    fn backup_list_symlink_scan_accepts_absolute_roots() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let canonical_root = fs::canonicalize(tempdir.path()).map_err(|error| error.to_string())?;
        let candidate = canonical_root.join("missing-backup-root");

        let result = backup_list_symlink_component(&candidate)
            .map_err(|error| format!("absolute backup list scan should not fail: {error:?}"))?;

        ensure_equal(result, None, "absolute backup list symlink scan result")
    }

    #[cfg(unix)]
    #[test]
    fn list_backups_rejects_symlinked_backup_root() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let real_root = tempdir.path().join("real-backups");
        fs::create_dir_all(&real_root).map_err(|error| error.to_string())?;
        let linked_root = workspace.join("linked-backups");
        symlink(&real_root, &linked_root).map_err(|error| error.to_string())?;

        let result = list_backups(&BackupListOptions {
            workspace_path: workspace,
            output_dir: Some(linked_root),
        });

        match result {
            Err(DomainError::Storage { message, repair }) => {
                ensure(
                    message.contains("symbolic link"),
                    "symlinked backup root should be rejected explicitly",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some("choose a real, non-symlink directory with --output-dir"),
                    "symlinked backup root repair",
                )
            }
            other => Err(format!("expected storage error, got {other:?}")),
        }
    }

    #[cfg(unix)]
    #[test]
    fn list_backups_skips_symlinked_backup_entry_before_inspect() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        let backup_root = workspace.join("backups");
        fs::create_dir_all(&backup_root).map_err(|error| error.to_string())?;
        let real_backup = tempdir.path().join("real-backup");
        fs::create_dir_all(&real_backup).map_err(|error| error.to_string())?;
        fs::write(
            real_backup.join(MANIFEST_FILE),
            serde_json::to_vec(&json!({
                "schema": BACKUP_MANIFEST_SCHEMA_V1,
                "backupId": "backup-test",
                "artifacts": [],
            }))
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        symlink(&real_backup, backup_root.join("linked-backup"))
            .map_err(|error| error.to_string())?;

        let listed = list_backups(&BackupListOptions {
            workspace_path: workspace,
            output_dir: Some(backup_root),
        })
        .map_err(|error| error.message())?;

        ensure(
            listed.backups.is_empty(),
            "symlinked backup entry must not be inspected as a backup",
        )?;
        ensure(
            listed.degraded.iter().any(|degradation| {
                degradation.code == "backup_manifest_unreadable"
                    && degradation.message.contains("symbolic link")
            }),
            "symlinked backup entry should be reported with existing unreadable code",
        )
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
            include_graph_cache: false,
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
            include_graph_cache: true,
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

    #[cfg(unix)]
    #[test]
    fn include_derived_skips_symlinked_index_manifest_before_read() -> TestResult {
        use std::os::unix::fs::symlink;

        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let index_dir = workspace.join(WORKSPACE_MARKER).join("index");
        fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        let outside_manifest = tempdir.path().join("outside-index-manifest.json");
        fs::write(&outside_manifest, r#"{"schema":"outside.index.v1"}"#)
            .map_err(|error| error.to_string())?;
        symlink(&outside_manifest, index_dir.join("meta.json"))
            .map_err(|error| error.to_string())?;

        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace,
            database_path: Some(database),
            output_dir: Some(tempdir.path().join("backups")),
            label: Some("derived-symlink".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: true,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            !created
                .derived
                .iter()
                .any(|derived| derived.kind == "index_manifest"),
            "symlinked index manifest must not be included as a derived asset",
        )?;
        ensure(
            created
                .degraded
                .iter()
                .any(|degradation| degradation.code == "index_manifest_symlink"),
            "symlinked index manifest should be reported as degraded",
        )?;
        ensure(
            created
                .degraded
                .iter()
                .any(|degradation| degradation.code == "index_manifest_missing"),
            "backup should still report no safe index manifest was included",
        )
    }

    #[test]
    fn include_graph_cache_preserves_graph_cache_assets_through_restore() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let workspace_id = connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "missing fixture workspace".to_owned())?
            .id;
        let snapshot_id = "gsnap_0000000000000000000000001";
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_id.clone(),
                    snapshot_version: 7,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 3,
                    edge_count: 2,
                    metrics_json: json!({"pagerank": {"mem": 0.5}}).to_string(),
                    content_hash: "blake3:graph-cache-fixture".to_owned(),
                    source_generation: 9,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_algorithm_witness(&CreateGraphAlgorithmWitnessInput {
                workspace_id: workspace_id.clone(),
                snapshot_id: snapshot_id.to_owned(),
                algorithm: "pagerank".to_owned(),
                params_json: json!({"alpha": 0.85}).to_string(),
                witness_json: json!({"pathDecisionHash": "blake3:witness"}).to_string(),
            })
            .map_err(|error| error.to_string())?;
        connection
            .upsert_graph_algorithm_result(&CreateGraphAlgorithmResultInput {
                workspace_id,
                snapshot_id: snapshot_id.to_owned(),
                algorithm: "pagerank".to_owned(),
                params_hash: "blake3:params".to_owned(),
                result_json: json!({"scores": {"mem": 0.5}}).to_string(),
                ttl_seconds: 3600,
            })
            .map_err(|error| error.to_string())?;
        drop(connection);

        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("graph-cache".to_owned()),
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            created
                .derived
                .iter()
                .any(|asset| asset.kind == "graph_snapshot"),
            "backup includes graph snapshot derived asset",
        )?;
        ensure(
            created
                .derived
                .iter()
                .any(|asset| asset.kind == "graph_algorithm_witness"),
            "backup includes graph algorithm witness derived asset",
        )?;
        ensure(
            created
                .derived
                .iter()
                .any(|asset| asset.kind == "graph_algorithm_result"),
            "backup includes graph algorithm result derived asset",
        )?;
        let manifest_text =
            fs::read_to_string(&created.manifest_path).map_err(|error| error.to_string())?;
        let mut manifest =
            serde_json::from_str::<JsonValue>(&manifest_text).map_err(|error| error.to_string())?;
        ensure_equal(
            manifest
                .pointer("/graphCache/included")
                .and_then(JsonValue::as_bool),
            Some(true),
            "manifest graph cache included",
        )?;
        ensure_equal(
            manifest
                .pointer("/graphCache/assetCounts/graphAlgorithmResults")
                .and_then(JsonValue::as_u64),
            Some(1),
            "manifest graph result count",
        )?;
        ensure(
            manifest
                .pointer("/graphCache/schemaVersion")
                .and_then(JsonValue::as_u64)
                .is_some(),
            "manifest records graph table schema version",
        )?;
        let current_schema_version = crate::db::MIGRATIONS
            .last()
            .map(crate::db::Migration::version)
            .ok_or_else(|| "missing compiled DB migrations".to_owned())?;
        let older_schema_version = current_schema_version
            .checked_sub(1)
            .ok_or_else(|| "compiled DB schema version cannot be downgraded for test".to_owned())?;
        manifest["graphCache"]["schemaVersion"] = json!(older_schema_version);
        let mut downgraded_manifest =
            serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
        downgraded_manifest.push(b'\n');
        fs::write(&created.manifest_path, downgraded_manifest)
            .map_err(|error| error.to_string())?;

        let side_path = tempdir.path().join("restore-graph-cache-side-path");
        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&created.backup_path),
            side_path,
            restore_graph_cache: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            restored.restored_graph_cache_count,
            3,
            "restore replays graph cache rows",
        )?;
        ensure(
            restored.degraded.iter().any(|degradation| {
                degradation.code == "graph_cache_schema_older_than_binary"
                    && degradation.severity == "warning"
                    && degradation
                        .message
                        .contains(&older_schema_version.to_string())
            }),
            "restore warns when backup graph cache schema is older than current binary",
        )?;

        let restored_connection =
            DbConnection::open_file(Path::new(&restored.restored_database_path))
                .map_err(|error| error.to_string())?;
        let restored_workspace_id = restored_connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "missing restored workspace".to_owned())?
            .id;
        let snapshots = restored_connection
            .list_graph_snapshots(
                &restored_workspace_id,
                Some(GraphSnapshotType::MemoryLinks),
                10,
            )
            .map_err(|error| error.to_string())?;
        ensure_equal(snapshots.len(), 1, "restored graph snapshot count")?;
        ensure_equal(
            snapshots[0].content_hash.as_str(),
            "blake3:graph-cache-fixture",
            "restored graph snapshot hash",
        )?;
        let witnesses = restored_connection
            .list_graph_algorithm_witnesses(&restored_workspace_id, snapshot_id, Some("pagerank"))
            .map_err(|error| error.to_string())?;
        ensure_equal(witnesses.len(), 1, "restored witness count")?;
        let results = restored_connection
            .list_graph_algorithm_results(&restored_workspace_id, snapshot_id, Some("pagerank"))
            .map_err(|error| error.to_string())?;
        ensure_equal(results.len(), 1, "restored result count")?;

        let skip_side_path = tempdir.path().join("restore-graph-cache-skip-side-path");
        let skipped = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: skip_side_path,
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            skipped.restored_graph_cache_count,
            0,
            "skip restore does not replay graph cache rows",
        )?;
        let skipped_connection =
            DbConnection::open_file(Path::new(&skipped.restored_database_path))
                .map_err(|error| error.to_string())?;
        let skipped_workspace_id = skipped_connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "missing skipped restored workspace".to_owned())?
            .id;
        let skipped_snapshots = skipped_connection
            .list_graph_snapshots(
                &skipped_workspace_id,
                Some(GraphSnapshotType::MemoryLinks),
                10,
            )
            .map_err(|error| error.to_string())?;
        ensure(
            skipped_snapshots.is_empty(),
            "skip restore leaves graph cache cold",
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
            include_graph_cache: true,
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
    fn backup_derived_assets_include_authoritative_shard_fanout_layout() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path().join("workspace");
        fs::create_dir_all(workspace.join(WORKSPACE_MARKER)).map_err(|error| error.to_string())?;
        let shard_root = tempdir.path().join("data/shards");
        fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
        let catalog_path = tempdir.path().join("data/catalog.db");
        fs::write(&catalog_path, b"catalog-db").map_err(|error| error.to_string())?;
        let workspace_id = "wsp_backup_shard";
        let shard_path = shard_root.join("wsp_backup_shard.db");
        fs::write(&shard_path, b"workspace-shard-db").map_err(|error| error.to_string())?;

        let status = resolve_shard_fanout_status(ShardFanoutResolverInput {
            enabled: true,
            workspace_id: Some(workspace_id.to_owned()),
            workspace_root: Some(workspace),
            shards_dir_override: Some(shard_root),
        });
        ensure_equal(
            status.posture,
            ShardFanoutPosture::Enabled,
            "shard fan-out fixture posture",
        )?;
        let mut degraded = Vec::new();
        let mut payloads = Vec::new();
        collect_shard_fanout_payloads_from_status(
            &status,
            "2026-05-21T00:00:00Z",
            &mut degraded,
            &mut payloads,
        );

        ensure(degraded.is_empty(), "authoritative shard assets are clean")?;
        ensure(
            payloads
                .iter()
                .any(|payload| payload.report.kind == "shard_fanout_catalog"),
            "catalog derived asset is included",
        )?;
        ensure(
            payloads
                .iter()
                .any(|payload| payload.report.kind == "shard_fanout_workspace_shard"),
            "workspace shard derived asset is included",
        )?;
        let manifest_payload = payloads
            .iter()
            .find(|payload| payload.report.kind == "shard_fanout_manifest")
            .ok_or_else(|| "shard fan-out manifest derived asset missing".to_owned())?;
        let manifest = serde_json::from_slice::<JsonValue>(&manifest_payload.bytes)
            .map_err(|error| format!("shard fan-out manifest must parse as JSON: {error}"))?;

        ensure_equal(
            manifest.get("schema").and_then(JsonValue::as_str),
            Some("ee.backup.derived.shard_fanout.v1"),
            "shard manifest schema",
        )?;
        ensure_equal(
            manifest.get("workspaceId").and_then(JsonValue::as_str),
            Some(workspace_id),
            "manifest workspace id",
        )?;
        ensure_equal(
            manifest
                .pointer("/catalog/backupPath")
                .and_then(JsonValue::as_str),
            Some("derived/shards/catalog.db"),
            "manifest catalog backup path",
        )?;
        ensure_equal(
            manifest
                .pointer("/redaction/status")
                .and_then(JsonValue::as_str),
            Some("not_applicable"),
            "manifest redaction posture",
        )
    }

    #[test]
    fn restore_shard_fanout_assets_reconstructs_side_path_layout() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let restore_artifacts = tempdir.path().join("restore-artifacts");
        fs::create_dir_all(restore_artifacts.join("derived/shards"))
            .map_err(|error| error.to_string())?;
        let catalog_restore_path = restore_artifacts.join("derived/shards/catalog.db");
        let shard_restore_path = restore_artifacts.join("derived/shards/wsp_restore.db");
        fs::write(&catalog_restore_path, b"catalog-copy").map_err(|error| error.to_string())?;
        fs::write(&shard_restore_path, b"shard-copy").map_err(|error| error.to_string())?;
        let manifest_restore_path = restore_artifacts.join("derived/shards/manifest.json");
        fs::write(
            &manifest_restore_path,
            json_payload_bytes(&json!({
                "schema": "ee.backup.derived.shard_fanout.v1",
                "catalog": {
                    "backupPath": "derived/shards/catalog.db",
                },
                "shards": [{
                    "shardId": "wsp_restore",
                    "backupPath": "derived/shards/wsp_restore.db",
                }],
            }))
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let restored_derived = vec![
            BackupRestoredDerivedAssetReport {
                path: "derived/shards/catalog.db".to_owned(),
                kind: "shard_fanout_catalog".to_owned(),
                restore_path: catalog_restore_path.to_string_lossy().into_owned(),
                lab_episode_path: None,
            },
            BackupRestoredDerivedAssetReport {
                path: "derived/shards/wsp_restore.db".to_owned(),
                kind: "shard_fanout_workspace_shard".to_owned(),
                restore_path: shard_restore_path.to_string_lossy().into_owned(),
                lab_episode_path: None,
            },
            BackupRestoredDerivedAssetReport {
                path: "derived/shards/manifest.json".to_owned(),
                kind: "shard_fanout_manifest".to_owned(),
                restore_path: manifest_restore_path.to_string_lossy().into_owned(),
                lab_episode_path: None,
            },
        ];
        let side_path = tempdir.path().join("restore-side-path");

        restore_shard_fanout_assets(&side_path, &restored_derived)
            .map_err(|error| error.message())?;

        let catalog = fs::read(side_path.join(WORKSPACE_MARKER).join("catalog.db"))
            .map_err(|error| error.to_string())?;
        let shard = fs::read(
            side_path
                .join(WORKSPACE_MARKER)
                .join("shards")
                .join("wsp_restore.db"),
        )
        .map_err(|error| error.to_string())?;
        ensure_equal(catalog, b"catalog-copy".to_vec(), "restored catalog bytes")?;
        ensure_equal(shard, b"shard-copy".to_vec(), "restored shard bytes")
    }

    #[test]
    fn verify_and_restore_report_wal_holds_orphaned_warning() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "CREATE TABLE IF NOT EXISTS ee_wal_holds (
                    workspace_id TEXT NOT NULL,
                    episode_id TEXT NOT NULL,
                    lsn TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    PRIMARY KEY (workspace_id, episode_id, lsn)
                )",
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "INSERT INTO ee_wal_holds
                    (workspace_id, episode_id, lsn, created_at, expires_at)
                 VALUES
                    ('ws_backup_wal_hold', 'ep_backup_wal_hold', 'lsn-backup-fixture',
                     '2026-01-01T00:00:00Z', '2026-12-31T00:00:00Z')",
            )
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
            include_graph_cache: true,
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
            restore_graph_cache: true,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(restored.status.as_str(), "degraded", "restore status")?;
        ensure(
            restored.issue_count >= 1,
            "restore reports at least the WAL-hold warning",
        )?;
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
            include_graph_cache: true,
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

    #[test]
    fn verify_backup_fails_required_artifact_without_hash() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let backup_path = tempdir.path().join("backup");
        fs::create_dir_all(&backup_path).map_err(|error| error.to_string())?;
        let records_payload = b"{\"schema\":\"ee.export.header.v1\"}\n";
        fs::write(backup_path.join(RECORDS_FILE), records_payload)
            .map_err(|error| error.to_string())?;
        let manifest = json!({
            "schema": BACKUP_MANIFEST_SCHEMA_V1,
            "backupId": "missing-required-artifact-hash",
            "artifacts": [{
                "path": RECORDS_FILE,
                "kind": "jsonl_export",
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

        ensure_equal(verified.status.as_str(), "failed", "verify status")?;
        ensure(
            verified.issues.iter().any(|issue| {
                issue.code == "artifact_hash_missing" && issue.path.as_deref() == Some(RECORDS_FILE)
            }),
            "verify must fail closed when required artifact hash is absent",
        )
    }

    #[test]
    fn verify_backup_fails_derived_asset_without_hash() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let backup_path = tempdir.path().join("backup");
        let derived_path = "derived/wal_holds.json";
        fs::create_dir_all(backup_path.join("derived")).map_err(|error| error.to_string())?;
        let records_payload = b"{\"schema\":\"ee.export.header.v1\"}\n";
        let derived_payload = b"{\"present\":false,\"rowCount\":0}\n";
        fs::write(backup_path.join(RECORDS_FILE), records_payload)
            .map_err(|error| error.to_string())?;
        fs::write(backup_path.join(derived_path), derived_payload)
            .map_err(|error| error.to_string())?;
        let manifest = json!({
            "schema": BACKUP_MANIFEST_SCHEMA_V2,
            "backupId": "missing-derived-hash",
            "artifacts": [{
                "path": RECORDS_FILE,
                "kind": "jsonl_export",
                "hash": hash_bytes(records_payload),
                "sizeBytes": records_payload.len(),
                "required": true,
            }],
            "derived": [{
                "path": derived_path,
                "kind": "wal_holds",
                "byte_size": derived_payload.len(),
                "captured_at": "2026-05-25T00:00:00Z",
            }],
        });
        let manifest_bytes =
            serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(backup_path.join(MANIFEST_FILE), manifest_bytes)
            .map_err(|error| error.to_string())?;

        let verified =
            verify_backup(&BackupVerifyOptions { backup_path }).map_err(|error| error.message())?;

        ensure_equal(verified.status.as_str(), "failed", "verify status")?;
        ensure(
            verified.issues.iter().any(|issue| {
                issue.code == "derived_asset_hash_missing"
                    && issue.path.as_deref() == Some(derived_path)
            }),
            "verify must fail closed when derived asset hash is absent",
        )
    }

    #[test]
    fn copy_derived_artifacts_rejects_restore_time_hash_drift() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let backup_path = tempdir.path().join("backup");
        let restore_artifact_dir = tempdir.path().join("restore-artifacts");
        let side_path = tempdir.path().join("restore-side-path");
        let derived_path = "derived/wal_holds.json";
        fs::create_dir_all(backup_path.join("derived")).map_err(|error| error.to_string())?;
        fs::create_dir_all(&restore_artifact_dir).map_err(|error| error.to_string())?;
        let trusted_bytes = b"{\"schema\":\"trusted\"}\n";
        fs::write(
            backup_path.join(derived_path),
            b"{\"schema\":\"tampered-after-verify\"}\n",
        )
        .map_err(|error| error.to_string())?;
        let inspect = BackupInspectReport {
            schema: BACKUP_INSPECT_SCHEMA_V1,
            backup_id: "backup_restore_hash_drift".to_owned(),
            label: None,
            created_at: None,
            ee_version: None,
            backup_path: backup_path.to_string_lossy().into_owned(),
            manifest_path: backup_path
                .join(MANIFEST_FILE)
                .to_string_lossy()
                .into_owned(),
            manifest_hash: "blake3:manifest".to_owned(),
            workspace_id: None,
            workspace_path: None,
            database_path: None,
            redaction_level: None,
            export_scope: None,
            counts: BackupCounts::default(),
            verification_status: Some("verified".to_owned()),
            artifacts: Vec::new(),
            derived: vec![BackupDerivedAssetReport {
                path: derived_path.to_owned(),
                kind: "wal_holds".to_owned(),
                hash: Some(hash_bytes(trusted_bytes)),
                byte_size: Some(trusted_bytes.len() as u64),
                captured_at: Some("2026-05-25T00:00:00Z".to_owned()),
                episode_id_if_lab: None,
            }],
            degraded: Vec::new(),
            issues: Vec::new(),
        };

        let result = copy_derived_artifacts_to_restore(
            &backup_path,
            &restore_artifact_dir,
            &side_path,
            &inspect,
        );

        match result {
            Err(DomainError::Import { message, repair }) => {
                ensure(
                    message.contains("hash changed during restore"),
                    "restore-time hash drift should be explicit",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some(
                        "rerun ee backup verify <backup-path> --json and restore from a trusted backup copy",
                    ),
                    "restore-time hash drift repair",
                )?;
            }
            other => return Err(format!("expected import error, got {other:?}")),
        }
        ensure(
            !restore_artifact_dir.join(derived_path).exists(),
            "restore must not copy a derived asset whose hash changed",
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
    fn inspect_backup_rejects_symlinked_backup_directory_before_canonicalize() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_backup_path = tempdir.path().join("real-backup");
        fs::create_dir_all(&real_backup_path).map_err(|error| error.to_string())?;
        fs::write(
            real_backup_path.join(MANIFEST_FILE),
            serde_json::to_vec(&json!({
                "schema": BACKUP_MANIFEST_SCHEMA_V1,
                "backupId": "backup-test",
                "artifacts": [],
            }))
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let linked_backup_path = tempdir.path().join("linked-backup");
        std::os::unix::fs::symlink(&real_backup_path, &linked_backup_path)
            .map_err(|error| error.to_string())?;

        let result = inspect_backup(&BackupInspectOptions {
            backup_path: linked_backup_path,
        });

        match result {
            Err(DomainError::PolicyDenied { message, repair }) => {
                ensure(
                    message.contains("traverses symbolic link"),
                    "symlinked backup directory should be rejected before canonicalization",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some("choose a self-contained backup directory"),
                    "symlinked backup directory repair",
                )
            }
            other => Err(format!("expected policy denied error, got {other:?}")),
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
            verified
                .checked_artifacts
                .iter()
                .all(|artifact| artifact.path == MANIFEST_FILE),
            "symlink artifact should not be hashed as backup evidence (only the manifest itself may be reported)",
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
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let side_path = tempdir.path().join("restore-side-path");

        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            restore_graph_cache: true,
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
        assert_backup_history_round_trip(true)
    }

    #[test]
    fn restore_backup_to_side_path_preserves_history_without_optional_caches() -> TestResult {
        assert_backup_history_round_trip(false)
    }

    #[test]
    fn backup_memory_id_mapping_rejects_redaction_collisions() -> TestResult {
        let (_tempdir, _workspace, database) = fixture().map_err(|error| error.message())?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let first = connection
            .get_memory(&MemoryId::from_uuid(Uuid::from_u128(2)).to_string())
            .map_err(|error| error.to_string())?
            .ok_or("missing fixture memory")?;
        let mut second = first.clone();
        second.id = MemoryId::from_uuid(Uuid::from_u128((1 << 30) + 2)).to_string();
        second.content = "A distinct memory with the same abbreviated ID suffix".to_owned();
        let memories = [first, second];
        for level in [
            RedactionLevel::None,
            RedactionLevel::Strict,
            RedactionLevel::Paranoid,
        ] {
            let ids =
                backup_memory_id_mapping(&memories, level).map_err(|error| error.message())?;
            ensure_equal(ids.len(), 2, "distinct source identities retained")?;
            ensure(
                ids.values().collect::<BTreeSet<_>>().len() == 2,
                "distinct restored identities",
            )?;
        }
        for level in [RedactionLevel::Standard, RedactionLevel::Full] {
            let error = backup_memory_id_mapping(&memories, level)
                .err()
                .ok_or("ambiguous redacted identities must reject the backup")?;
            ensure(
                error
                    .message()
                    .contains("distinct memories to the same identity"),
                "identity collision has a specific diagnostic",
            )?;
        }
        Ok(())
    }

    fn assert_backup_history_round_trip(include_derived: bool) -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let source_connection =
            DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let source_workspace_id = source_connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "missing source workspace".to_owned())?
            .id;
        let source_memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        let source_session_path = "/Users/alice/.local/share/cass/session.jsonl";
        let source_workspace_path = "/Users/alice/private/source-workspace";
        let source_session_id = SessionId::from_uuid(Uuid::from_u128(4)).to_string();
        source_connection
            .insert_session(
                &source_session_id,
                &CreateSessionInput {
                    workspace_id: source_workspace_id.clone(),
                    cass_session_id: source_session_path.to_owned(),
                    source_path: Some(source_session_path.to_owned()),
                    agent_name: Some("codex".to_owned()),
                    model: Some("gpt-5".to_owned()),
                    started_at: Some("2026-09-01T00:00:00Z".to_owned()),
                    ended_at: Some("2026-09-01T00:05:00Z".to_owned()),
                    message_count: 2,
                    token_count: Some(128),
                    content_hash:
                        "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .to_owned(),
                    metadata_json: Some(
                        json!({
                            "schema": "cass.session.v1",
                            "workspaceDir": source_workspace_path,
                        })
                        .to_string(),
                    ),
                },
            )
            .map_err(|error| error.to_string())?;
        let admitted_evidence_id = EvidenceId::from_uuid(Uuid::from_u128(5)).to_string();
        let admitted_excerpt = "Use the verified CASS evidence during release preparation.";
        source_connection
            .insert_evidence_span(
                &admitted_evidence_id,
                &CreateEvidenceSpanInput {
                    workspace_id: source_workspace_id.clone(),
                    session_id: source_session_id.clone(),
                    memory_id: Some(source_memory_id.clone()),
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("{source_session_path}:1"),
                    span_kind: "message".to_owned(),
                    start_line: 1,
                    end_line: 2,
                    start_byte: Some(0),
                    end_byte: Some(64),
                    role: Some("assistant".to_owned()),
                    excerpt: admitted_excerpt.to_owned(),
                    content_hash: hash_bytes(admitted_excerpt.as_bytes()),
                    metadata_json: Some(r#"{"source":"cass"}"#.to_owned()),
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;
        let denied_evidence_id = EvidenceId::from_uuid(Uuid::from_u128(6)).to_string();
        let denied_excerpt = "Documentation-derived context requires explicit curation.";
        source_connection
            .insert_evidence_span(
                &denied_evidence_id,
                &CreateEvidenceSpanInput {
                    workspace_id: source_workspace_id.clone(),
                    session_id: source_session_id.clone(),
                    memory_id: Some(source_memory_id),
                    producer_kind: EvidenceProducerKind::DocsBootstrap,
                    cass_span_id: "docs-bootstrap:recovery-fixture".to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: 3,
                    end_line: 4,
                    start_byte: Some(65),
                    end_byte: Some(128),
                    role: Some("docs_bootstrap".to_owned()),
                    excerpt: denied_excerpt.to_owned(),
                    content_hash: hash_bytes(denied_excerpt.as_bytes()),
                    metadata_json: Some(r#"{"source":"docs"}"#.to_owned()),
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;
        let source_session = source_connection
            .get_session(&source_session_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source CASS session was not stored".to_owned())?;
        let source_admitted_evidence = source_connection
            .get_evidence_span(&admitted_evidence_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source admitted evidence was not stored".to_owned())?;
        let source_denied_evidence = source_connection
            .get_evidence_span(&denied_evidence_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source denied evidence was not stored".to_owned())?;
        let task_episode_id = "ep_923456789012345678901234567";
        source_connection
            .insert_task_episode(
                task_episode_id,
                &CreateTaskEpisodeInput {
                    workspace_id: Some(source_workspace_id),
                    session_id: Some("sess_restore_fixture".to_owned()),
                    task_input: "Restore the durable task episode".to_owned(),
                    retrieved_memory_ids: vec![MemoryId::from_uuid(Uuid::from_u128(2)).to_string()],
                    context_pack_id: Some("pack_restore_fixture".to_owned()),
                    actions: vec![StoredEpisodeAction {
                        action_type: "verify".to_owned(),
                        target_id: Some("backup".to_owned()),
                        details: Some("round trip".to_owned()),
                        timestamp: "2026-09-01T00:00:01Z".to_owned(),
                    }],
                    outcome: "success".to_owned(),
                    outcome_details: Some("episode survived".to_owned()),
                    started_at: "2026-09-01T00:00:00Z".to_owned(),
                    ended_at: Some("2026-09-01T00:00:02Z".to_owned()),
                    duration_ms: Some(2_000),
                    agent: Some("codex".to_owned()),
                    episode_hash: Some("blake3:episode-restore-fixture".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let source_episode = source_connection
            .get_task_episode(task_episode_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source task episode was not stored".to_owned())?;
        let legacy_evidence_id = EvidenceId::from_uuid(Uuid::from_u128(7)).to_string();
        if !include_derived {
            let mut legacy = source_admitted_evidence.clone();
            legacy.id = legacy_evidence_id.clone();
            legacy.excerpt = "api_key=legacy-backup-secret-canary".to_owned();
            legacy.content_hash = hash_bytes(legacy.excerpt.as_bytes());
            legacy.canonical_excerpt_hash = Some(legacy.content_hash.clone());
            legacy.security_policy_epoch = 0;
            legacy.search_eligibility = "denied".to_owned();
            legacy.pack_eligibility = "denied".to_owned();
            legacy.cass_span_id = "/Users/alice/private/legacy-backup-secret-canary".to_owned();
            legacy.metadata_json = Some(r#"{"api_key":"legacy-backup-secret-canary"}"#.to_owned());
            source_connection
                .insert_evidence_span_for_recovery(&legacy)
                .map_err(|error| error.to_string())?;
        }
        source_connection
            .close()
            .map_err(|error| error.to_string())?;
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

        let out = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-derived".to_owned()),
            redaction_level: if include_derived {
                RedactionLevel::None
            } else {
                RedactionLevel::Standard
            },
            include_derived,
            include_graph_cache: include_derived,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let session_asset = created
            .derived
            .iter()
            .find(|asset| asset.kind == "cass_sessions")
            .ok_or_else(|| "backup omitted CASS session asset".to_owned())?;
        if !include_derived {
            for asset in &created.derived {
                let bytes = fs::read(Path::new(&created.backup_path).join(&asset.path))
                    .map_err(|error| error.to_string())?;
                ensure(
                    !String::from_utf8_lossy(&bytes).contains("legacy-backup-secret-canary"),
                    format!(
                        "default backup leaked legacy evidence through {}",
                        asset.kind
                    ),
                )?;
            }
        }
        let session_asset_text = String::from_utf8(
            fs::read(Path::new(&created.backup_path).join(&session_asset.path))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            !session_asset_text.contains(source_session_path)
                && !session_asset_text.contains(source_workspace_path),
            "portable CASS session asset omits host-local paths",
        )?;
        let side_path = tempdir
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join("restore-derived-side-path");

        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            restore_graph_cache: include_derived,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure_equal(restored.status.as_str(), "completed", "restore status")?;
        ensure_equal(
            restored.restored_task_episode_count,
            1,
            "restored task episode count",
        )?;
        ensure_equal(
            restored.restored_cass_session_count,
            1,
            "restored CASS session count",
        )?;
        ensure_equal(
            restored.restored_evidence_span_count,
            if include_derived { 2 } else { 3 },
            "restored evidence span count",
        )?;
        ensure_equal(
            restored
                .restored_derived
                .iter()
                .any(|derived| derived.kind == "wal_holds"),
            include_derived,
            "WAL diagnostics follow the optional cache flag",
        )?;
        ensure_equal(
            restored
                .restored_derived
                .iter()
                .any(|derived| derived.lab_episode_path.is_some()),
            include_derived,
            "frozen lab cache paths follow the optional cache flag",
        )?;
        ensure_equal(
            Path::new(&restored.restore_artifact_dir)
                .join("derived/wal_holds.json")
                .is_file(),
            include_derived,
            "WAL diagnostic file follows the optional cache flag",
        )?;
        ensure_equal(
            side_path
                .join(WORKSPACE_MARKER)
                .join("lab")
                .join("episodes")
                .join("ep_restore.json")
                .is_file(),
            include_derived,
            "frozen lab cache file follows the optional cache flag",
        )?;

        let restored_connection =
            DbConnection::open_file(Path::new(&restored.restored_database_path))
                .map_err(|error| error.to_string())?;
        let restored_episode = restored_connection
            .get_task_episode(task_episode_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "restored database omitted task episode".to_owned())?;
        let restored_workspace_id = restored_connection
            .list_workspaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "restored database omitted workspace".to_owned())?
            .id;
        let restored_memories = restored_connection
            .list_memories(&restored_workspace_id, None, true)
            .map_err(|error| error.to_string())?;
        ensure_equal(restored_memories.len(), 1, "history's memory was restored")?;
        let restored_memory_id = restored_memories[0].id.clone();
        ensure_equal(
            restored_memories[0].content.as_str(),
            if include_derived {
                "Authorization header should be redacted"
            } else {
                "[REDACTED]"
            },
            "history's memory content follows the selected export redaction policy",
        )?;
        ensure_equal(
            restored_memory_id == MemoryId::from_uuid(Uuid::from_u128(2)).to_string(),
            include_derived,
            "standard redaction remaps the memory identity; no redaction preserves it",
        )?;
        let restored_session = restored_connection
            .get_session(&source_session_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "restored database omitted CASS session".to_owned())?;
        let expected_session = BackupCassSessionRecord::from_stored(&source_session)
            .into_restored(restored_workspace_id.clone());
        ensure_equal(
            restored_session,
            expected_session,
            "portable CASS session round trip",
        )?;
        let restored_admitted_evidence = restored_connection
            .get_evidence_span(&admitted_evidence_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "restored database omitted admitted evidence".to_owned())?;
        let restored_denied_evidence = restored_connection
            .get_evidence_span(&denied_evidence_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "restored database omitted denied evidence".to_owned())?;
        if !include_derived {
            let legacy = restored_connection
                .get_evidence_span(&legacy_evidence_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "restored database omitted legacy evidence identity".to_owned())?;
            ensure(
                !legacy.excerpt.contains("legacy-backup-secret-canary"),
                "restored legacy excerpt stays redacted",
            )?;
            ensure_equal(
                legacy.metadata_json,
                None,
                "unchecked legacy metadata removed",
            )?;
            ensure_equal(
                legacy.search_eligibility.as_str(),
                "denied",
                "legacy search denied",
            )?;
            ensure_equal(
                legacy.pack_eligibility.as_str(),
                "denied",
                "legacy pack denied",
            )?;
            ensure_equal(
                legacy.security_policy_epoch,
                0,
                "redaction does not certify evidence",
            )?;
            ensure_equal(
                legacy.memory_id.as_deref(),
                Some(restored_memory_id.as_str()),
                "legacy evidence references the restored memory",
            )?;
        }
        let mut expected_admitted_evidence = source_admitted_evidence;
        expected_admitted_evidence.workspace_id = restored_workspace_id.clone();
        expected_admitted_evidence.memory_id = Some(restored_memory_id.clone());
        let mut expected_denied_evidence = source_denied_evidence;
        expected_denied_evidence.workspace_id = restored_workspace_id.clone();
        expected_denied_evidence.memory_id = Some(restored_memory_id.clone());
        ensure_equal(
            restored_admitted_evidence,
            expected_admitted_evidence,
            "admitted evidence round trip",
        )?;
        ensure_equal(
            restored_denied_evidence,
            expected_denied_evidence,
            "denied evidence round trip",
        )?;
        let (restored_admitted, restored_admission_report) = restored_connection
            .list_search_admitted_evidence_spans_for_workspace(&restored_workspace_id)
            .map_err(|error| error.to_string())?;
        ensure_equal(
            restored_admitted
                .iter()
                .map(|span| span.id.as_str())
                .collect::<Vec<_>>(),
            vec![admitted_evidence_id.as_str()],
            "restored live evidence admission",
        )?;
        ensure_equal(
            restored_admission_report
                .by_producer
                .get("docs_bootstrap")
                .map(|counts| counts.denied),
            Some(1),
            "restored denied evidence remains fail-closed",
        )?;
        ensure_equal(
            restored_episode.workspace_id.as_deref(),
            Some(restored_workspace_id.as_str()),
            "task episode workspace foreign key remaps to side path",
        )?;
        let mut expected_episode = source_episode;
        expected_episode.workspace_id = Some(restored_workspace_id);
        expected_episode.retrieved_memory_ids = vec![restored_memory_id];
        if !include_derived {
            expected_episode.episode_hash = None;
        }
        ensure_equal(
            restored_episode,
            expected_episode,
            "task episode round trip outside intentional workspace remap",
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
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let side_path = tempdir.path().join("restore-dry-run-side-path");

        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            restore_graph_cache: true,
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
            include_graph_cache: false,
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
            restore_graph_cache: true,
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

    #[test]
    fn restore_backup_rejects_side_path_inside_workspace() -> TestResult {
        let (_tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-inside-workspace".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let side_path = workspace.join("restore-side-path");

        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            restore_graph_cache: true,
            dry_run: false,
        });

        match result {
            Err(DomainError::PolicyDenied { message, repair }) => {
                ensure(
                    message.contains("outside source workspace"),
                    "workspace-contained side path is rejected",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some("choose a separate --side-path target outside the workspace"),
                    "workspace-contained side path repair",
                )?;
            }
            other => return Err(format!("expected policy denied error, got {other:?}")),
        }
        ensure(
            !side_path.exists(),
            "restore must not create a side path inside the source workspace",
        )
    }

    #[test]
    fn restore_backup_rejects_parent_dir_side_path_inside_workspace() -> TestResult {
        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-parent-dir-inside-workspace".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let outside_prefix = tempdir.path().join("outside-prefix");
        fs::create_dir_all(&outside_prefix).map_err(|error| error.to_string())?;
        let side_path = outside_prefix
            .join("..")
            .join("workspace")
            .join("restore-side-path");

        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            restore_graph_cache: true,
            dry_run: false,
        });

        match result {
            Err(DomainError::PolicyDenied { message, .. }) => ensure(
                message.contains("outside source workspace"),
                "parent-dir workspace-contained side path is rejected",
            )?,
            other => return Err(format!("expected policy denied error, got {other:?}")),
        }
        ensure(
            !workspace.join("restore-side-path").exists(),
            "restore must not resolve a parent-dir side path into the source workspace",
        )
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
            include_graph_cache: false,
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
            restore_graph_cache: true,
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

    #[cfg(unix)]
    #[test]
    fn restore_backup_rejects_symlinked_side_path_before_canonicalize() -> TestResult {
        use std::os::unix::fs::symlink;

        let (tempdir, workspace, database) = fixture().map_err(|error| error.message())?;
        let out = workspace.join("backups");
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: Some(out),
            label: Some("restore-symlink-side-path".to_owned()),
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        let real_side_path = tempdir.path().join("real-side-path");
        fs::create_dir_all(&real_side_path).map_err(|error| error.to_string())?;
        let linked_side_path = tempdir.path().join("linked-side-path");
        symlink(&real_side_path, &linked_side_path).map_err(|error| error.to_string())?;

        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: linked_side_path,
            restore_graph_cache: true,
            dry_run: false,
        });

        match result {
            Err(DomainError::PolicyDenied { message, repair }) => {
                ensure(
                    message.contains("symbolic link"),
                    "symlinked side path should be rejected before canonicalization",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some("choose a real, non-symlink directory for --side-path"),
                    "symlinked side path repair",
                )?;
            }
            other => return Err(format!("expected policy denied error, got {other:?}")),
        }
        ensure(
            fs::read_dir(&real_side_path)
                .map_err(|error| error.to_string())?
                .next()
                .is_none(),
            "restore must not write through a symlinked side path",
        )
    }

    #[test]
    fn backup_side_path_symlink_scan_accepts_absolute_roots() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let candidate = tempdir.path().join("restore-side-path");

        let result = first_existing_symlink_component(&candidate)
            .map_err(|error| format!("absolute side path scan should not fail: {error:?}"))?;

        ensure_equal(result, None, "absolute side path symlink scan result")
    }

    #[cfg(unix)]
    #[test]
    fn write_new_relative_file_rejects_symlinked_parent_before_write() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = tempdir.path().join("backup");
        let outside = tempdir.path().join("outside");
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
        symlink(&outside, root.join("derived")).map_err(|error| error.to_string())?;

        let result = write_new_relative_file(&root, "derived/payload.bin", b"payload");

        match result {
            Err(DomainError::PolicyDenied { message, repair }) => {
                ensure(
                    message.contains("traverses symbolic link"),
                    "symlinked relative artifact parent is rejected",
                )?;
                ensure_equal(
                    repair.as_deref(),
                    Some("replace symlinked backup artifact paths with real directories"),
                    "symlinked relative artifact repair",
                )?;
            }
            other => return Err(format!("expected policy denied error, got {other:?}")),
        }
        ensure(
            !outside.join("payload.bin").exists(),
            "backup relative artifact write must not follow symlinked parent",
        )
    }

    #[cfg(unix)]
    #[test]
    fn collect_lab_episode_file_dir_skips_symlinked_episode_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let episode_dir = tempdir.path().join("episodes");
        let outside = tempdir.path().join("outside-secret.json");
        fs::create_dir_all(&episode_dir).map_err(|error| error.to_string())?;
        fs::write(&outside, b"secret episode payload").map_err(|error| error.to_string())?;
        symlink(&outside, episode_dir.join("episode.json")).map_err(|error| error.to_string())?;

        let mut degraded = Vec::new();
        let mut payloads = Vec::new();
        collect_lab_episode_file_dir(
            &episode_dir,
            "workspace",
            "2026-05-25T00:00:00Z",
            &mut degraded,
            &mut payloads,
        );

        ensure(
            payloads.is_empty(),
            "symlinked lab episode files must not be included as derived backup payloads",
        )?;
        ensure(
            degraded.is_empty(),
            "skipping a non-regular lab episode directory entry should not degrade the backup",
        )
    }

    #[cfg(unix)]
    #[test]
    fn collect_lab_episode_file_dir_rejects_symlinked_directory() -> TestResult {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_episode_dir = tempdir.path().join("real-episodes");
        let linked_episode_dir = tempdir.path().join("linked-episodes");
        fs::create_dir_all(&real_episode_dir).map_err(|error| error.to_string())?;
        fs::write(real_episode_dir.join("episode.json"), b"outside payload")
            .map_err(|error| error.to_string())?;
        symlink(&real_episode_dir, &linked_episode_dir).map_err(|error| error.to_string())?;

        let mut degraded = Vec::new();
        let mut payloads = Vec::new();
        collect_lab_episode_file_dir(
            &linked_episode_dir,
            "workspace",
            "2026-05-25T00:00:00Z",
            &mut degraded,
            &mut payloads,
        );

        ensure(
            payloads.is_empty(),
            "symlinked lab episode directories must not be traversed for backup payloads",
        )?;
        ensure(
            degraded.iter().any(|degradation| {
                degradation.code == "lab_episodes_unreadable"
                    && degradation.message.contains("traverses symbolic link")
            }),
            "symlinked lab episode directory should be reported as unreadable",
        )
    }

    #[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
    #[test]
    fn open_backup_artifact_for_read_rejects_symlinked_final_path() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_artifact = tempdir.path().join("outside-records.jsonl");
        fs::write(&outside_artifact, "outside").map_err(|error| error.to_string())?;
        let artifact_path = tempdir.path().join("records.jsonl");
        std::os::unix::fs::symlink(&outside_artifact, &artifact_path)
            .map_err(|error| error.to_string())?;

        match open_backup_artifact_for_read(&artifact_path) {
            Ok(_) => Err("symlinked backup artifact final read unexpectedly succeeded".to_owned()),
            Err(error) => {
                ensure(
                    error.raw_os_error().is_some() || error.kind() == io::ErrorKind::Other,
                    "final read open returns an OS no-follow error",
                )?;
                let outside =
                    fs::read_to_string(&outside_artifact).map_err(|error| error.to_string())?;
                ensure_equal(
                    outside.as_str(),
                    "outside",
                    "backup artifact final read must not mutate symlink target",
                )
            }
        }
    }

    #[test]
    fn link_ids_remain_available_for_future_backup_richness() {
        let _ = MemoryLinkId::from_uuid(Uuid::from_u128(3)).to_string();
    }
}
